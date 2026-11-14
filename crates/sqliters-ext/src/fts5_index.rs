//! Write side of the FTS5 index: a port of `sqlite/ext/fts5/fts5_hash.c` and of the parts of
//! `sqlite/ext/fts5/fts5_index.c` that create, merge and edit segments.
//!
//! The functions keep the names, the order of operations and the decisions of the C source:
//! every statement the C module runs against `%_data` / `%_idx` / `%_config` is run here in
//! the same order with the same values, so that the shadow tables (and with them the
//! database file) come out identical. Where C keeps a sticky `p->rc`, the port returns
//! `Result` and stops at the first error.
#![forbid(unsafe_code)]

use std::collections::HashMap;

use sqliters_types::{ErrorCode, Result, ShadowHint, SqliteError, VTabHostContext, Value};

pub(crate) const FTS5_MAIN_PREFIX: u8 = b'0';
const FTS5_OPT_WORK_UNIT: i32 = 1000;
const FTS5_WORK_UNIT: i32 = 64;
const FTS5_MIN_DLIDX_SIZE: i32 = 4;
const FTS5_MAX_LEVEL: usize = 64;
const FTS5_MAX_SEGMENT: i32 = 2000;
const FTS5_MAX_PGNO: i64 = 0x7FFF_FF00;
const FTS5_DATA_PADDING: usize = 20;
pub(crate) const FTS5_AVERAGES_ROWID: i64 = 1;
pub(crate) const FTS5_STRUCTURE_ROWID: i64 = 10;
const FTS5_STRUCTURE_V2: [u8; 4] = [0xFF, 0x00, 0x00, 0x01];
pub(crate) const FTS5_CURRENT_VERSION: i32 = 4;
pub(crate) const FTS5_CURRENT_VERSION_SECUREDELETE: i32 = 5;
/// `sizeof(Fts5HashEntry)` on the 64-bit reference build: part of `nPendingData`.
const SZ_HASH_ENTRY: i64 = 48;

const FTS5_SEGITER_ONETERM: u8 = 0x01;

const FTS5_DATA_PAGE_B: u32 = 31;
const FTS5_DATA_HEIGHT_B: u32 = 5;
const FTS5_DATA_DLI_B: u32 = 1;

#[inline]
const fn fts5_dri(segid: i64, dlidx: i64, height: i64, pgno: i64) -> i64 {
    (segid << (FTS5_DATA_PAGE_B + FTS5_DATA_HEIGHT_B + FTS5_DATA_DLI_B))
        .wrapping_add(dlidx << (FTS5_DATA_PAGE_B + FTS5_DATA_HEIGHT_B))
        .wrapping_add(height << FTS5_DATA_PAGE_B)
        .wrapping_add(pgno)
}
#[inline]
pub(crate) const fn segment_rowid(segid: i32, pgno: i32) -> i64 {
    fts5_dri(segid as i64, 0, 0, pgno as i64)
}
#[inline]
const fn dlidx_rowid(segid: i32, height: i32, pgno: i32) -> i64 {
    fts5_dri(segid as i64, 1, height as i64, pgno as i64)
}
#[inline]
pub(crate) const fn tombstone_rowid(segid: i32, ipg: i32) -> i64 {
    fts5_dri(segid as i64 + (1 << 16), 0, 0, ipg as i64)
}

// ---------------------------------------------------------------------------------------
// Varints and big-endian helpers. Reads past the end of a buffer see zero bytes, which is
// what the C code sees in the zeroed padding after every blob it loads.
// ---------------------------------------------------------------------------------------

#[inline]
fn byte_at(a: &[u8], i: usize) -> u8 {
    a.get(i).copied().unwrap_or(0)
}

/// `sqlite3Fts5GetVarint`: returns (value, bytes consumed).
#[inline]
pub(crate) fn get_varint(a: &[u8], off: usize) -> (u64, usize) {
    let b0 = byte_at(a, off);
    if b0 & 0x80 == 0 {
        return (u64::from(b0), 1);
    }
    let mut v: u64 = u64::from(b0 & 0x7f);
    for i in 1..8 {
        let b = byte_at(a, off + i);
        v = (v << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return (v, i + 1);
        }
    }
    let b = byte_at(a, off + 8);
    v = (v << 8) | u64::from(b);
    (v, 9)
}

/// `fts5GetVarint32`: a varint read into a C `int` (longer forms are cut to 31 bits).
#[inline]
pub(crate) fn get_varint32(a: &[u8], off: usize) -> (i32, usize) {
    let b0 = byte_at(a, off);
    if b0 & 0x80 == 0 {
        return (i32::from(b0), 1);
    }
    let (v, n) = get_varint(a, off);
    if n <= 3 {
        (v as i32, n)
    } else {
        (((v as u32) & 0x7FFF_FFFF) as i32, n)
    }
}

/// `sqlite3Fts5PutVarint`.
#[inline]
pub(crate) fn put_varint(buf: &mut Vec<u8>, v: u64) {
    if v <= 0x7f {
        buf.push(v as u8);
        return;
    }
    if v <= 0x3fff {
        buf.push(((v >> 7) | 0x80) as u8);
        buf.push((v & 0x7f) as u8);
        return;
    }
    if v & (0xff00_0000u64 << 32) != 0 {
        let mut tmp = [0u8; 9];
        let mut v = v;
        tmp[8] = v as u8;
        v >>= 8;
        for i in (0..8).rev() {
            tmp[i] = ((v & 0x7f) | 0x80) as u8;
            v >>= 7;
        }
        buf.extend_from_slice(&tmp);
        return;
    }
    let mut tmp = [0u8; 10];
    let mut n = 0;
    let mut v = v;
    loop {
        tmp[n] = ((v & 0x7f) | 0x80) as u8;
        n += 1;
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    tmp[0] &= 0x7f;
    for i in (0..n).rev() {
        buf.push(tmp[i]);
    }
}

/// `sqlite3Fts5PutVarint` into a fixed place of a buffer; returns the bytes written.
fn put_varint_at(a: &mut [u8], off: usize, v: u64) -> usize {
    let mut tmp = Vec::with_capacity(9);
    put_varint(&mut tmp, v);
    for (i, b) in tmp.iter().enumerate() {
        if let Some(slot) = a.get_mut(off + i) {
            *slot = *b;
        }
    }
    tmp.len()
}

/// `sqlite3Fts5GetVarintLen`.
#[inline]
fn varint_len32(v: u32) -> usize {
    if v < (1 << 7) {
        1
    } else if v < (1 << 14) {
        2
    } else if v < (1 << 21) {
        3
    } else if v < (1 << 28) {
        4
    } else {
        5
    }
}

#[inline]
fn get_u16(a: &[u8], off: usize) -> i32 {
    (i32::from(byte_at(a, off)) << 8) | i32::from(byte_at(a, off + 1))
}
#[inline]
fn put_u16(a: &mut [u8], off: usize, v: u16) {
    if off + 1 < a.len() {
        a[off] = (v >> 8) as u8;
        a[off + 1] = v as u8;
    }
}
#[inline]
fn get_u32(a: &[u8], off: usize) -> u32 {
    (u32::from(byte_at(a, off)) << 24)
        | (u32::from(byte_at(a, off + 1)) << 16)
        | (u32::from(byte_at(a, off + 2)) << 8)
        | u32::from(byte_at(a, off + 3))
}
#[inline]
fn put_u32(a: &mut [u8], off: usize, v: u32) {
    if off + 3 < a.len() {
        a[off..off + 4].copy_from_slice(&v.to_be_bytes());
    }
}
#[inline]
fn get_u64(a: &[u8], off: usize) -> u64 {
    (u64::from(get_u32(a, off)) << 32) | u64::from(get_u32(a, off + 4))
}
#[inline]
fn put_u64(a: &mut [u8], off: usize, v: u64) {
    if off + 7 < a.len() {
        a[off..off + 8].copy_from_slice(&v.to_be_bytes());
    }
}

/// A cheap content hash for the decision trace (FNV-1a).
#[allow(dead_code)]
pub(crate) fn trace_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// `FTS5_CORRUPT` = `SQLITE_CORRUPT_VTAB`: the extended result code of every corruption the
/// module itself detects (`sqlite3_extended_errcode()`); the message is that of SQLITE_CORRUPT.
pub(crate) const FTS5_CORRUPT: i32 = 11 | (1 << 8);

/// A statement the module steps on a shadow table hands back its result code only
/// (`p->rc = sqlite3_reset(pStmt)`): the message of the failed statement is not kept, the
/// statement that called the module fails with `sqlite3ErrStr(rc)` ("constraint failed", ...).
#[cold]
pub(crate) fn rc_only(mut e: SqliteError) -> SqliteError {
    e.message = e.code.as_str().to_owned();
    e
}

fn corrupt() -> SqliteError {
    SqliteError::with_extended(FTS5_CORRUPT, "database disk image is malformed")
}

// ---------------------------------------------------------------------------------------
// fts5_hash.c
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Detail {
    Full,
    None,
    Columns,
}

#[derive(Debug, Default)]
struct HashEntry {
    /// The doclist under construction (what follows the key in `Fts5HashEntry`).
    data: Vec<u8>,
    /// `iSzPoslist` as an offset into `data`; `None` when the current position list has
    /// already received its size field.
    i_sz_poslist: Option<usize>,
    b_del: bool,
    b_content: bool,
    i_col: i16,
    i_pos: i32,
    i_rowid: i64,
}

impl HashEntry {
    /// `Fts5HashEntry.nData`.
    #[inline]
    fn n_data(&self, n_key: usize) -> i64 {
        SZ_HASH_ENTRY + n_key as i64 + self.data.len() as i64
    }
}

/// `fts5HashAddPoslistSize(pHash, p, 0)`: writes the size field of the current position
/// list in place.
fn hash_add_poslist_size(detail: Detail, p: &mut HashEntry) {
    if let Some(isz) = p.i_sz_poslist {
        if detail == Detail::None {
            if p.b_del {
                p.data.push(0);
                if p.b_content {
                    p.data.push(0);
                }
            }
        } else {
            let n_sz = p.data.len() - isz - 1;
            let n_pos = n_sz * 2 + usize::from(p.b_del);
            if n_pos <= 127 {
                p.data[isz] = n_pos as u8;
            } else {
                let mut v = Vec::with_capacity(5);
                put_varint(&mut v, n_pos as u64);
                debug_assert_eq!(v.len(), varint_len32(n_pos as u32));
                p.data.splice(isz..=isz, v);
            }
        }
        p.i_sz_poslist = None;
        p.b_del = false;
        p.b_content = false;
    }
}

/// `fts5HashAddPoslistSize(pHash, p, pFaux)`: the finished doclist as a copy; the entry is
/// left as it is (`sqlite3Fts5HashQuery`).
fn hash_doclist_copy(detail: Detail, p: &HashEntry) -> Vec<u8> {
    let mut out = p.data.clone();
    if let Some(isz) = p.i_sz_poslist {
        if detail == Detail::None {
            if p.b_del {
                out.push(0);
                if p.b_content {
                    out.push(0);
                }
            }
        } else {
            let n_sz = out.len() - isz - 1;
            let n_pos = n_sz * 2 + usize::from(p.b_del);
            if n_pos <= 127 {
                out[isz] = n_pos as u8;
            } else {
                let mut v = Vec::with_capacity(5);
                put_varint(&mut v, n_pos as u64);
                out.splice(isz..=isz, v);
            }
        }
    }
    out
}

#[derive(Debug)]
pub(crate) struct Fts5Hash {
    detail: Detail,
    map: HashMap<Vec<u8>, HashEntry>,
    key_buf: Vec<u8>,
}

impl Fts5Hash {
    fn new(detail: Detail) -> Self {
        Self {
            detail,
            map: HashMap::new(),
            key_buf: Vec::new(),
        }
    }

    /// `sqlite3Fts5HashWrite`; returns the change of `*pHash->pnByte`.
    fn write(&mut self, rowid: i64, i_col: i32, i_pos: i32, b_byte: u8, token: &[u8]) -> i64 {
        let detail = self.detail;
        let mut i_pos = i_pos;
        let mut b_new = detail == Detail::Full;
        let mut n_incr: i64 = 0;
        self.key_buf.clear();
        self.key_buf.push(b_byte);
        self.key_buf.extend_from_slice(token);
        let n_key = self.key_buf.len();
        if !self.map.contains_key(self.key_buf.as_slice()) {
            let mut e = HashEntry::default();
            put_varint(&mut e.data, rowid as u64);
            e.i_rowid = rowid;
            e.i_sz_poslist = Some(e.data.len());
            if detail != Detail::None {
                e.data.push(0);
                e.i_col = if detail == Detail::Full { 0 } else { -1 };
            }
            self.map.insert(self.key_buf.clone(), e);
        } else if let Some(p) = self.map.get(self.key_buf.as_slice()) {
            n_incr -= p.n_data(n_key);
        }
        let Some(p) = self.map.get_mut(self.key_buf.as_slice()) else {
            return 0;
        };

        // If this is a new rowid, append the size field for the previous entry, and the
        // new rowid for this entry.
        if rowid != p.i_rowid {
            let diff = (rowid as u64).wrapping_sub(p.i_rowid as u64);
            hash_add_poslist_size(detail, p);
            put_varint(&mut p.data, diff);
            p.i_rowid = rowid;
            b_new = true;
            p.i_sz_poslist = Some(p.data.len());
            if detail != Detail::None {
                p.data.push(0);
                p.i_col = if detail == Detail::Full { 0 } else { -1 };
                p.i_pos = 0;
            }
        }

        if i_col >= 0 {
            if detail == Detail::None {
                p.b_content = true;
            } else {
                // Append a new column value, if necessary
                if i_col != i32::from(p.i_col) {
                    if detail == Detail::Full {
                        p.data.push(0x01);
                        put_varint(&mut p.data, i_col as u64);
                        p.i_col = i_col as i16;
                        p.i_pos = 0;
                    } else {
                        b_new = true;
                        i_pos = i_col;
                        p.i_col = i_col as i16;
                    }
                }
                // Append the new position offset, if necessary
                if b_new {
                    put_varint(
                        &mut p.data,
                        i64::from(i_pos.wrapping_sub(p.i_pos).wrapping_add(2)) as u64,
                    );
                    p.i_pos = i_pos;
                }
            }
        } else {
            // This is a delete. Set the delete flag.
            p.b_del = true;
        }
        n_incr += p.n_data(n_key);
        n_incr
    }

    /// `sqlite3Fts5HashQuery`: the doclist of one key (index byte + term), if present.
    pub(crate) fn query(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.map.get(key).map(|p| hash_doclist_copy(self.detail, p))
    }

    /// The whole table for `fts5FlushOneHash`: every entry in key order with its doclist
    /// finished (`sqlite3Fts5HashScanInit(pHash, 0, 0)` + `sqlite3Fts5HashScanEntry`). The
    /// entries are moved out; [`Self::restore`] puts them back if the flush fails.
    fn take_sorted(&mut self) -> Vec<(Vec<u8>, HashEntry)> {
        let detail = self.detail;
        let mut out: Vec<(Vec<u8>, HashEntry)> =
            std::mem::take(&mut self.map).into_iter().collect();
        for (_, p) in &mut out {
            hash_add_poslist_size(detail, p);
        }
        out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn restore(&mut self, entries: Vec<(Vec<u8>, HashEntry)>) {
        self.map = entries.into_iter().collect();
    }

    /// The entries whose key starts with `prefix`, in key order, with finished doclists;
    /// the table is left untouched.
    pub(crate) fn scan_copy(&self, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out: Vec<(Vec<u8>, Vec<u8>)> = self
            .map
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, p)| (k.clone(), hash_doclist_copy(self.detail, p)))
            .collect();
        out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The side effect of a scan (`sqlite3Fts5HashScanInit` + `sqlite3Fts5HashScanEntry` on
    /// every entry of the scan): the doclists of the entries whose key starts with `prefix`
    /// get the size field of their last position list.
    pub(crate) fn scan_touch(&mut self, prefix: &[u8]) {
        let detail = self.detail;
        for (k, p) in &mut self.map {
            if k.starts_with(prefix) {
                hash_add_poslist_size(detail, p);
            }
        }
    }

    fn clear(&mut self) {
        // (`sqlite3Fts5HashClear` frees every entry; the slot array is kept.)
        self.map = HashMap::new();
    }
}

// ---------------------------------------------------------------------------------------
// Structure record
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StructureSegment {
    pub(crate) i_segid: i32,
    pub(crate) pgno_first: i32,
    pub(crate) pgno_last: i32,
    pub(crate) i_origin1: u64,
    pub(crate) i_origin2: u64,
    pub(crate) n_pg_tombstone: i32,
    pub(crate) n_entry_tombstone: u64,
    pub(crate) n_entry: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StructureLevel {
    pub(crate) n_merge: i32,
    pub(crate) a_seg: Vec<StructureSegment>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Structure {
    pub(crate) n_write_counter: u64,
    pub(crate) n_origin_cntr: u64,
    pub(crate) n_segment: i32,
    pub(crate) a_level: Vec<StructureLevel>,
}

/// `fts5StructureDecode`: (cookie, structure).
pub(crate) fn structure_decode(data: &[u8]) -> Result<(i32, Structure)> {
    let n_data = data.len();
    let cookie = get_u32(data, 0) as i32;
    let mut i = 4usize;
    let mut v2 = false;
    if n_data >= 8 && data[4..8] == FTS5_STRUCTURE_V2 {
        i += 4;
        v2 = true;
    }
    let (n_level, n) = get_varint32(data, i);
    i += n;
    let (n_segment_total, n) = get_varint32(data, i);
    i += n;
    if !(0..=FTS5_MAX_SEGMENT).contains(&n_level)
        || !(0..=FTS5_MAX_SEGMENT).contains(&n_segment_total)
    {
        return Err(corrupt());
    }
    let mut ret = Structure {
        n_segment: n_segment_total,
        ..Structure::default()
    };
    let (wc, n) = get_varint(data, i);
    i += n;
    ret.n_write_counter = wc;
    let mut n_segment = n_segment_total;
    let mut n_origin_cntr: u64 = 0;
    for i_lvl in 0..n_level as usize {
        if i >= n_data {
            return Err(corrupt());
        }
        let (n_merge, n) = get_varint32(data, i);
        i += n;
        let (n_total, n) = get_varint32(data, i);
        i += n;
        if n_total < n_merge {
            return Err(corrupt());
        }
        if n_total < 0 {
            return Err(corrupt());
        }
        n_segment -= n_total;
        let mut lvl = StructureLevel {
            n_merge,
            a_seg: Vec::with_capacity(n_total as usize),
        };
        for _ in 0..n_total {
            if i >= n_data {
                return Err(corrupt());
            }
            let mut seg = StructureSegment::default();
            let (v, n) = get_varint32(data, i);
            i += n;
            seg.i_segid = v;
            let (v, n) = get_varint32(data, i);
            i += n;
            seg.pgno_first = v;
            let (v, n) = get_varint32(data, i);
            i += n;
            seg.pgno_last = v;
            if v2 {
                let (v, n) = get_varint(data, i);
                i += n;
                seg.i_origin1 = v;
                let (v, n) = get_varint(data, i);
                i += n;
                seg.i_origin2 = v;
                let (v, n) = get_varint32(data, i);
                i += n;
                seg.n_pg_tombstone = v;
                let (v, n) = get_varint(data, i);
                i += n;
                seg.n_entry_tombstone = v;
                let (v, n) = get_varint(data, i);
                i += n;
                seg.n_entry = v;
                n_origin_cntr = n_origin_cntr.max(seg.i_origin2);
            }
            if seg.pgno_last < seg.pgno_first || i64::from(seg.pgno_last) > FTS5_MAX_PGNO {
                return Err(corrupt());
            }
            lvl.a_seg.push(seg);
        }
        if i_lvl > 0 && ret.a_level[i_lvl - 1].n_merge != 0 && n_total == 0 {
            return Err(corrupt());
        }
        if i_lvl == n_level as usize - 1 && lvl.n_merge != 0 {
            return Err(corrupt());
        }
        ret.a_level.push(lvl);
    }
    if n_segment != 0 {
        return Err(corrupt());
    }
    if v2 {
        ret.n_origin_cntr = n_origin_cntr + 1;
    }
    Ok((cookie, ret))
}

/// The serialised structure record (`fts5StructureWrite` without the write).
pub(crate) fn structure_encode(cookie: i32, st: &Structure) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    let cookie = cookie.max(0);
    buf.extend_from_slice(&(cookie as u32).to_be_bytes());
    if st.n_origin_cntr > 0 {
        buf.extend_from_slice(&FTS5_STRUCTURE_V2);
    }
    put_varint(&mut buf, st.a_level.len() as u64);
    put_varint(&mut buf, st.n_segment as i64 as u64);
    put_varint(&mut buf, st.n_write_counter);
    for lvl in &st.a_level {
        put_varint(&mut buf, lvl.n_merge as i64 as u64);
        put_varint(&mut buf, lvl.a_seg.len() as u64);
        for seg in &lvl.a_seg {
            put_varint(&mut buf, seg.i_segid as i64 as u64);
            put_varint(&mut buf, seg.pgno_first as i64 as u64);
            put_varint(&mut buf, seg.pgno_last as i64 as u64);
            if st.n_origin_cntr > 0 {
                put_varint(&mut buf, seg.i_origin1);
                put_varint(&mut buf, seg.i_origin2);
                put_varint(&mut buf, seg.n_pg_tombstone as i64 as u64);
                put_varint(&mut buf, seg.n_entry_tombstone);
                put_varint(&mut buf, seg.n_entry);
            }
        }
    }
    buf
}

/// `fts5SegmentSize`.
#[inline]
fn segment_size(seg: &StructureSegment) -> i32 {
    1 + seg.pgno_last - seg.pgno_first
}

// ---------------------------------------------------------------------------------------
// Fts5Data
// ---------------------------------------------------------------------------------------

/// A blob of `%_data` (`Fts5Data`): `p` holds `nn` bytes followed by zero padding.
#[derive(Debug, Clone, Default)]
struct Data {
    p: Vec<u8>,
    nn: i32,
    sz_leaf: i32,
}

impl Data {
    fn from_blob(mut blob: Vec<u8>) -> Self {
        let nn = blob.len() as i32;
        blob.resize(blob.len() + FTS5_DATA_PADDING, 0);
        Self {
            p: blob,
            nn,
            sz_leaf: 0,
        }
    }
    #[inline]
    fn is_termless(&self) -> bool {
        self.sz_leaf >= self.nn
    }
    #[inline]
    fn first_rowid_off(&self) -> i32 {
        get_u16(&self.p, 0)
    }
    /// `fts5LeafFirstTermOff`.
    #[inline]
    fn first_term_off(&self) -> i32 {
        get_varint32(&self.p, self.sz_leaf as usize).0
    }
}

// ---------------------------------------------------------------------------------------
// Configuration values the index needs (`Fts5Config`)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct IndexConfig {
    pub(crate) detail: Detail,
    /// `aPrefix[]`: the prefix index sizes in characters.
    pub(crate) prefixes: Vec<usize>,
    pub(crate) contentless_delete: bool,
    pub(crate) n_col: usize,
    // Values of %_config (`sqlite3Fts5ConfigLoad`); `pgsz == 0` means "not loaded".
    pub(crate) pgsz: i32,
    pub(crate) n_automerge: i32,
    pub(crate) n_usermerge: i32,
    pub(crate) n_crisis_merge: i32,
    pub(crate) n_hash_size: i32,
    pub(crate) n_delete_merge: i32,
    pub(crate) secure_delete: bool,
    pub(crate) i_version: i32,
    pub(crate) i_cookie: i32,
    /// `zRank` / `zRankArgs` as the text of the `rank` row (None = default).
    pub(crate) rank: Option<String>,
}

impl IndexConfig {
    pub(crate) fn new(
        detail: Detail,
        prefixes: Vec<usize>,
        contentless_delete: bool,
        n_col: usize,
    ) -> Self {
        Self {
            detail,
            prefixes,
            contentless_delete,
            n_col,
            pgsz: 0,
            n_automerge: 4,
            n_usermerge: 4,
            n_crisis_merge: 16,
            n_hash_size: 1024 * 1024,
            n_delete_merge: 10,
            secure_delete: false,
            i_version: 0,
            i_cookie: 0,
            rank: None,
        }
    }

    /// The "Set default values" block of `sqlite3Fts5ConfigLoad`.
    pub(crate) fn set_defaults(&mut self) {
        self.pgsz = 4050;
        self.n_automerge = 4;
        self.n_usermerge = 4;
        self.n_crisis_merge = 16;
        self.n_hash_size = 1024 * 1024;
        self.n_delete_merge = 10;
    }
}

// ---------------------------------------------------------------------------------------
// Segment writer
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct PageWriter {
    pgno: i32,
    i_prev_pgidx: i32,
    buf: Vec<u8>,
    pgidx: Vec<u8>,
    term: Vec<u8>,
}

#[derive(Debug, Default)]
struct DlidxWriter {
    pgno: i32,
    b_prev_valid: bool,
    i_prev: i64,
    buf: Vec<u8>,
}

#[derive(Debug, Default)]
struct SegWriter {
    i_segid: i32,
    writer: PageWriter,
    i_prev_rowid: i64,
    b_first_rowid_in_doclist: bool,
    b_first_rowid_in_page: bool,
    b_first_term_in_page: bool,
    n_leaf_written: i32,
    n_empty: i32,
    a_dlidx: Vec<DlidxWriter>,
    btterm: Vec<u8>,
    i_bt_page: i32,
}

// ---------------------------------------------------------------------------------------
// Doclist-index iterator (forward only)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct DlidxLvl {
    data: Option<Data>,
    i_off: i32,
    b_eof: bool,
    i_leaf_pgno: i32,
    i_rowid: i64,
}

#[derive(Debug, Default)]
struct DlidxIter {
    i_segid: i32,
    a_lvl: Vec<DlidxLvl>,
}

/// `fts5DlidxLvlNext`.
fn dlidx_lvl_next(lvl: &mut DlidxLvl) -> bool {
    let Some(data) = lvl.data.as_ref() else {
        lvl.b_eof = true;
        return true;
    };
    if lvl.i_off == 0 {
        lvl.i_off = 1;
        let (pg, n) = get_varint32(&data.p, 1);
        lvl.i_leaf_pgno = pg;
        lvl.i_off += n as i32;
        let (r, n) = get_varint(&data.p, lvl.i_off as usize);
        lvl.i_rowid = r as i64;
        lvl.i_off += n as i32;
    } else {
        let mut i_off = lvl.i_off;
        while i_off < data.nn {
            if byte_at(&data.p, i_off as usize) != 0 {
                break;
            }
            i_off += 1;
        }
        if i_off < data.nn {
            lvl.i_leaf_pgno += (i_off - lvl.i_off) + 1;
            let (v, n) = get_varint(&data.p, i_off as usize);
            i_off += n as i32;
            lvl.i_rowid = lvl.i_rowid.wrapping_add(v as i64);
            lvl.i_off = i_off;
        } else {
            lvl.b_eof = true;
        }
    }
    lvl.b_eof
}

// ---------------------------------------------------------------------------------------
// Segment iterator / multi-segment iterator (forward only, segments only)
// ---------------------------------------------------------------------------------------

#[derive(Debug, Default)]
struct TombstoneArray {
    n_tombstone: i32,
    ap: Vec<Option<Data>>,
}

#[derive(Debug, Default)]
struct SegIter {
    /// Position of the segment in the structure (level, index) and a copy of its record.
    seg_pos: Option<(usize, usize)>,
    seg: StructureSegment,
    flags: u8,
    i_leaf_pgno: i32,
    leaf: Option<Data>,
    next_leaf: Option<Data>,
    i_leaf_offset: i64,
    tomb: Option<TombstoneArray>,
    i_term_leaf_pgno: i32,
    i_term_leaf_offset: i32,
    i_pgidx_off: i32,
    i_endof_doclist: i32,
    dlidx: Option<DlidxIter>,
    term: Vec<u8>,
    i_rowid: i64,
    n_pos: i32,
    b_del: bool,
    /// `pSeg==0`: the iterator reads the pending-terms hash (`fts5SegIterHashInit` with a
    /// scan); the entries of the scan in key order and the position of the current one.
    hash_entries: Option<Vec<(Vec<u8>, Vec<u8>)>>,
    hash_pos: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct CResult {
    i_first: u16,
    b_term_eq: bool,
}

#[derive(Debug, Default)]
struct MultiIter {
    n_seg: usize,
    a_seg: Vec<SegIter>,
    a_first: Vec<CResult>,
    b_skip_empty: bool,
    b_eof: bool,
    i_switch_rowid: i64,
}

impl MultiIter {
    /// `fts5MultiIterAlloc`.
    fn alloc(n_seg: usize) -> Self {
        let mut n_slot = 2usize;
        while n_slot < n_seg {
            n_slot *= 2;
        }
        let mut a_seg = Vec::with_capacity(n_slot);
        a_seg.resize_with(n_slot, SegIter::default);
        Self {
            n_seg: n_slot,
            a_seg,
            a_first: vec![CResult::default(); n_slot],
            b_skip_empty: false,
            b_eof: false,
            i_switch_rowid: 0,
        }
    }
    #[inline]
    fn first(&self) -> usize {
        usize::from(self.a_first[1].i_first)
    }
    /// `fts5MultiIterDoCompare`.
    fn do_compare(&mut self, i_out: usize) -> usize {
        let (i1, i2) = if i_out >= self.n_seg / 2 {
            let i1 = (i_out - self.n_seg / 2) * 2;
            (i1, i1 + 1)
        } else {
            (
                usize::from(self.a_first[i_out * 2].i_first),
                usize::from(self.a_first[i_out * 2 + 1].i_first),
            )
        };
        self.a_first[i_out].b_term_eq = false;
        let i_res;
        let p1 = &self.a_seg[i1];
        let p2 = &self.a_seg[i2];
        if p1.leaf.is_none() {
            i_res = i2;
        } else if p2.leaf.is_none() {
            i_res = i1;
        } else {
            let mut res = p1.term.as_slice().cmp(p2.term.as_slice());
            if res == std::cmp::Ordering::Equal {
                self.a_first[i_out].b_term_eq = true;
                if p1.i_rowid == p2.i_rowid {
                    return i2;
                }
                res = if p1.i_rowid > p2.i_rowid {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Less
                };
            }
            i_res = if res == std::cmp::Ordering::Less {
                i1
            } else {
                i2
            };
        }
        self.a_first[i_out].i_first = i_res as u16;
        0
    }
    /// `fts5MultiIterSetEof`.
    fn set_eof(&mut self) {
        let seg = &self.a_seg[self.first()];
        self.b_eof = seg.leaf.is_none();
        self.i_switch_rowid = seg.i_rowid;
    }
    /// `fts5MultiIterAdvanceRowid` (forward iteration); returns (collision, new first).
    fn advance_rowid(&mut self, i_changed: usize) -> (bool, usize) {
        let mut i_new = i_changed;
        let new_rowid = self.a_seg[i_new].i_rowid;
        if new_rowid == self.i_switch_rowid || new_rowid >= self.i_switch_rowid {
            let mut i_other = i_changed ^ 1;
            self.i_switch_rowid = i64::MAX;
            let mut i = (self.n_seg + i_changed) / 2;
            loop {
                if self.a_first[i].b_term_eq {
                    let rn = self.a_seg[i_new].i_rowid;
                    let ro = self.a_seg[i_other].i_rowid;
                    if rn == ro {
                        return (true, i_new);
                    } else if ro < rn {
                        self.i_switch_rowid = ro;
                        i_new = i_other;
                    } else if ro < self.i_switch_rowid {
                        self.i_switch_rowid = ro;
                    }
                }
                self.a_first[i].i_first = i_new as u16;
                if i == 1 {
                    break;
                }
                i_other = usize::from(self.a_first[i ^ 1].i_first);
                i /= 2;
            }
        }
        (false, i_new)
    }
}

/// `fts5IndexTombstoneQuery`.
fn tombstone_query(hash: &Data, n_hash_table: i32, i_rowid: u64) -> bool {
    let sz_key: usize = if byte_at(&hash.p, 0) == 4 { 4 } else { 8 };
    let n_slot: i64 = if hash.nn > 16 {
        (i64::from(hash.nn) - 8) / sz_key as i64
    } else {
        1
    };
    let mut i_slot = ((i_rowid / n_hash_table.max(1) as u64) % n_slot as u64) as i64;
    let mut n_collide = n_slot;
    if i_rowid == 0 {
        return byte_at(&hash.p, 1) != 0;
    }
    loop {
        let off = 8 + i_slot as usize * sz_key;
        let v = if sz_key == 4 {
            u64::from(get_u32(&hash.p, off))
        } else {
            get_u64(&hash.p, off)
        };
        if v == 0 {
            break;
        }
        if v == i_rowid {
            return true;
        }
        if n_collide == 0 {
            break;
        }
        n_collide -= 1;
        i_slot = (i_slot + 1) % n_slot;
    }
    false
}

/// `fts5IndexTombstoneAddToPage`.
fn tombstone_add_to_page(pg: &mut Data, b_force: bool, n_pg: i32, i_rowid: u64) -> i32 {
    let sz_key: usize = if byte_at(&pg.p, 0) == 4 { 4 } else { 8 };
    let n_slot: i64 = if pg.nn > 16 {
        (i64::from(pg.nn) - 8) / sz_key as i64
    } else {
        1
    };
    let n_elem = get_u32(&pg.p, 4) as i32;
    let mut i_slot = ((i_rowid / n_pg.max(1) as u64) % n_slot as u64) as i64;
    let mut n_collide = n_slot;
    if sz_key == 4 && i_rowid > 0xFFFF_FFFF {
        return 2;
    }
    if i_rowid == 0 {
        if pg.p.len() > 1 {
            pg.p[1] = 0x01;
        }
        return 0;
    }
    if !b_force && i64::from(n_elem) >= n_slot / 2 {
        return 1;
    }
    put_u32(&mut pg.p, 4, (n_elem + 1) as u32);
    loop {
        let off = 8 + i_slot as usize * sz_key;
        let v = if sz_key == 4 {
            u64::from(get_u32(&pg.p, off))
        } else {
            get_u64(&pg.p, off)
        };
        if v == 0 {
            break;
        }
        i_slot = (i_slot + 1) % n_slot;
        if n_collide == 0 {
            return 0;
        }
        n_collide -= 1;
    }
    let off = 8 + i_slot as usize * sz_key;
    if sz_key == 4 {
        put_u32(&mut pg.p, off, i_rowid as u32);
    } else {
        put_u64(&mut pg.p, off, i_rowid);
    }
    0
}

/// `fts5PoslistPrefix`.
fn poslist_prefix(a: &[u8], n_max: i32) -> i32 {
    let mut ret = get_varint32(a, 0).1 as i32;
    if ret < n_max {
        loop {
            let i = get_varint32(a, ret as usize).1 as i32;
            if ret + i > n_max {
                break;
            }
            ret += i;
        }
    }
    ret
}

/// `fts5PrefixCompress`.
#[inline]
fn prefix_compress(n_old: usize, old: &[u8], new: &[u8]) -> usize {
    let mut i = 0;
    while i < n_old {
        if old[i] != new[i] {
            break;
        }
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------------------
// Fts5Index
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) struct Fts5Index {
    pub(crate) config: IndexConfig,
    /// The virtual table name (`pConfig->zName`).
    pub(crate) name: String,
    data_tbl: String,
    idx_tbl: String,
    config_tbl: String,
    data_hint: ShadowHint,
    n_work_unit: i32,
    pub(crate) hash: Option<Fts5Hash>,
    pub(crate) n_pending_data: i64,
    pub(crate) n_pending_row: i32,
    i_write_rowid: i64,
    pub(crate) b_delete: bool,
    pub(crate) n_contentless_delete: i32,
    flush_rc: Option<SqliteError>,
    /// `p->pStruct`: the cached structure record.
    p_struct: Option<Structure>,
    /// Counts the writes to `%_data` / `%_idx`; readers that cache pages compare it.
    pub(crate) write_gen: u64,
    /// The `%_idx` rows of segments looked up during the current flush (`term`, `pgno`
    /// value, in term order): `fts5SegIterSeekInit` runs `SELECT pgno FROM %_idx WHERE
    /// segid=? AND term<=? ORDER BY term DESC LIMIT 1` once per segment for every entry a
    /// secure delete removes; the answers come from here instead of one index seek each.
    /// Only filled while `fts5FlushOneHash` runs, and kept in step with the rows it writes.
    idx_cache: HashMap<i32, Vec<(Vec<u8>, i64)>>,
    idx_cache_on: bool,
    sql_idx_scan: String,
    // SQL texts of the statements of `Fts5Index` (prepared lazily in C).
    sql_data_delete: String,
    sql_idx_delete: String,
    sql_idx_insert: String,
    sql_idx_select: String,
    sql_idx_secure_delete: String,
    sql_config_version: String,
    sql_config_select: String,
    db_name: String,
}

fn q_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

impl Fts5Index {
    pub(crate) fn new(db_name: &str, tbl_name: &str, config: IndexConfig) -> Self {
        let mut ix = Self {
            config,
            name: String::new(),
            data_tbl: String::new(),
            idx_tbl: String::new(),
            config_tbl: String::new(),
            data_hint: ShadowHint::NONE,
            n_work_unit: FTS5_WORK_UNIT,
            hash: None,
            n_pending_data: 0,
            n_pending_row: 0,
            i_write_rowid: 0,
            b_delete: false,
            n_contentless_delete: 0,
            flush_rc: None,
            p_struct: None,
            write_gen: 0,
            idx_cache: HashMap::new(),
            idx_cache_on: false,
            sql_idx_scan: String::new(),
            sql_data_delete: String::new(),
            sql_idx_delete: String::new(),
            sql_idx_insert: String::new(),
            sql_idx_select: String::new(),
            sql_idx_secure_delete: String::new(),
            sql_config_version: String::new(),
            sql_config_select: String::new(),
            db_name: String::new(),
        };
        ix.set_names(db_name, tbl_name);
        ix
    }

    /// (Re)computes the shadow table names and statement texts (`xRename`).
    pub(crate) fn set_names(&mut self, db_name: &str, tbl_name: &str) {
        self.name = tbl_name.to_owned();
        self.db_name = db_name.to_owned();
        self.data_tbl = format!("{tbl_name}_data");
        self.idx_tbl = format!("{tbl_name}_idx");
        self.config_tbl = format!("{tbl_name}_config");
        self.data_hint = ShadowHint::NONE;
        let db_q = q_ident(db_name);
        let data_q = q_ident(&self.data_tbl);
        let idx_q = q_ident(&self.idx_tbl);
        let config_q = q_ident(&self.config_tbl);
        self.sql_data_delete = format!("DELETE FROM {db_q}.{data_q} WHERE id>=? AND id<=?");
        self.sql_idx_delete = format!("DELETE FROM {db_q}.{idx_q} WHERE segid=?");
        self.sql_idx_insert = format!("INSERT INTO {db_q}.{idx_q}(segid,term,pgno) VALUES(?,?,?)");
        self.sql_idx_select = format!(
            "SELECT pgno FROM {db_q}.{idx_q} WHERE segid=? AND term<=? ORDER BY term DESC LIMIT 1"
        );
        self.sql_idx_scan =
            format!("SELECT term, pgno FROM {db_q}.{idx_q} WHERE segid=? ORDER BY term");
        self.idx_cache.clear();
        self.sql_idx_secure_delete =
            format!("DELETE FROM {db_q}.{idx_q} WHERE (segid, (pgno/2)) = (?1, ?2)");
        self.sql_config_version = format!(
            "REPLACE INTO {db_q}.{config_q} VALUES ('version', {FTS5_CURRENT_VERSION_SECUREDELETE})"
        );
        self.sql_config_select = format!("SELECT k, v FROM {db_q}.{config_q}");
    }

    fn corrupt_rowid(&self, i_rowid: i64) -> SqliteError {
        SqliteError::with_extended(
            FTS5_CORRUPT,
            format!(
                "fts5: corruption found reading blob {i_rowid} from table \"{}\"",
                self.name
            ),
        )
    }
    fn corrupt_iter(&self, it: &SegIter) -> SqliteError {
        SqliteError::with_extended(
            FTS5_CORRUPT,
            format!(
                "fts5: corruption on page {}, segment {}, table \"{}\"",
                it.i_leaf_pgno, it.seg.i_segid, self.name
            ),
        )
    }
    fn corrupt_idx(&self) -> SqliteError {
        SqliteError::with_extended(
            FTS5_CORRUPT,
            format!("fts5: corruption in table \"{}\"", self.name),
        )
    }

    // -----------------------------------------------------------------------------------
    // %_data access
    // -----------------------------------------------------------------------------------

    /// `fts5DataRead`.
    fn data_read(&mut self, ctx: &mut dyn VTabHostContext, i_rowid: i64) -> Result<Data> {
        let row = ctx.shadow_get_rowid(&self.data_tbl, i_rowid)?;
        match row.and_then(|mut r| {
            if r.len() >= 2 {
                Some(r.swap_remove(1))
            } else {
                None
            }
        }) {
            Some(Value::Blob(b)) => Ok(Data::from_blob(b)),
            Some(Value::Text(s)) => Ok(Data::from_blob(s.into_bytes())),
            _ => Err(self.corrupt_rowid(i_rowid)),
        }
    }

    /// `fts5LeafRead`.
    fn leaf_read(&mut self, ctx: &mut dyn VTabHostContext, i_rowid: i64) -> Result<Data> {
        let mut d = self.data_read(ctx, i_rowid)?;
        if d.nn >= 4 {
            d.sz_leaf = get_u16(&d.p, 2);
        }
        if d.sz_leaf < 4 || d.sz_leaf > d.nn {
            return Err(self.corrupt_rowid(i_rowid));
        }
        Ok(d)
    }

    /// `fts5DataWrite`: `REPLACE INTO %_data(id, block) VALUES(?,?)`.
    fn data_write(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_rowid: i64,
        data: &[u8],
    ) -> Result<()> {
        sqliters_types::dtrace!(
            "TR|f5|w|tbl=data k={} n={} h={:016x}",
            i_rowid,
            data.len(),
            trace_hash(data)
        );
        self.write_gen = self.write_gen.wrapping_add(1);
        ctx.shadow_replace_blob(&self.data_tbl, &mut self.data_hint, Some(i_rowid), data)
            .map_err(rc_only)?;
        Ok(())
    }

    /// `fts5DataDelete`: `DELETE FROM %_data WHERE id>=? AND id<=?`.
    fn data_delete(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_first: i64,
        i_last: i64,
    ) -> Result<()> {
        sqliters_types::dtrace!("TR|f5|d|tbl=data k1={} k2={}", i_first, i_last);
        self.write_gen = self.write_gen.wrapping_add(1);
        ctx.execute_sql_counted(
            &self.sql_data_delete,
            &[Value::Integer(i_first), Value::Integer(i_last)],
        )
        .map_err(rc_only)?;
        Ok(())
    }

    /// `fts5DataRemoveSegment`.
    fn data_remove_segment(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &StructureSegment,
    ) -> Result<()> {
        let i_segid = seg.i_segid;
        let i_first = segment_rowid(i_segid, 0);
        let i_last = segment_rowid(i_segid + 1, 0) - 1;
        self.data_delete(ctx, i_first, i_last)?;
        if seg.n_pg_tombstone != 0 {
            let t1 = tombstone_rowid(i_segid, 0);
            let t2 = tombstone_rowid(i_segid, seg.n_pg_tombstone - 1);
            self.data_delete(ctx, t1, t2)?;
        }
        sqliters_types::dtrace!("TR|f5|d|tbl=idx segid={}", i_segid);
        self.idx_cache.remove(&i_segid);
        ctx.execute_sql_counted(&self.sql_idx_delete, &[Value::Integer(i64::from(i_segid))])
            .map_err(rc_only)?;
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Structure record
    // -----------------------------------------------------------------------------------

    /// `sqlite3Fts5ConfigLoad`.
    pub(crate) fn config_load(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_cookie: i32,
    ) -> Result<()> {
        self.config.set_defaults();
        let rows = ctx.query_sql(&self.sql_config_select, &[])?;
        let mut i_version = 0i32;
        for r in &rows {
            let (Some(k), Some(v)) = (r.first(), r.get(1)) else {
                continue;
            };
            let zk = k.to_text_lossy();
            if zk.eq_ignore_ascii_case("version") {
                i_version = v.to_integer_lossy() as i32;
            } else {
                let _ = config_set_value(&mut self.config, &zk, v);
            }
        }
        if i_version != FTS5_CURRENT_VERSION && i_version != FTS5_CURRENT_VERSION_SECUREDELETE {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "invalid fts5 file format (found {i_version}, expected {FTS5_CURRENT_VERSION} or {FTS5_CURRENT_VERSION_SECUREDELETE}) - run 'rebuild'"
                ),
            ));
        }
        self.config.i_version = i_version;
        self.config.i_cookie = i_cookie;
        Ok(())
    }

    /// `fts5StructureReadUncached`.
    fn structure_read_uncached(&mut self, ctx: &mut dyn VTabHostContext) -> Result<Structure> {
        let data = self.data_read(ctx, FTS5_STRUCTURE_ROWID)?;
        let (i_cookie, st) = match structure_decode(&data.p[..data.nn as usize]) {
            Ok(v) => v,
            Err(_) => {
                return Err(SqliteError::with_extended(
                    FTS5_CORRUPT,
                    format!("fts5: corrupt structure record for table \"{}\"", self.name),
                ));
            }
        };
        if self.config.pgsz == 0 || self.config.i_cookie != i_cookie {
            self.config_load(ctx, i_cookie)?;
        }
        Ok(st)
    }

    /// `fts5StructureRead`: makes sure `p->pStruct` is loaded.
    pub(crate) fn structure_read(
        &mut self,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<&mut Structure> {
        if self.p_struct.is_none() {
            let st = self.structure_read_uncached(ctx)?;
            self.p_struct = Some(st);
        }
        match self.p_struct.as_mut() {
            Some(s) => Ok(s),
            None => Err(corrupt()),
        }
    }

    /// `fts5StructureRead` followed by `fts5StructureInvalidate`: the caller owns the
    /// structure and the cache is empty.
    fn structure_take(&mut self, ctx: &mut dyn VTabHostContext) -> Result<Structure> {
        self.structure_read(ctx)?;
        self.p_struct.take().ok_or_else(corrupt)
    }

    /// `fts5StructureInvalidate`.
    pub(crate) fn structure_invalidate(&mut self) {
        self.p_struct = None;
    }

    /// `fts5StructureWrite`.
    fn structure_write(&mut self, ctx: &mut dyn VTabHostContext, st: &Structure) -> Result<()> {
        let buf = structure_encode(self.config.i_cookie, st);
        self.data_write(ctx, FTS5_STRUCTURE_ROWID, &buf)
    }

    /// `fts5StructurePromoteTo`.
    fn structure_promote_to(i_promote: usize, sz_promote: i32, st: &mut Structure) {
        if st.a_level[i_promote].n_merge == 0 {
            for il in (i_promote + 1)..st.a_level.len() {
                if st.a_level[il].n_merge != 0 {
                    return;
                }
                let mut is = st.a_level[il].a_seg.len() as isize - 1;
                while is >= 0 {
                    let sz = segment_size(&st.a_level[il].a_seg[is as usize]);
                    if sz > sz_promote {
                        return;
                    }
                    // fts5StructureExtendLevel(.., iPromote, 1, 1): room at the front
                    let seg = st.a_level[il].a_seg[is as usize].clone();
                    st.a_level[i_promote].a_seg.insert(0, seg);
                    // pLvl->nSeg--: the level loses its last segment
                    st.a_level[il].a_seg.truncate(is as usize);
                    sqliters_types::dtrace!(
                        "TR|f5|promote|from={} to={} sz={} max={}",
                        il,
                        i_promote,
                        sz,
                        sz_promote
                    );
                    is -= 1;
                }
            }
        }
    }

    /// `fts5StructurePromote`.
    fn structure_promote(i_lvl: usize, st: &mut Structure) {
        let n_seg = st.a_level[i_lvl].a_seg.len();
        if n_seg == 0 {
            return;
        }
        let sz_seg = segment_size(&st.a_level[i_lvl].a_seg[n_seg - 1]);
        let mut i_promote: isize = -1;
        let mut sz_promote = 0;
        // Check for condition (a)
        let mut i_tst = i_lvl as isize - 1;
        while i_tst >= 0 && st.a_level[i_tst as usize].a_seg.is_empty() {
            i_tst -= 1;
        }
        if i_tst >= 0 {
            let mut sz_max = 0;
            for s in &st.a_level[i_tst as usize].a_seg {
                let sz = s.pgno_last - s.pgno_first + 1;
                if sz > sz_max {
                    sz_max = sz;
                }
            }
            if sz_max >= sz_seg {
                // Condition (a) is true. Promote the newest segment on level iLvl to
                // level iTst.
                i_promote = i_tst;
                sz_promote = sz_max;
            }
        }
        // If condition (a) is not met, assume (b) is true.
        if i_promote < 0 {
            i_promote = i_lvl as isize;
            sz_promote = sz_seg;
        }
        sqliters_types::dtrace!(
            "TR|f5|promchk|lvl={} to={} sz={}",
            i_lvl,
            i_promote,
            sz_promote
        );
        Self::structure_promote_to(i_promote as usize, sz_promote, st);
    }

    /// `fts5AllocateSegid`.
    fn allocate_segid(st: &Structure) -> Result<i32> {
        if st.n_segment >= FTS5_MAX_SEGMENT {
            return Err(SqliteError::new(
                ErrorCode::Full,
                "database or disk is full",
            ));
        }
        let mut a_used = [0u32; ((FTS5_MAX_SEGMENT + 31) / 32) as usize];
        for lvl in &st.a_level {
            for seg in &lvl.a_seg {
                let i_id = seg.i_segid;
                if i_id <= FTS5_MAX_SEGMENT && i_id > 0 {
                    a_used[((i_id - 1) / 32) as usize] |= 1u32 << ((i_id - 1) % 32);
                }
            }
        }
        let mut i = 0usize;
        while i < a_used.len() && a_used[i] == 0xFFFF_FFFF {
            i += 1;
        }
        let mask = if i < a_used.len() { a_used[i] } else { 0 };
        let mut i_segid = 0i32;
        while i_segid < 32 && (mask & (1u32 << i_segid)) != 0 {
            i_segid += 1;
        }
        i_segid += 1 + (i as i32) * 32;
        sqliters_types::dtrace!("TR|f5|segid|new={}", i_segid);
        Ok(i_segid)
    }
}

/// `sqlite3Fts5ConfigSetValue`: returns `Ok(false)` for a bad key or value (`*pbBadkey`).
pub(crate) fn config_set_value(cfg: &mut IndexConfig, key: &str, val: &Value) -> Result<bool> {
    // sqlite3_value_numeric_type(pVal)==SQLITE_INTEGER -> sqlite3_value_int(pVal)
    let int_val = |v: &Value| -> Option<i32> {
        match v {
            Value::Integer(i) => Some(*i as i32),
            Value::Text(s) => {
                let t = s.trim_matches(|c: char| c.is_ascii_whitespace());
                t.parse::<i64>().ok().map(|i| i as i32)
            }
            _ => None,
        }
    };
    let mut ok = true;
    if key.eq_ignore_ascii_case("pgsz") {
        let pgsz = int_val(val).unwrap_or(0);
        if !(32..=64 * 1024).contains(&pgsz) {
            ok = false;
        } else {
            cfg.pgsz = pgsz;
        }
    } else if key.eq_ignore_ascii_case("hashsize") {
        let n = int_val(val).unwrap_or(-1);
        if n <= 0 {
            ok = false;
        } else {
            cfg.n_hash_size = n;
        }
    } else if key.eq_ignore_ascii_case("automerge") {
        let mut n = int_val(val).unwrap_or(-1);
        if !(0..=64).contains(&n) {
            ok = false;
        } else {
            if n == 1 {
                n = 4;
            }
            cfg.n_automerge = n;
        }
    } else if key.eq_ignore_ascii_case("usermerge") {
        let n = int_val(val).unwrap_or(-1);
        if !(2..=16).contains(&n) {
            ok = false;
        } else {
            cfg.n_usermerge = n;
        }
    } else if key.eq_ignore_ascii_case("crisismerge") {
        let mut n = int_val(val).unwrap_or(-1);
        if n < 0 {
            ok = false;
        } else {
            if n <= 1 {
                n = 16;
            }
            if n >= FTS5_MAX_SEGMENT {
                n = FTS5_MAX_SEGMENT - 1;
            }
            cfg.n_crisis_merge = n;
        }
    } else if key.eq_ignore_ascii_case("deletemerge") {
        let mut n = -1;
        match int_val(val) {
            Some(v) => n = v,
            None => ok = false,
        }
        if n < 0 {
            n = 10;
        }
        if n > 100 {
            n = 0;
        }
        cfg.n_delete_merge = n;
    } else if key.eq_ignore_ascii_case("rank") {
        // (validated by the caller: `sqlite3Fts5ConfigParseRank`)
        cfg.rank = match val {
            Value::Null => None,
            other => Some(other.to_text_lossy()),
        };
    } else if key.eq_ignore_ascii_case("secure-delete") {
        let b = int_val(val).unwrap_or(-1);
        if b < 0 {
            ok = false;
        } else {
            cfg.secure_delete = b != 0;
        }
    } else if key.eq_ignore_ascii_case("insttoken") {
        let b = int_val(val).unwrap_or(-1);
        if b < 0 {
            ok = false;
        }
    } else {
        ok = false;
    }
    Ok(ok)
}

// ---------------------------------------------------------------------------------------
// Segment iterator
// ---------------------------------------------------------------------------------------

impl Fts5Index {
    /// `fts5SegIterNextPage`.
    fn seg_iter_next_page(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
    ) -> Result<()> {
        it.leaf = None;
        it.i_leaf_pgno += 1;
        if it.next_leaf.is_some() {
            it.leaf = it.next_leaf.take();
        } else if it.i_leaf_pgno <= it.seg.pgno_last {
            it.leaf = Some(self.leaf_read(ctx, segment_rowid(it.seg.i_segid, it.i_leaf_pgno))?);
        } else {
            it.leaf = None;
        }
        if let Some(leaf) = it.leaf.as_ref() {
            it.i_pgidx_off = leaf.sz_leaf;
            if leaf.is_termless() {
                it.i_endof_doclist = leaf.nn + 1;
            } else {
                let (v, n) = get_varint32(&leaf.p, it.i_pgidx_off as usize);
                it.i_pgidx_off += n as i32;
                it.i_endof_doclist = v;
            }
        }
        Ok(())
    }

    /// `fts5SegIterLoadNPos`.
    fn seg_iter_load_npos(&self, it: &mut SegIter) {
        let Some(leaf) = it.leaf.as_ref() else {
            return;
        };
        let mut i_off = it.i_leaf_offset;
        if self.config.detail == Detail::None {
            let i_eod = i64::from(it.i_endof_doclist.min(leaf.sz_leaf));
            it.b_del = false;
            it.n_pos = 1;
            if i_off < i_eod && byte_at(&leaf.p, i_off as usize) == 0 {
                it.b_del = true;
                i_off += 1;
                if i_off < i_eod && byte_at(&leaf.p, i_off as usize) == 0 {
                    it.n_pos = 1;
                    i_off += 1;
                } else {
                    it.n_pos = 0;
                }
            }
        } else {
            let (n_sz, n) = get_varint32(&leaf.p, i_off as usize);
            i_off += n as i64;
            it.b_del = (n_sz & 1) != 0;
            it.n_pos = n_sz >> 1;
        }
        it.i_leaf_offset = i_off;
    }

    /// `fts5SegIterLoadRowid`.
    fn seg_iter_load_rowid(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
    ) -> Result<()> {
        let mut i_off = it.i_leaf_offset;
        loop {
            let Some(leaf) = it.leaf.as_ref() else {
                return Err(self.corrupt_iter(it));
            };
            if i_off < i64::from(leaf.sz_leaf) {
                break;
            }
            self.seg_iter_next_page(ctx, it)?;
            if it.leaf.is_none() {
                return Err(self.corrupt_iter(it));
            }
            i_off = 4;
        }
        if let Some(leaf) = it.leaf.as_ref() {
            let (v, n) = get_varint(&leaf.p, i_off as usize);
            it.i_rowid = v as i64;
            i_off += n as i64;
        }
        it.i_leaf_offset = i_off;
        Ok(())
    }

    /// `fts5SegIterLoadTerm`.
    fn seg_iter_load_term(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
        n_keep: i32,
    ) -> Result<()> {
        let Some(leaf) = it.leaf.as_ref() else {
            return Err(self.corrupt_iter(it));
        };
        let mut i_off = it.i_leaf_offset;
        let (n_new, n) = get_varint32(&leaf.p, i_off as usize);
        i_off += n as i64;
        if i_off + i64::from(n_new) > i64::from(leaf.sz_leaf)
            || i64::from(n_keep) > it.term.len() as i64
            || n_new == 0
            || n_new < 0
            || n_keep < 0
        {
            return Err(self.corrupt_iter(it));
        }
        it.term.truncate(n_keep as usize);
        it.term
            .extend_from_slice(&leaf.p[i_off as usize..(i_off + i64::from(n_new)) as usize]);
        i_off += i64::from(n_new);
        it.i_term_leaf_offset = i_off as i32;
        it.i_term_leaf_pgno = it.i_leaf_pgno;
        it.i_leaf_offset = i_off;
        if it.i_pgidx_off >= leaf.nn {
            it.i_endof_doclist = leaf.nn + 1;
        } else {
            let (n_extra, n) = get_varint32(&leaf.p, it.i_pgidx_off as usize);
            it.i_pgidx_off += n as i32;
            it.i_endof_doclist += n_extra;
        }
        self.seg_iter_load_rowid(ctx, it)
    }

    /// `fts5SegIterAllocTombstone`.
    fn seg_iter_alloc_tombstone(it: &mut SegIter) {
        let n_tomb = it.seg.n_pg_tombstone;
        if n_tomb > 0 {
            let mut ap = Vec::with_capacity(n_tomb as usize);
            ap.resize_with(n_tomb as usize, || None);
            it.tomb = Some(TombstoneArray {
                n_tombstone: n_tomb,
                ap,
            });
        }
    }

    /// `fts5SegIterInit`.
    fn seg_iter_init(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg_pos: (usize, usize),
        seg: &StructureSegment,
        it: &mut SegIter,
    ) -> Result<()> {
        if seg.pgno_first == 0 {
            // The segment is an input to an incremental merge and all of its data has
            // already been merged: the iterator stays at EOF.
            return Ok(());
        }
        *it = SegIter::default();
        it.seg_pos = Some(seg_pos);
        it.seg = seg.clone();
        it.i_leaf_pgno = seg.pgno_first - 1;
        loop {
            self.seg_iter_next_page(ctx, it)?;
            if !it.leaf.as_ref().is_some_and(|l| l.nn == 4) {
                break;
            }
        }
        if let Some(sz_leaf) = it.leaf.as_ref().map(|l| l.sz_leaf) {
            it.i_leaf_offset = 4;
            it.i_pgidx_off = sz_leaf + 1;
            self.seg_iter_load_term(ctx, it, 0)?;
            self.seg_iter_load_npos(it);
            Self::seg_iter_alloc_tombstone(it);
        }
        Ok(())
    }

    /// `pIter->xNext`: `fts5SegIterNext` or `fts5SegIterNext_None`.
    fn seg_iter_next(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
        pb_new_term: Option<&mut bool>,
    ) -> Result<()> {
        if self.config.detail == Detail::None {
            self.seg_iter_next_none(ctx, it, pb_new_term)
        } else {
            self.seg_iter_next_full(ctx, it, pb_new_term)
        }
    }

    /// `fts5SegIterNext_None`.
    fn seg_iter_next_none(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
        pb_new_term: Option<&mut bool>,
    ) -> Result<()> {
        let mut i_off = it.i_leaf_offset;
        if it.hash_entries.is_some() {
            let Some(leaf) = it.leaf.as_ref() else {
                return Ok(());
            };
            if i_off < i64::from(it.i_endof_doclist) {
                let (d, n) = get_varint(&leaf.p, i_off as usize);
                i_off += n as i64;
                it.i_leaf_offset = i_off;
                it.i_rowid = it.i_rowid.wrapping_add(d as i64);
            } else if (it.flags & FTS5_SEGITER_ONETERM) == 0 {
                if !Self::seg_iter_hash_advance(it) {
                    it.leaf = None;
                    return Ok(());
                }
                if let Some(l) = it.leaf.as_ref() {
                    it.i_endof_doclist = l.nn;
                }
                if let Some(b) = pb_new_term {
                    *b = true;
                }
            } else {
                it.leaf = None;
                return Ok(());
            }
            self.seg_iter_load_npos(it);
            return Ok(());
        }
        // Next entry is on the next page
        loop {
            let Some(leaf) = it.leaf.as_ref() else {
                return Ok(());
            };
            if i_off < i64::from(leaf.sz_leaf) {
                break;
            }
            self.seg_iter_next_page(ctx, it)?;
            if it.leaf.is_none() {
                return Ok(());
            }
            it.i_rowid = 0;
            i_off = 4;
        }
        let Some(leaf) = it.leaf.as_ref() else {
            return Ok(());
        };
        if i_off < i64::from(it.i_endof_doclist) {
            // Next entry is on the current page
            let (d, n) = get_varint(&leaf.p, i_off as usize);
            i_off += n as i64;
            it.i_leaf_offset = i_off;
            it.i_rowid = it.i_rowid.wrapping_add(d as i64);
        } else if (it.flags & FTS5_SEGITER_ONETERM) == 0 {
            let mut n_keep = 0;
            if i_off != i64::from(leaf.first_term_off()) {
                let (k, n) = get_varint32(&leaf.p, i_off as usize);
                n_keep = k;
                i_off += n as i64;
            }
            it.i_leaf_offset = i_off;
            self.seg_iter_load_term(ctx, it, n_keep)?;
            if let Some(b) = pb_new_term {
                *b = true;
            }
        } else {
            it.leaf = None;
            return Ok(());
        }
        self.seg_iter_load_npos(it);
        Ok(())
    }

    /// `fts5SegIterNext`.
    fn seg_iter_next_full(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
        pb_new_term: Option<&mut bool>,
    ) -> Result<()> {
        let Some(leaf) = it.leaf.as_ref() else {
            return Ok(());
        };
        let mut b_new_term = false;
        let mut n_keep = 0;
        let n = i64::from(leaf.sz_leaf);
        let mut i_off = it.i_leaf_offset + i64::from(it.n_pos);
        if i_off < n {
            // The next entry is on the current page.
            if i_off >= i64::from(it.i_endof_doclist) {
                b_new_term = true;
                if i_off != i64::from(leaf.first_term_off()) {
                    let (k, nb) = get_varint32(&leaf.p, i_off as usize);
                    n_keep = k;
                    i_off += nb as i64;
                }
            } else {
                let (d, nb) = get_varint(&leaf.p, i_off as usize);
                i_off += nb as i64;
                it.i_rowid = it.i_rowid.wrapping_add(d as i64);
            }
            it.i_leaf_offset = i_off;
        } else if it.hash_entries.is_some() {
            let mut advanced = false;
            if (it.flags & FTS5_SEGITER_ONETERM) == 0 {
                advanced = Self::seg_iter_hash_advance(it);
            }
            if !advanced {
                it.leaf = None;
            } else {
                if let Some(l) = it.leaf.as_ref() {
                    it.i_endof_doclist = l.nn + 1;
                }
                if let Some(b) = pb_new_term {
                    *b = true;
                }
                // (bNewTerm stays 0 here: the size field of the first entry is read below)
                if let Some(leaf) = it.leaf.as_ref() {
                    let (n_sz, nb) = get_varint32(&leaf.p, it.i_leaf_offset as usize);
                    it.i_leaf_offset += nb as i64;
                    it.b_del = (n_sz & 1) != 0;
                    it.n_pos = n_sz >> 1;
                }
            }
            return Ok(());
        } else {
            let mut i_off2: i32 = 0;
            // Next entry is not on the current page
            while i_off2 == 0 {
                self.seg_iter_next_page(ctx, it)?;
                let Some(leaf) = it.leaf.as_ref() else {
                    break;
                };
                i_off2 = leaf.first_rowid_off();
                if i_off2 != 0 && i_off2 < leaf.sz_leaf {
                    let (v, nb) = get_varint(&leaf.p, i_off2 as usize);
                    it.i_rowid = v as i64;
                    i_off2 += nb as i32;
                    it.i_leaf_offset = i64::from(i_off2);
                    if leaf.nn > leaf.sz_leaf {
                        let (eod, nb) = get_varint32(&leaf.p, leaf.sz_leaf as usize);
                        it.i_pgidx_off = leaf.sz_leaf + nb as i32;
                        it.i_endof_doclist = eod;
                    }
                } else if leaf.nn > leaf.sz_leaf {
                    let (v, nb) = get_varint32(&leaf.p, leaf.sz_leaf as usize);
                    it.i_pgidx_off = leaf.sz_leaf + nb as i32;
                    i_off2 = v;
                    it.i_leaf_offset = i64::from(i_off2);
                    it.i_endof_doclist = i_off2;
                    b_new_term = true;
                }
                if i_off2 > leaf.sz_leaf {
                    return Err(self.corrupt_iter(it));
                }
            }
        }

        // Check if the iterator is now at EOF. If so, return early.
        if it.leaf.is_some() {
            if b_new_term {
                if (it.flags & FTS5_SEGITER_ONETERM) != 0 {
                    it.leaf = None;
                } else {
                    self.seg_iter_load_term(ctx, it, n_keep)?;
                    self.seg_iter_load_npos(it);
                    if let Some(b) = pb_new_term {
                        *b = true;
                    }
                }
            } else if let Some(leaf) = it.leaf.as_ref() {
                let (n_sz, nb) = get_varint32(&leaf.p, it.i_leaf_offset as usize);
                it.i_leaf_offset += nb as i64;
                it.b_del = (n_sz & 1) != 0;
                it.n_pos = n_sz >> 1;
            }
        }
        Ok(())
    }

    /// `fts5DlidxIterInit` (forward).
    fn dlidx_iter_init(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_segid: i32,
        i_leaf_pg: i32,
    ) -> Result<DlidxIter> {
        let mut iter = DlidxIter {
            i_segid,
            a_lvl: Vec::new(),
        };
        let mut i = 0;
        loop {
            let i_rowid = dlidx_rowid(i_segid, i, i_leaf_pg);
            let data = self.data_read(ctx, i_rowid)?;
            let b_done = (byte_at(&data.p, 0) & 0x01) == 0;
            iter.a_lvl.push(DlidxLvl {
                data: Some(data),
                ..DlidxLvl::default()
            });
            if iter.a_lvl.len() > (1 << FTS5_DATA_HEIGHT_B) {
                return Err(corrupt());
            }
            if b_done {
                break;
            }
            i += 1;
        }
        // fts5DlidxIterFirst
        for lvl in &mut iter.a_lvl {
            dlidx_lvl_next(lvl);
        }
        Ok(iter)
    }

    /// `fts5DlidxIterNextR`.
    fn dlidx_iter_next_r(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        iter: &mut DlidxIter,
        i_lvl: usize,
    ) -> Result<bool> {
        if dlidx_lvl_next(&mut iter.a_lvl[i_lvl]) && (i_lvl + 1) < iter.a_lvl.len() {
            self.dlidx_iter_next_r(ctx, iter, i_lvl + 1)?;
            if !iter.a_lvl[i_lvl + 1].b_eof {
                let pg = iter.a_lvl[i_lvl + 1].i_leaf_pgno;
                let data = self.data_read(ctx, dlidx_rowid(iter.i_segid, i_lvl as i32, pg))?;
                iter.a_lvl[i_lvl] = DlidxLvl {
                    data: Some(data),
                    ..DlidxLvl::default()
                };
                dlidx_lvl_next(&mut iter.a_lvl[i_lvl]);
            }
        }
        Ok(iter.a_lvl[0].b_eof)
    }

    /// `fts5SegIterLoadDlidx`.
    fn seg_iter_load_dlidx(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
    ) -> Result<()> {
        let Some(leaf) = it.leaf.as_ref() else {
            return Ok(());
        };
        // Check if the current doclist ends on this page. If it does, return early without
        // loading the doclist-index (as it belongs to a different term.
        if it.i_term_leaf_pgno == it.i_leaf_pgno && it.i_endof_doclist < leaf.sz_leaf {
            return Ok(());
        }
        it.dlidx = Some(self.dlidx_iter_init(ctx, it.seg.i_segid, it.i_term_leaf_pgno)?);
        Ok(())
    }

    /// `fts5LeafSeek` with `bGe == 0`.
    fn leaf_seek(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
        term: &[u8],
    ) -> Result<()> {
        let Some(leaf) = it.leaf.as_ref() else {
            return Ok(());
        };
        let a = &leaf.p;
        let n = leaf.nn as u32;
        let n_term = term.len() as u32;
        let mut n_match: u32 = 0;
        let mut n_keep: u32 = 0;
        let mut n_new: u32;
        let mut i_pgidx = leaf.sz_leaf as u32;
        let (v, nb) = get_varint32(a, i_pgidx as usize);
        i_pgidx += nb as u32;
        let mut i_term_off = v as u32;
        let mut i_off = i_term_off;
        if i_off > n {
            return Err(self.corrupt_iter(it));
        }
        let found;
        loop {
            // Figure out how many new bytes are in this term
            let (v, nb) = get_varint32(a, i_off as usize);
            n_new = v as u32;
            i_off += nb as u32;
            if n_keep < n_match {
                found = false;
                break;
            }
            if i_off.wrapping_add(n_new) > n {
                return Err(self.corrupt_iter(it));
            }
            if n_keep == n_match {
                let n_cmp = n_new.min(n_term - n_match);
                let mut i = 0u32;
                while i < n_cmp {
                    if byte_at(a, (i_off + i) as usize) != term[(n_match + i) as usize] {
                        break;
                    }
                    i += 1;
                }
                n_match += i;
                if n_term == n_match {
                    found = i == n_new;
                    break;
                } else if i < n_new && byte_at(a, (i_off + i) as usize) > term[n_match as usize] {
                    found = false;
                    break;
                }
            }
            if i_pgidx >= n {
                found = false;
                break;
            }
            let (k, nb) = get_varint32(a, i_pgidx as usize);
            i_pgidx += nb as u32;
            i_term_off = i_term_off.wrapping_add(k as u32);
            i_off = i_term_off;
            if i_off >= n {
                return Err(self.corrupt_iter(it));
            }
            // Read the nKeep field of the next term.
            let (k, nb) = get_varint32(a, i_off as usize);
            n_keep = k as u32;
            i_off += nb as u32;
        }
        if !found {
            // search_failed with bGe==0
            it.leaf = None;
            return Ok(());
        }
        // search_success:
        if i64::from(i_off) + i64::from(n_new) > i64::from(n) || n_new < 1 {
            return Err(self.corrupt_iter(it));
        }
        it.i_leaf_offset = i64::from(i_off + n_new);
        it.i_term_leaf_offset = it.i_leaf_offset as i32;
        it.i_term_leaf_pgno = it.i_leaf_pgno;
        it.term.clear();
        it.term
            .extend_from_slice(&term[..(n_keep as usize).min(term.len())]);
        it.term
            .extend_from_slice(&a[i_off as usize..(i_off + n_new) as usize]);
        if i_pgidx >= n {
            it.i_endof_doclist = leaf.nn + 1;
        } else {
            let (n_extra, nb) = get_varint32(a, i_pgidx as usize);
            i_pgidx += nb as u32;
            it.i_endof_doclist = (i_term_off as i32).wrapping_add(n_extra);
        }
        it.i_pgidx_off = i_pgidx as i32;
        self.seg_iter_load_rowid(ctx, it)?;
        self.seg_iter_load_npos(it);
        Ok(())
    }

    /// `SELECT pgno FROM %_idx WHERE segid=? AND term<=? ORDER BY term DESC LIMIT 1`.
    fn idx_lookup_le(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_segid: i32,
        term: &[u8],
    ) -> Result<Option<i64>> {
        if !self.idx_cache_on {
            let rows = ctx.query_sql(
                &self.sql_idx_select,
                &[
                    Value::Integer(i64::from(i_segid)),
                    Value::Blob(term.to_vec()),
                ],
            )?;
            return Ok(rows
                .first()
                .and_then(|r| r.first())
                .map(Value::to_integer_lossy));
        }
        if !self.idx_cache.contains_key(&i_segid) {
            let rows = ctx.query_sql(&self.sql_idx_scan, &[Value::Integer(i64::from(i_segid))])?;
            let mut v: Vec<(Vec<u8>, i64)> = Vec::with_capacity(rows.len());
            let mut all_blobs = true;
            for mut r in rows {
                if r.len() < 2 {
                    continue;
                }
                let pg = r[1].to_integer_lossy();
                match r.swap_remove(0) {
                    Value::Blob(b) => v.push((b, pg)),
                    _ => all_blobs = false,
                }
            }
            if !all_blobs {
                // terms of another storage class do not compare like blobs: ask the table
                let rows = ctx.query_sql(
                    &self.sql_idx_select,
                    &[
                        Value::Integer(i64::from(i_segid)),
                        Value::Blob(term.to_vec()),
                    ],
                )?;
                return Ok(rows
                    .first()
                    .and_then(|r| r.first())
                    .map(Value::to_integer_lossy));
            }
            v.sort_by(|a, b| a.0.cmp(&b.0));
            self.idx_cache.insert(i_segid, v);
        }
        let Some(v) = self.idx_cache.get(&i_segid) else {
            return Ok(None);
        };
        let pos = v.partition_point(|(k, _)| k.as_slice() <= term);
        Ok(if pos > 0 { Some(v[pos - 1].1) } else { None })
    }

    /// `fts5SegIterSeekInit` for a plain term lookup (flags without SCAN / DESC).
    fn seg_iter_seek_init(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        term: &[u8],
        seg_pos: (usize, usize),
        seg: &StructureSegment,
        it: &mut SegIter,
    ) -> Result<()> {
        let mut i_pg: i32 = 1;
        let mut b_dlidx = false;
        *it = SegIter::default();
        it.seg_pos = Some(seg_pos);
        it.seg = seg.clone();

        // This block sets stack variable iPg to the leaf page number that may contain
        // term (pTerm/nTerm), if it is present in the segment.
        if let Some(val) = self.idx_lookup_le(ctx, seg.i_segid, term)? {
            let val = i64::from(val as i32);
            i_pg = (val >> 1) as i32;
            b_dlidx = (val & 1) != 0;
        }
        if i_pg < seg.pgno_first {
            i_pg = seg.pgno_first;
            b_dlidx = false;
        }
        it.i_leaf_pgno = i_pg - 1;
        self.seg_iter_next_page(ctx, it)?;
        if it.leaf.is_some() {
            self.leaf_seek(ctx, it, term)?;
        }
        it.flags |= FTS5_SEGITER_ONETERM;
        if it.leaf.is_some() && b_dlidx {
            self.seg_iter_load_dlidx(ctx, it)?;
        }
        Self::seg_iter_alloc_tombstone(it);
        Ok(())
    }

    /// `fts5SegIterGotoPage`.
    fn seg_iter_goto_page(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
        i_leaf_pgno: i32,
    ) -> Result<()> {
        if i_leaf_pgno > it.seg.pgno_last {
            return Err(self.corrupt_idx());
        }
        it.next_leaf = None;
        it.i_leaf_pgno = i_leaf_pgno - 1;
        loop {
            self.seg_iter_next_page(ctx, it)?;
            let Some(leaf) = it.leaf.as_ref() else {
                break;
            };
            let mut i_off = leaf.first_rowid_off();
            if i_off > 0 {
                let n = leaf.sz_leaf;
                if i_off < 4 || i_off >= n {
                    return Err(self.corrupt_idx());
                }
                let (v, nb) = get_varint(&leaf.p, i_off as usize);
                it.i_rowid = v as i64;
                i_off += nb as i32;
                it.i_leaf_offset = i64::from(i_off);
                self.seg_iter_load_npos(it);
                break;
            }
        }
        Ok(())
    }

    /// `fts5SegIterNextFrom` (forward).
    fn seg_iter_next_from(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
        i_match: i64,
    ) -> Result<()> {
        let mut i_leaf_pgno = it.i_leaf_pgno;
        let mut b_move = true;
        if let Some(mut dl) = it.dlidx.take() {
            let res = (|| -> Result<()> {
                while !dl.a_lvl[0].b_eof && i_match > dl.a_lvl[0].i_rowid {
                    i_leaf_pgno = dl.a_lvl[0].i_leaf_pgno;
                    self.dlidx_iter_next_r(ctx, &mut dl, 0)?;
                }
                Ok(())
            })();
            it.dlidx = Some(dl);
            res?;
        }
        if i_leaf_pgno > it.i_leaf_pgno {
            self.seg_iter_goto_page(ctx, it, i_leaf_pgno)?;
            b_move = false;
        }
        loop {
            if b_move {
                self.seg_iter_next(ctx, it, None)?;
            }
            if it.leaf.is_none() {
                break;
            }
            if it.i_rowid >= i_match {
                break;
            }
            b_move = true;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Multi-segment iterator
    // -----------------------------------------------------------------------------------

    /// `fts5MultiIterAdvanced`.
    fn multi_iter_advanced(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        i_changed: usize,
        i_minset: usize,
    ) -> Result<()> {
        let mut i = (mi.n_seg + i_changed) / 2;
        while i >= i_minset && i > 0 {
            let i_eq = mi.do_compare(i);
            if i_eq != 0 {
                let mut seg = std::mem::take(&mut mi.a_seg[i_eq]);
                let r = self.seg_iter_next(ctx, &mut seg, None);
                mi.a_seg[i_eq] = seg;
                r?;
                i = mi.n_seg + i_eq;
            }
            i /= 2;
        }
        Ok(())
    }

    /// `fts5MultiIterIsDeleted`.
    fn multi_iter_is_deleted(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
    ) -> Result<bool> {
        let i_first = mi.first();
        let seg = &mut mi.a_seg[i_first];
        if seg.leaf.is_none() {
            return Ok(false);
        }
        let i_rowid = seg.i_rowid as u64;
        let i_segid = seg.seg.i_segid;
        let Some(arr) = seg.tomb.as_mut() else {
            return Ok(false);
        };
        // Figure out which page the rowid might be present on.
        let i_pg = (i_rowid % arr.n_tombstone as u64) as usize;
        // If tombstone hash page iPg has not yet been loaded from the database, load it
        // now.
        if arr.ap[i_pg].is_none() {
            match self.data_read(ctx, tombstone_rowid(i_segid, i_pg as i32)) {
                Ok(d) => arr.ap[i_pg] = Some(d),
                Err(e) => return Err(e),
            }
        }
        match arr.ap[i_pg].as_ref() {
            Some(pg) => Ok(tombstone_query(pg, arr.n_tombstone, i_rowid)),
            None => Ok(false),
        }
    }

    /// `fts5MultiIterNext`.
    fn multi_iter_next(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        b_from: bool,
        i_from: i64,
    ) -> Result<()> {
        let mut b_use_from = b_from;
        loop {
            let i_first = mi.first();
            let mut b_new_term = false;
            let mut seg = std::mem::take(&mut mi.a_seg[i_first]);
            let r = if b_use_from && seg.dlidx.is_some() {
                self.seg_iter_next_from(ctx, &mut seg, i_from)
            } else {
                self.seg_iter_next(ctx, &mut seg, Some(&mut b_new_term))
            };
            let at_eof = seg.leaf.is_none();
            mi.a_seg[i_first] = seg;
            r?;
            let mut cur = i_first;
            let advanced = if at_eof || b_new_term {
                true
            } else {
                let (coll, i_new) = mi.advance_rowid(i_first);
                cur = i_new;
                coll
            };
            if advanced {
                self.multi_iter_advanced(ctx, mi, i_first, 1)?;
                mi.set_eof();
                cur = mi.first();
                if mi.a_seg[cur].leaf.is_none() {
                    return Ok(());
                }
            }
            if (!mi.b_skip_empty || mi.a_seg[cur].n_pos != 0)
                && !self.multi_iter_is_deleted(ctx, mi)?
            {
                return Ok(());
            }
            b_use_from = false;
        }
    }

    /// `fts5MultiIterFinishSetup`.
    fn multi_iter_finish_setup(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
    ) -> Result<()> {
        let mut i_iter = mi.n_seg - 1;
        while i_iter > 0 {
            let i_eq = mi.do_compare(i_iter);
            if i_eq != 0 {
                let mut seg = std::mem::take(&mut mi.a_seg[i_eq]);
                let r = self.seg_iter_next(ctx, &mut seg, None);
                mi.a_seg[i_eq] = seg;
                r?;
                self.multi_iter_advanced(ctx, mi, i_eq, i_iter)?;
            }
            i_iter -= 1;
        }
        mi.set_eof();
        let empty = mi.b_skip_empty && {
            let s = &mi.a_seg[mi.first()];
            s.leaf.is_some() && s.n_pos == 0
        };
        if empty || self.multi_iter_is_deleted(ctx, mi)? {
            self.multi_iter_next(ctx, mi, false, 0)?;
        }
        Ok(())
    }

    /// `fts5MultiIterNew(p, pStruct, flags, 0, 0, 0, iLevel, nSegment, &pIter)`: an iterator
    /// over the first `n_segment` segments of level `i_level`.
    fn multi_iter_new_level(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &Structure,
        i_level: usize,
        n_segment: usize,
    ) -> Result<MultiIter> {
        let n_seg = st.a_level[i_level].a_seg.len().min(n_segment);
        let mut mi = MultiIter::alloc(n_seg);
        let mut i_iter = 0;
        for i_seg in (0..n_seg).rev() {
            let mut it = SegIter::default();
            self.seg_iter_init(
                ctx,
                (i_level, i_seg),
                &st.a_level[i_level].a_seg[i_seg],
                &mut it,
            )?;
            mi.a_seg[i_iter] = it;
            i_iter += 1;
        }
        self.multi_iter_finish_setup(ctx, &mut mi)?;
        Ok(mi)
    }

    /// `fts5MultiIterNew(p, pStruct, FTS5INDEX_QUERY_SKIPHASH, 0, pTerm, nTerm, -1, 0, ..)`:
    /// an iterator over the entries of one term in every segment.
    fn multi_iter_new_term(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &Structure,
        term: &[u8],
    ) -> Result<MultiIter> {
        let n_seg = st.n_segment.max(0) as usize;
        let mut mi = MultiIter::alloc(n_seg);
        let mut i_iter = 0;
        for (i_lvl, lvl) in st.a_level.iter().enumerate() {
            for i_seg in (0..lvl.a_seg.len()).rev() {
                let mut it = SegIter::default();
                self.seg_iter_seek_init(ctx, term, (i_lvl, i_seg), &lvl.a_seg[i_seg], &mut it)?;
                if i_iter < mi.a_seg.len() {
                    mi.a_seg[i_iter] = it;
                }
                i_iter += 1;
            }
        }
        self.multi_iter_finish_setup(ctx, &mut mi)?;
        Ok(mi)
    }

    /// `fts5MultiIterNextFrom`.
    fn multi_iter_next_from(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        i_match: i64,
    ) -> Result<()> {
        loop {
            self.multi_iter_next(ctx, mi, true, i_match)?;
            if mi.b_eof {
                break;
            }
            let i_rowid = mi.a_seg[mi.first()].i_rowid;
            if i_rowid >= i_match {
                break;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Segment writer
// ---------------------------------------------------------------------------------------

impl Fts5Index {
    /// `fts5WriteInit`.
    fn write_init(&self, i_segid: i32) -> SegWriter {
        let n_buffer = self.config.pgsz.max(0) as usize + FTS5_DATA_PADDING;
        let mut w = SegWriter {
            i_segid,
            ..SegWriter::default()
        };
        w.a_dlidx.push(DlidxWriter::default());
        w.writer.pgno = 1;
        w.b_first_term_in_page = true;
        w.i_bt_page = 1;
        w.writer.pgidx.reserve(n_buffer);
        w.writer.buf.reserve(n_buffer);
        // Initialize the 4-byte leaf-page header to 0x00.
        w.writer.buf.extend_from_slice(&[0, 0, 0, 0]);
        w
    }

    /// `fts5WriteDlidxClear`.
    fn write_dlidx_clear(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
        b_flush: bool,
    ) -> Result<()> {
        for i in 0..w.a_dlidx.len() {
            if w.a_dlidx[i].buf.is_empty() {
                break;
            }
            if b_flush {
                let rowid = dlidx_rowid(w.i_segid, i as i32, w.a_dlidx[i].pgno);
                let buf = std::mem::take(&mut w.a_dlidx[i].buf);
                self.data_write(ctx, rowid, &buf)?;
            }
            w.a_dlidx[i].buf.clear();
            w.a_dlidx[i].b_prev_valid = false;
        }
        Ok(())
    }

    /// `fts5WriteFlushDlidx`.
    fn write_flush_dlidx(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
    ) -> Result<bool> {
        let mut b_flag = false;
        // If there were FTS5_MIN_DLIDX_SIZE or more empty leaf pages written to the
        // database, also write the doclist-index to disk.
        if !w.a_dlidx[0].buf.is_empty() && w.n_empty >= FTS5_MIN_DLIDX_SIZE {
            b_flag = true;
        }
        sqliters_types::dtrace!(
            "TR|f5|dlidx|flag={} empty={} n0={}",
            u8::from(b_flag),
            w.n_empty,
            w.a_dlidx[0].buf.len()
        );
        self.write_dlidx_clear(ctx, w, b_flag)?;
        w.n_empty = 0;
        Ok(b_flag)
    }

    /// `fts5WriteFlushBtree`.
    fn write_flush_btree(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
    ) -> Result<()> {
        if w.i_bt_page == 0 {
            return Ok(());
        }
        let b_flag = self.write_flush_dlidx(ctx, w)?;
        let pgno_val = i64::from(b_flag) + (i64::from(w.i_bt_page) << 1);
        sqliters_types::dtrace!(
            "TR|f5|w|tbl=idx segid={} term={:016x}/{} pgno={}",
            w.i_segid,
            trace_hash(&w.btterm),
            w.btterm.len(),
            pgno_val
        );
        ctx.execute_sql_counted(
            &self.sql_idx_insert,
            &[
                Value::Integer(i64::from(w.i_segid)),
                Value::Blob(w.btterm.clone()),
                Value::Integer(pgno_val),
            ],
        )
        .map_err(rc_only)?;
        self.idx_cache.remove(&w.i_segid);
        w.i_bt_page = 0;
        Ok(())
    }

    /// `fts5WriteBtreeTerm`.
    fn write_btree_term(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
        term: &[u8],
    ) -> Result<()> {
        self.write_flush_btree(ctx, w)?;
        w.btterm.clear();
        w.btterm.extend_from_slice(term);
        w.i_bt_page = w.writer.pgno;
        Ok(())
    }

    /// `fts5WriteBtreeNoTerm`.
    fn write_btree_no_term(w: &mut SegWriter) {
        // If there were no rowids on the leaf page either and the doclist-index has
        // already been started, append an 0x00 byte to it.
        if w.b_first_rowid_in_page && !w.a_dlidx[0].buf.is_empty() {
            put_varint(&mut w.a_dlidx[0].buf, 0);
        }
        // Increment the "number of sequential leaves without a term" counter.
        w.n_empty += 1;
    }

    /// `fts5WriteDlidxAppend`.
    fn write_dlidx_append(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
        i_rowid: i64,
    ) -> Result<()> {
        let mut b_done = false;
        let mut i = 0usize;
        while !b_done {
            let i_val: i64;
            if w.a_dlidx[i].buf.len() as i64 >= i64::from(self.config.pgsz) {
                // The current doclist-index page is full. Write it to disk and push a
                // copy of iRowid (which will become the first rowid on the next
                // doclist-index leaf page) up into the next level of the b-tree
                // hierarchy. If the node being flushed is currently the root node, also
                // push its first rowid upwards.
                w.a_dlidx[i].buf[0] = 0x01; // Not the root node
                let rowid = dlidx_rowid(w.i_segid, i as i32, w.a_dlidx[i].pgno);
                let page = w.a_dlidx[i].buf.clone();
                self.data_write(ctx, rowid, &page)?;
                // fts5WriteDlidxGrow(p, pWriter, i+2)
                while w.a_dlidx.len() < i + 2 {
                    w.a_dlidx.push(DlidxWriter::default());
                }
                if w.a_dlidx[i + 1].buf.is_empty() {
                    // fts5DlidxExtractFirstRowid
                    let b = &w.a_dlidx[i].buf;
                    let (_, n) = get_varint(b, 1);
                    let (first, _) = get_varint(b, 1 + n);
                    let i_first = first as i64;
                    // This was the root node. Push its first rowid up to the new root.
                    let pg = w.a_dlidx[i].pgno;
                    let up = &mut w.a_dlidx[i + 1];
                    up.pgno = pg;
                    put_varint(&mut up.buf, 0);
                    put_varint(&mut up.buf, pg as i64 as u64);
                    put_varint(&mut up.buf, i_first as u64);
                    up.b_prev_valid = true;
                    up.i_prev = i_first;
                }
                w.a_dlidx[i].buf.clear();
                w.a_dlidx[i].b_prev_valid = false;
                w.a_dlidx[i].pgno += 1;
            } else {
                b_done = true;
            }

            if w.a_dlidx[i].b_prev_valid {
                i_val = (i_rowid as u64).wrapping_sub(w.a_dlidx[i].i_prev as u64) as i64;
            } else {
                let i_pgno = if i == 0 {
                    i64::from(w.writer.pgno)
                } else {
                    i64::from(w.a_dlidx[i - 1].pgno)
                };
                put_varint(&mut w.a_dlidx[i].buf, u64::from(!b_done));
                put_varint(&mut w.a_dlidx[i].buf, i_pgno as u64);
                i_val = i_rowid;
            }
            put_varint(&mut w.a_dlidx[i].buf, i_val as u64);
            w.a_dlidx[i].b_prev_valid = true;
            w.a_dlidx[i].i_prev = i_rowid;
            i += 1;
        }
        Ok(())
    }

    /// `fts5WriteFlushLeaf`.
    fn write_flush_leaf(&mut self, ctx: &mut dyn VTabHostContext, w: &mut SegWriter) -> Result<()> {
        // Set the szLeaf header field.
        let n = w.writer.buf.len();
        put_u16(&mut w.writer.buf, 2, n as u16);
        if w.b_first_term_in_page {
            // No term was written to this page.
            Self::write_btree_no_term(w);
        } else {
            // Append the pgidx to the page buffer. Set the szLeaf header field.
            let pgidx = std::mem::take(&mut w.writer.pgidx);
            w.writer.buf.extend_from_slice(&pgidx);
            w.writer.pgidx = pgidx;
        }
        sqliters_types::dtrace!(
            "TR|f5|leaf|segid={} pgno={} sz={} nn={} term={}",
            w.i_segid,
            w.writer.pgno,
            n,
            w.writer.buf.len(),
            u8::from(!w.b_first_term_in_page)
        );
        // Write the page out to disk
        let i_rowid = segment_rowid(w.i_segid, w.writer.pgno);
        let page = std::mem::take(&mut w.writer.buf);
        let res = self.data_write(ctx, i_rowid, &page);
        w.writer.buf = page;
        res?;
        // Initialize the next page.
        w.writer.buf.clear();
        w.writer.pgidx.clear();
        w.writer.buf.extend_from_slice(&[0, 0, 0, 0]);
        w.writer.i_prev_pgidx = 0;
        w.writer.pgno += 1;
        // Increase the leaves written counter
        w.n_leaf_written += 1;
        // The new leaf holds no terms or rowids
        w.b_first_term_in_page = true;
        w.b_first_rowid_in_page = true;
        Ok(())
    }

    /// `fts5WriteAppendTerm`.
    fn write_append_term(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
        term: &[u8],
    ) -> Result<()> {
        let n_term = term.len();
        let pgsz = i64::from(self.config.pgsz);
        // If the current leaf page is full, flush it to disk.
        if (w.writer.buf.len() + w.writer.pgidx.len() + n_term + 2) as i64 >= pgsz
            && w.writer.buf.len() > 4
        {
            self.write_flush_leaf(ctx, w)?;
        }
        let n_min = w.writer.term.len().min(n_term);

        // TODO1: Updating pgidx here.
        put_varint(
            &mut w.writer.pgidx,
            (w.writer.buf.len() as i64 - i64::from(w.writer.i_prev_pgidx)) as u64,
        );
        w.writer.i_prev_pgidx = w.writer.buf.len() as i32;

        let n_prefix;
        if w.b_first_term_in_page {
            n_prefix = 0;
            if w.writer.pgno != 1 {
                // This is the first term on a leaf that is not the leftmost leaf in the
                // segment b-tree. In this case it is necessary to add a term to the
                // b-tree hierarchy that is (a) larger than the largest term already
                // written to the segment and (b) smaller than or equal to this term. In
                // other words, a prefix of (pTerm/nTerm) that is one byte longer than
                // the common prefix (if any) of zTerm and pPage->term.
                let mut n = n_term;
                if !w.writer.term.is_empty() {
                    n = 1 + prefix_compress(n_min, &w.writer.term, term);
                }
                let n = n.min(n_term);
                self.write_btree_term(ctx, w, &term[..n])?;
            }
        } else {
            n_prefix = prefix_compress(n_min, &w.writer.term, term);
            put_varint(&mut w.writer.buf, n_prefix as u64);
        }
        sqliters_types::dtrace!(
            "TR|f5|term|segid={} pgno={} off={} n={} pre={}",
            w.i_segid,
            w.writer.pgno,
            w.writer.i_prev_pgidx,
            n_term,
            n_prefix
        );

        // Append the number of bytes of new data, then the term data itself to the page.
        put_varint(&mut w.writer.buf, (n_term - n_prefix) as u64);
        w.writer.buf.extend_from_slice(&term[n_prefix..]);

        // Update the Fts5PageWriter.term field.
        w.writer.term.clear();
        w.writer.term.extend_from_slice(term);
        w.b_first_term_in_page = false;
        w.b_first_rowid_in_page = false;
        w.b_first_rowid_in_doclist = true;
        w.a_dlidx[0].pgno = w.writer.pgno;
        Ok(())
    }

    /// `fts5WriteAppendRowid`.
    fn write_append_rowid(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
        i_rowid: i64,
    ) -> Result<()> {
        if (w.writer.buf.len() + w.writer.pgidx.len()) as i64 >= i64::from(self.config.pgsz) {
            self.write_flush_leaf(ctx, w)?;
        }
        // If this is to be the first rowid written to the page, set the rowid-pointer in
        // the page-header. Also append a value to the dlidx buffer, in case a
        // doclist-index is to be written for this doclist.
        if w.b_first_rowid_in_page {
            let n = w.writer.buf.len();
            put_u16(&mut w.writer.buf, 0, n as u16);
            self.write_dlidx_append(ctx, w, i_rowid)?;
        }
        // Write the rowid.
        if w.b_first_rowid_in_doclist || w.b_first_rowid_in_page {
            put_varint(&mut w.writer.buf, i_rowid as u64);
        } else {
            put_varint(
                &mut w.writer.buf,
                (i_rowid as u64).wrapping_sub(w.i_prev_rowid as u64),
            );
        }
        w.i_prev_rowid = i_rowid;
        w.b_first_rowid_in_doclist = false;
        w.b_first_rowid_in_page = false;
        Ok(())
    }

    /// `fts5WriteAppendPoslistData`.
    fn write_append_poslist_data(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        w: &mut SegWriter,
        data: &[u8],
    ) -> Result<()> {
        let pgsz = i64::from(self.config.pgsz);
        let mut a = data;
        while (w.writer.buf.len() + w.writer.pgidx.len() + a.len()) as i64 >= pgsz {
            let n_req = pgsz - w.writer.buf.len() as i64 - w.writer.pgidx.len() as i64;
            let mut n_copy: i64 = 0;
            while n_copy < n_req {
                n_copy += get_varint(a, n_copy as usize).1 as i64;
            }
            let n_copy = (n_copy.max(0) as usize).min(a.len());
            w.writer.buf.extend_from_slice(&a[..n_copy]);
            a = &a[n_copy..];
            self.write_flush_leaf(ctx, w)?;
        }
        if !a.is_empty() {
            w.writer.buf.extend_from_slice(a);
        }
        Ok(())
    }

    /// `fts5WriteFinish`: returns the number of leaf pages in the segment.
    fn write_finish(&mut self, ctx: &mut dyn VTabHostContext, w: &mut SegWriter) -> Result<i32> {
        if w.writer.buf.len() > 4 {
            self.write_flush_leaf(ctx, w)?;
        }
        let n_leaf = w.writer.pgno - 1;
        if w.writer.pgno > 1 {
            self.write_flush_btree(ctx, w)?;
        }
        Ok(n_leaf)
    }

    // -----------------------------------------------------------------------------------
    // Merging
    // -----------------------------------------------------------------------------------

    /// `fts5ChunkIterate` with `fts5MergeChunkCallback`.
    fn chunk_iterate_to_writer(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &mut SegIter,
        w: &mut SegWriter,
    ) -> Result<()> {
        let mut n_rem = i64::from(seg.n_pos);
        let Some(leaf) = seg.leaf.take() else {
            return Ok(());
        };
        let start = (seg.i_leaf_offset.max(0) as usize).min(leaf.p.len());
        let n_chunk = n_rem
            .min(i64::from(leaf.sz_leaf) - seg.i_leaf_offset)
            .max(0) as usize;
        let end = (start + n_chunk).min(leaf.p.len());
        let r = self.write_append_poslist_data(ctx, w, &leaf.p[start..end]);
        seg.leaf = Some(leaf);
        r?;
        n_rem -= n_chunk as i64;
        let mut pgno = seg.i_leaf_pgno;
        let pgno_save = pgno + 1;
        while n_rem > 0 {
            pgno += 1;
            let data = self.leaf_read(ctx, segment_rowid(seg.seg.i_segid, pgno))?;
            let n_chunk = n_rem.min(i64::from(data.sz_leaf) - 4).max(0) as usize;
            let end = (4 + n_chunk).min(data.p.len());
            self.write_append_poslist_data(ctx, w, &data.p[4..end])?;
            n_rem -= n_chunk as i64;
            if pgno == pgno_save {
                seg.next_leaf = Some(data);
            }
            if n_chunk == 0 && n_rem > 0 && pgno > seg.seg.pgno_last {
                return Err(self.corrupt_idx());
            }
        }
        Ok(())
    }

    /// `fts5TrimSegments`.
    fn trim_segments(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &mut Structure,
        mi: &mut MultiIter,
    ) -> Result<()> {
        for i in 0..mi.n_seg {
            let seg = &mi.a_seg[i];
            let Some((lv, ix)) = seg.seg_pos else {
                continue;
            };
            if seg.leaf.is_none() {
                // All keys from this input segment have been transfered to the output.
                // Set both the first and last page-numbers to 0 to indicate that the
                // segment is now empty.
                st.a_level[lv].a_seg[ix].pgno_last = 0;
                st.a_level[lv].a_seg[ix].pgno_first = 0;
                sqliters_types::dtrace!("TR|f5|trim|segid={} first=0", seg.seg.i_segid);
            } else {
                let i_off = seg.i_term_leaf_offset; // Offset on new first leaf page
                let i_id = seg.seg.i_segid;
                let i_leaf_rowid = segment_rowid(i_id, seg.i_term_leaf_pgno);
                let data = self.leaf_read(ctx, i_leaf_rowid)?;
                if i_off > data.sz_leaf {
                    // This can occur if the pages that the segments occupy overlap - if
                    // a single page has been assigned to more than one segment.
                    return Err(self.corrupt_rowid(i_leaf_rowid));
                }
                let seg = &mi.a_seg[i];
                let mut buf: Vec<u8> = Vec::with_capacity(data.nn as usize + 16);
                buf.extend_from_slice(&[0, 0, 0, 0]);
                put_varint(&mut buf, seg.term.len() as u64);
                buf.extend_from_slice(&seg.term);
                buf.extend_from_slice(&data.p[i_off as usize..data.sz_leaf as usize]);
                // Set the szLeaf field
                let n = buf.len();
                put_u16(&mut buf, 2, n as u16);
                // Set up the new page-index array
                put_varint(&mut buf, 4);
                if seg.i_leaf_pgno == seg.i_term_leaf_pgno
                    && seg.i_endof_doclist < data.sz_leaf
                    && seg.i_pgidx_off <= data.nn
                {
                    let n_diff = data.sz_leaf - seg.i_endof_doclist;
                    let v = buf.len() as i64 - 1 - i64::from(n_diff) - 4;
                    put_varint(&mut buf, v as u64);
                    buf.extend_from_slice(&data.p[seg.i_pgidx_off as usize..data.nn as usize]);
                }
                let term_pgno = seg.i_term_leaf_pgno;
                st.a_level[lv].a_seg[ix].pgno_first = term_pgno;
                sqliters_types::dtrace!("TR|f5|trim|segid={} first={}", i_id, term_pgno);
                self.data_delete(ctx, segment_rowid(i_id, 1), i_leaf_rowid)?;
                self.data_write(ctx, i_leaf_rowid, &buf)?;
            }
        }
        Ok(())
    }

    /// `fts5IndexMergeLevel`.
    fn index_merge_level(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &mut Structure,
        i_lvl: usize,
        mut pn_rem: Option<&mut i32>,
    ) -> Result<()> {
        let n_rem = pn_rem.as_deref().copied().unwrap_or(0);
        let n_input: usize;
        let mut writer: SegWriter;
        let detail = self.config.detail;
        let mut term: Vec<u8> = Vec::new();
        let mut b_term_written = false;

        if st.a_level[i_lvl].n_merge != 0 {
            n_input = st.a_level[i_lvl].n_merge as usize;
            let Some(out) = st.a_level.get(i_lvl + 1).and_then(|l| l.a_seg.last()) else {
                return Err(corrupt());
            };
            writer = self.write_init(out.i_segid);
            writer.writer.pgno = out.pgno_last + 1;
            writer.i_bt_page = 0;
            sqliters_types::dtrace!(
                "TR|f5|merge|lvl={} cont=1 nin={} out={} pgno={}",
                i_lvl,
                n_input,
                out.i_segid,
                writer.writer.pgno
            );
        } else {
            let i_segid = Self::allocate_segid(st)?;
            // Extend the Fts5Structure object as required to ensure the output segment
            // can be created.
            if i_lvl == st.a_level.len() - 1 {
                st.a_level.push(StructureLevel::default());
            }
            writer = self.write_init(i_segid);
            // Add the new segment to the output level
            let mut seg = StructureSegment {
                pgno_first: 1,
                i_segid,
                ..StructureSegment::default()
            };
            st.n_segment += 1;
            // Read input from all segments in the input level
            n_input = st.a_level[i_lvl].a_seg.len();
            // Set the range of origins that will go into the output segment.
            if st.n_origin_cntr > 0 {
                seg.i_origin1 = st.a_level[i_lvl].a_seg[0].i_origin1;
                seg.i_origin2 = st.a_level[i_lvl].a_seg[n_input - 1].i_origin2;
            }
            st.a_level[i_lvl + 1].a_seg.push(seg);
            sqliters_types::dtrace!(
                "TR|f5|merge|lvl={} cont=0 nin={} out={} pgno=1",
                i_lvl,
                n_input,
                i_segid
            );
        }
        let b_oldest = st.a_level[i_lvl + 1].a_seg.len() == 1 && st.a_level.len() == i_lvl + 2;

        let mut mi = self.multi_iter_new_level(ctx, st, i_lvl, n_input)?;
        while !mi.b_eof {
            let i_first = mi.first();
            {
                let seg_iter = &mi.a_seg[i_first];
                if seg_iter.term.as_slice() != term.as_slice() {
                    if pn_rem.is_some() && writer.n_leaf_written > n_rem {
                        break;
                    }
                    term.clear();
                    term.extend_from_slice(&seg_iter.term);
                    b_term_written = false;
                }
            }
            // Check for key annihilation.
            let (n_pos, b_del, i_rowid) = {
                let s = &mi.a_seg[i_first];
                (s.n_pos, s.b_del, s.i_rowid)
            };
            if !(n_pos == 0 && (b_oldest || !b_del)) {
                if !b_term_written {
                    // This is a new term. Append a term to the output segment.
                    self.write_append_term(ctx, &mut writer, &term)?;
                    b_term_written = true;
                }
                // Append the rowid to the output
                self.write_append_rowid(ctx, &mut writer, i_rowid)?;
                if detail == Detail::None {
                    if b_del {
                        put_varint(&mut writer.writer.buf, 0);
                        if n_pos > 0 {
                            put_varint(&mut writer.writer.buf, 0);
                        }
                    }
                } else {
                    // Append the position-list data to the output
                    let n_pos_field = i64::from(n_pos) * 2 + i64::from(b_del);
                    put_varint(&mut writer.writer.buf, n_pos_field as u64);
                    let mut seg = std::mem::take(&mut mi.a_seg[i_first]);
                    let r = self.chunk_iterate_to_writer(ctx, &mut seg, &mut writer);
                    mi.a_seg[i_first] = seg;
                    r?;
                }
            }
            self.multi_iter_next(ctx, &mut mi, false, 0)?;
        }

        // Flush the last leaf page to disk. Set the output segment b-tree height and
        // last leaf page number at the same time.
        let pgno_last = self.write_finish(ctx, &mut writer)?;
        let i_out = st.a_level[i_lvl + 1].a_seg.len() - 1;
        st.a_level[i_lvl + 1].a_seg[i_out].pgno_last = pgno_last;

        if mi.b_eof {
            // Remove the redundant segments from the %_data table
            for i in 0..n_input {
                let old = st.a_level[i_lvl].a_seg[i].clone();
                let add = old.n_entry.wrapping_sub(old.n_entry_tombstone);
                let out = &mut st.a_level[i_lvl + 1].a_seg[i_out];
                out.n_entry = out.n_entry.wrapping_add(add);
                self.data_remove_segment(ctx, &old)?;
            }
            // Remove the redundant segments from the input level
            st.a_level[i_lvl].a_seg.drain(..n_input);
            st.n_segment -= n_input as i32;
            st.a_level[i_lvl].n_merge = 0;
            if pgno_last == 0 {
                st.a_level[i_lvl + 1].a_seg.pop();
                st.n_segment -= 1;
            }
            sqliters_types::dtrace!(
                "TR|f5|merged|lvl={} done=1 leaves={} last={}",
                i_lvl,
                writer.n_leaf_written,
                pgno_last
            );
        } else {
            self.trim_segments(ctx, st, &mut mi)?;
            st.a_level[i_lvl].n_merge = n_input as i32;
            sqliters_types::dtrace!(
                "TR|f5|merged|lvl={} done=0 leaves={} last={}",
                i_lvl,
                writer.n_leaf_written,
                pgno_last
            );
        }
        if let Some(r) = pn_rem.as_deref_mut() {
            *r -= writer.n_leaf_written;
        }
        Ok(())
    }

    /// `fts5IndexFindDeleteMerge`.
    fn index_find_delete_merge(&self, st: &Structure) -> i32 {
        let mut i_ret = -1;
        if self.config.contentless_delete && self.config.n_delete_merge > 0 {
            let mut n_best: i64 = 0;
            for (ii, lvl) in st.a_level.iter().enumerate() {
                let mut n_entry: i64 = 0;
                let mut n_tomb: i64 = 0;
                for seg in &lvl.a_seg {
                    n_entry = n_entry.wrapping_add(seg.n_entry as i64);
                    n_tomb = n_tomb.wrapping_add(seg.n_entry_tombstone as i64);
                }
                if n_entry > 0 {
                    let n_percent = (n_tomb.wrapping_mul(100) / n_entry) as i32;
                    if n_percent >= self.config.n_delete_merge && i64::from(n_percent) > n_best {
                        i_ret = ii as i32;
                        n_best = i64::from(n_percent);
                    }
                }
                // If pLvl is already the input level to an ongoing merge, look no
                // further for a merge candidate. The caller should be allowed to
                // continue merging from pLvl first.
                if lvl.n_merge != 0 {
                    break;
                }
            }
        }
        i_ret
    }

    /// `fts5IndexMerge`.
    fn index_merge(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &mut Structure,
        n_pg: i32,
        n_min: i32,
    ) -> Result<bool> {
        let mut n_rem = n_pg;
        let mut b_ret = false;
        let mut n_min = n_min;
        while n_rem > 0 {
            let mut i_best_lvl: i32 = 0; // Level offering the most input segments
            let mut n_best: i32 = 0; // Number of input segments on best level

            // Set iBestLvl to the level to read input segments from. Or to -1 if there
            // is no level suitable for merging.
            for (i_lvl, lvl) in st.a_level.iter().enumerate() {
                if lvl.n_merge != 0 {
                    if lvl.n_merge > n_best {
                        i_best_lvl = i_lvl as i32;
                        n_best = n_min;
                    }
                    break;
                }
                if lvl.a_seg.len() as i32 > n_best {
                    n_best = lvl.a_seg.len() as i32;
                    i_best_lvl = i_lvl as i32;
                }
            }
            if n_best < n_min {
                i_best_lvl = self.index_find_delete_merge(st);
            }
            sqliters_types::dtrace!(
                "TR|f5|mergesel|rem={} min={} best={} lvl={}",
                n_rem,
                n_min,
                n_best,
                i_best_lvl
            );
            if i_best_lvl < 0 {
                break;
            }
            b_ret = true;
            self.index_merge_level(ctx, st, i_best_lvl as usize, Some(&mut n_rem))?;
            if st.a_level[i_best_lvl as usize].n_merge == 0 {
                Self::structure_promote(i_best_lvl as usize + 1, st);
            }
            if n_min == 1 {
                n_min = 2;
            }
        }
        Ok(b_ret)
    }

    /// `fts5IndexAutomerge`.
    fn index_automerge(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &mut Structure,
        n_leaf: i32,
    ) -> Result<()> {
        if self.config.n_automerge > 0 {
            // Update the write-counter. While doing so, set nWork.
            let n_write = st.n_write_counter;
            let unit = self.n_work_unit as u64;
            let n_work =
                ((n_write.wrapping_add(n_leaf as i64 as u64) / unit) - (n_write / unit)) as i32;
            st.n_write_counter = st.n_write_counter.wrapping_add(n_leaf as i64 as u64);
            let n_rem =
                (i64::from(self.n_work_unit) * i64::from(n_work) * st.a_level.len() as i64) as i32;
            sqliters_types::dtrace!(
                "TR|f5|automerge|leaf={} wc={} work={} rem={}",
                n_leaf,
                n_write,
                n_work,
                n_rem
            );
            let n_min = self.config.n_automerge;
            self.index_merge(ctx, st, n_rem, n_min)?;
        }
        Ok(())
    }

    /// `fts5IndexCrisismerge`.
    fn index_crisismerge(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &mut Structure,
    ) -> Result<()> {
        let n_crisis = self.config.n_crisis_merge;
        if !st.a_level.is_empty() {
            let mut i_lvl = 0usize;
            while i_lvl < st.a_level.len() && st.a_level[i_lvl].a_seg.len() as i32 >= n_crisis {
                sqliters_types::dtrace!(
                    "TR|f5|crisis|lvl={} nseg={}",
                    i_lvl,
                    st.a_level[i_lvl].a_seg.len()
                );
                self.index_merge_level(ctx, st, i_lvl, None)?;
                Self::structure_promote(i_lvl + 1, st);
                i_lvl += 1;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Secure delete
// ---------------------------------------------------------------------------------------

impl Fts5Index {
    /// `fts5SecureDeleteIdxEntry`.
    fn secure_delete_idx_entry(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_segid: i32,
        i_pgno: i32,
    ) -> Result<()> {
        if i_pgno != 1 {
            sqliters_types::dtrace!("TR|f5|d|tbl=idx segid={} pgno={}", i_segid, i_pgno);
            self.idx_cache.remove(&i_segid);
            ctx.execute_sql_counted(
                &self.sql_idx_secure_delete,
                &[
                    Value::Integer(i64::from(i_segid)),
                    Value::Integer(i64::from(i_pgno)),
                ],
            )
            .map_err(rc_only)?;
        }
        Ok(())
    }

    /// `fts5SecureDeleteOverflow`: returns `*pbLastInDoclist`.
    fn secure_delete_overflow(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &StructureSegment,
        i_pgno: i32,
    ) -> Result<bool> {
        let b_detail_none = self.config.detail == Detail::None;
        let mut b_last_in_doclist = true;
        let mut pgno = i_pgno;
        while pgno <= seg.pgno_last {
            let i_rowid = segment_rowid(seg.i_segid, pgno);
            let mut leaf = self.leaf_read(ctx, i_rowid)?;
            let mut i_next = get_u16(&leaf.p, 0);
            if i_next != 0 {
                b_last_in_doclist = false;
            }
            if i_next == 0 && leaf.sz_leaf < leaf.nn {
                i_next = get_varint32(&leaf.p, leaf.sz_leaf as usize).0;
            }
            if i_next == 0 {
                // The page contains no terms or rowids. Replace it with an empty page
                // and move on to the right-hand peer.
                let a_empty = [0x00u8, 0x00, 0x00, 0x04];
                sqliters_types::dtrace!("TR|f5|sdovfl|pgno={} o=empty", pgno);
                if !b_detail_none {
                    self.data_write(ctx, i_rowid, &a_empty)?;
                }
            } else if b_detail_none {
                sqliters_types::dtrace!("TR|f5|sdovfl|pgno={} o=none-stop", pgno);
                break;
            } else if i_next >= leaf.sz_leaf || leaf.nn < leaf.sz_leaf || i_next < 4 {
                return Err(self.corrupt_rowid(i_rowid));
            } else {
                let n_shift = i_next - 4;
                let mut a_idx: Vec<u8> = Vec::new();
                // Unless the current page footer is 0 bytes in size (in which case the
                // new page footer will be as well), allocate and populate a buffer
                // containing the new page footer. Set stack variables aIdx and nIdx
                // accordingly.
                if leaf.nn > leaf.sz_leaf {
                    let mut i1 = leaf.sz_leaf;
                    let (i_first, nb) = get_varint32(&leaf.p, i1 as usize);
                    i1 += nb as i32;
                    if i_first < i_next {
                        return Err(self.corrupt_rowid(i_rowid));
                    }
                    put_varint(&mut a_idx, (i_first - n_shift) as u64);
                    if i1 < leaf.nn {
                        a_idx.extend_from_slice(&leaf.p[i1 as usize..leaf.nn as usize]);
                    }
                }
                // Modify the contents of buffer aPg[]. Set nPg to the new size in bytes.
                // The new page is always smaller than the old.
                let mut n_pg = leaf.sz_leaf - n_shift;
                leaf.p
                    .copy_within((4 + n_shift) as usize..leaf.sz_leaf as usize, 4);
                put_u16(&mut leaf.p, 2, n_pg as u16);
                if get_u16(&leaf.p, 0) != 0 {
                    put_u16(&mut leaf.p, 0, 4);
                }
                if !a_idx.is_empty() {
                    let dst = n_pg as usize;
                    leaf.p[dst..dst + a_idx.len()].copy_from_slice(&a_idx);
                    n_pg += a_idx.len() as i32;
                }
                sqliters_types::dtrace!("TR|f5|sdovfl|pgno={} o=shift n={}", pgno, n_shift);
                // Write the new page to disk and exit the loop
                let page = leaf.p[..n_pg as usize].to_vec();
                self.data_write(ctx, i_rowid, &page)?;
                break;
            }
            pgno += 1;
        }
        Ok(b_last_in_doclist)
    }

    /// `fts5DoSecureDelete`.
    #[allow(clippy::too_many_lines)]
    fn do_secure_delete(&mut self, ctx: &mut dyn VTabHostContext, seg: &mut SegIter) -> Result<()> {
        let b_detail_none = self.config.detail == Detail::None;
        let i_segid = seg.seg.i_segid;
        let Some(leaf) = seg.leaf.as_ref() else {
            return Ok(());
        };
        let n_pg = leaf.nn;
        let mut i_pg_idx = leaf.sz_leaf; // Offset of page footer
        let mut i_delta: u64;
        let mut i_next_off: i32;
        let mut i_off: i32;
        let n_idx = n_pg - i_pg_idx;
        let mut b_last_in_doclist = false;
        let mut i_start: i32;
        let mut i_del_key_off: i32 = 0; // Offset of deleted key, if any

        // A working copy of the page with slack after it (C edits the page in place in a
        // buffer that has FTS5_DATA_PADDING bytes after the data).
        let mut a_pg: Vec<u8> = leaf.p.clone();
        a_pg.resize(n_pg as usize + FTS5_DATA_PADDING + 16, 0);
        let mut a_idx: Vec<u8> = a_pg[i_pg_idx as usize..n_pg as usize].to_vec();
        a_idx.resize(n_idx as usize + 16, 0);

        // At this point segment iterator pSeg points to the entry this function should
        // remove from the b-tree segment.
        //
        // Set iStart to the offset of the rowid (or rowid delta) of the entry, iNextOff
        // to the offset of the next entry / term / page end, iDelta to the rowid delta.
        {
            if seg.i_leaf_pgno == seg.i_term_leaf_pgno {
                i_start = seg.i_term_leaf_offset;
            } else {
                i_start = get_u16(&a_pg, 0);
            }
            if i_start > n_pg {
                return Err(self.corrupt_idx());
            }
            let (d, nb) = get_varint(&a_pg, i_start as usize);
            i_delta = d;
            let mut i_sop = i_start + nb as i32;
            if b_detail_none {
                while i64::from(i_sop) < seg.i_leaf_offset {
                    if byte_at(&a_pg, i_sop as usize) == 0x00 {
                        i_sop += 1;
                    }
                    if byte_at(&a_pg, i_sop as usize) == 0x00 {
                        i_sop += 1;
                    }
                    i_start = i_sop;
                    let (d, nb) = get_varint(&a_pg, i_start as usize);
                    i_delta = d;
                    i_sop = i_start + nb as i32;
                }
                i_next_off = i_sop;
                if i_next_off < seg.i_endof_doclist && byte_at(&a_pg, i_next_off as usize) == 0x00 {
                    i_next_off += 1;
                }
                if i_next_off < seg.i_endof_doclist && byte_at(&a_pg, i_next_off as usize) == 0x00 {
                    i_next_off += 1;
                }
            } else {
                let (mut n_pos, nb) = get_varint32(&a_pg, i_sop as usize);
                i_sop += nb as i32;
                while i64::from(i_sop) < seg.i_leaf_offset {
                    i_start = i_sop + (n_pos / 2);
                    let (d, nb) = get_varint(&a_pg, i_start as usize);
                    i_delta = d;
                    i_sop = i_start + nb as i32;
                    let (np, nb) = get_varint32(&a_pg, i_sop as usize);
                    n_pos = np;
                    i_sop += nb as i32;
                }
                i_next_off = i_sop + seg.n_pos;
            }
        }

        i_off = i_start;

        // If the position-list for the entry being removed flows over past the end of
        // this page, delete the portion of the position-list on the next page and
        // beyond.
        //
        // Set variable bLastInDoclist to true if this entry happens to be the last
        // rowid in the doclist for its term.
        if i_next_off >= i_pg_idx {
            let pgno = seg.i_leaf_pgno + 1;
            let seg_rec = seg.seg.clone();
            b_last_in_doclist = self.secure_delete_overflow(ctx, &seg_rec, pgno)?;
            i_next_off = i_pg_idx;
        }

        if !seg.b_del {
            if i_next_off != i_pg_idx {
                // Loop through the page-footer. If iNextOff (offset of the entry
                // following the one we are removing) is equal to the offset of a key on
                // this page, then the entry is the last in its doclist.
                let mut i_key_off: i32 = 0;
                let mut i_idx = 0;
                while i_idx < n_idx {
                    let (i_val, nb) = get_varint32(&a_idx, i_idx as usize);
                    i_idx += nb as i32;
                    i_key_off = i_key_off.wrapping_add(i_val);
                    if i_key_off == i_next_off {
                        b_last_in_doclist = true;
                    }
                }
            }

            // If this is (a) the first rowid on a page and (b) is not followed by
            // another position list on the same page, set the "first-rowid" field of the
            // header to 0.
            if get_u16(&a_pg, 0) == i_start && (b_last_in_doclist || i_next_off == i_pg_idx) {
                put_u16(&mut a_pg, 0, 0);
            }
        }

        sqliters_types::dtrace!(
            "TR|f5|sd|segid={} pgno={} start={} next={} bdel={} last={} pgidx={}",
            i_segid,
            seg.i_leaf_pgno,
            i_start,
            i_next_off,
            u8::from(seg.b_del),
            u8::from(b_last_in_doclist),
            i_pg_idx
        );

        if seg.b_del {
            i_off += put_varint_at(&mut a_pg, i_off as usize, i_delta) as i32;
            if (i_off as usize) < a_pg.len() {
                a_pg[i_off as usize] = 0x01;
            }
            i_off += 1;
        } else if !b_last_in_doclist {
            if i_next_off != i_pg_idx {
                let (i_next_delta, nb) = get_varint(&a_pg, i_next_off as usize);
                i_next_off += nb as i32;
                i_off += put_varint_at(
                    &mut a_pg,
                    i_off as usize,
                    i_delta.wrapping_add(i_next_delta),
                ) as i32;
            }
        } else if seg.i_leaf_pgno == seg.i_term_leaf_pgno && i_start == seg.i_term_leaf_offset {
            // The entry being removed was the only position list in its doclist.
            // Therefore the term needs to be removed as well.
            let mut i_key = 0;
            let mut i_key_off: i32 = 0;

            // Set iKeyOff to the offset of the term that will be removed - the last
            // offset in the footer that is not greater than iStart.
            let mut i_idx = 0;
            while i_idx < n_idx {
                let (i_val, nb) = get_varint32(&a_idx, i_idx as usize);
                i_idx += nb as i32;
                if (i_key_off as u32).wrapping_add(i_val as u32) > i_start as u32 {
                    break;
                }
                i_key_off = i_key_off.wrapping_add(i_val);
                i_key += 1;
            }

            // Set iDelKeyOff to the value of the footer entry to remove from the page.
            i_del_key_off = i_key_off;
            i_off = i_key_off;

            if i_next_off != i_pg_idx {
                // This is the only position-list associated with the term, and there is
                // another term following it on this page. So the following term needs
                // to be moved to replace the term associated with the entry being
                // removed.
                i_del_key_off = i_next_off;
                let (n_prefix2, nb) = get_varint(&a_pg, i_next_off as usize);
                i_next_off += nb as i32;
                let (n_suffix2, nb) = get_varint(&a_pg, i_next_off as usize);
                i_next_off += nb as i32;
                let mut n_prefix: u64 = 0;
                if i_key != 1 {
                    let (v, nb) = get_varint(&a_pg, i_key_off as usize);
                    n_prefix = v;
                    i_key_off += nb as i32;
                }
                let (n_suffix_old, nb) = get_varint(&a_pg, i_key_off as usize);
                i_key_off += nb as i32;

                n_prefix = n_prefix.min(n_prefix2);
                let n_suffix = n_prefix2.wrapping_add(n_suffix2).wrapping_sub(n_prefix);

                if (i_key_off as u64).wrapping_add(n_suffix) > i_pg_idx as u64
                    || (i_next_off as u64).wrapping_add(n_suffix2) > i_pg_idx as u64
                {
                    let _ = n_suffix_old;
                    return Err(self.corrupt_idx());
                }
                if i_key != 1 {
                    i_off += put_varint_at(&mut a_pg, i_off as usize, n_prefix) as i32;
                }
                i_off += put_varint_at(&mut a_pg, i_off as usize, n_suffix) as i32;
                if n_prefix2 > seg.term.len() as u64 {
                    return Err(self.corrupt_idx());
                } else if n_prefix2 > n_prefix {
                    let src = &seg.term[n_prefix as usize..n_prefix2 as usize];
                    let dst = i_off as usize;
                    a_pg[dst..dst + src.len()].copy_from_slice(src);
                    i_off += (n_prefix2 - n_prefix) as i32;
                }
                a_pg.copy_within(
                    i_next_off as usize..(i_next_off as u64 + n_suffix2) as usize,
                    i_off as usize,
                );
                i_off += n_suffix2 as i32;
                i_next_off += n_suffix2 as i32;
            }
        } else if i_start == 4 {
            // The entry being removed may be the only position list in its doclist.
            let mut i_pgno = seg.i_leaf_pgno - 1;
            while i_pgno > seg.i_term_leaf_pgno {
                let b_empty = match self.leaf_read(ctx, segment_rowid(i_segid, i_pgno)) {
                    Ok(pg) => pg.nn == 4,
                    Err(e) => return Err(e),
                };
                if !b_empty {
                    break;
                }
                i_pgno -= 1;
            }

            if i_pgno == seg.i_term_leaf_pgno {
                let i_id = segment_rowid(i_segid, seg.i_term_leaf_pgno);
                let mut p_term = self.leaf_read(ctx, i_id)?;
                if p_term.sz_leaf == seg.i_term_leaf_offset {
                    let base = p_term.sz_leaf as usize;
                    let mut n_term_idx = p_term.nn - p_term.sz_leaf;
                    let mut i_term_idx: i32 = 0;
                    let mut i_term_off: i64 = 0;
                    loop {
                        let (i_val, n_byte) = get_varint32(&p_term.p, base + i_term_idx as usize);
                        i_term_off += i64::from(i_val as u32);
                        if (i_term_idx + n_byte as i32) >= n_term_idx {
                            break;
                        }
                        i_term_idx += n_byte as i32;
                    }
                    n_term_idx = i_term_idx;

                    if i_term_off > i64::from(p_term.sz_leaf) {
                        return Err(self.corrupt_idx());
                    }
                    p_term
                        .p
                        .copy_within(base..base + n_term_idx as usize, i_term_off as usize);
                    put_u16(&mut p_term.p, 2, i_term_off as u16);
                    let page = p_term.p[..(i_term_off + i64::from(n_term_idx)) as usize].to_vec();
                    sqliters_types::dtrace!(
                        "TR|f5|sdterm|pgno={} off={} nidx={}",
                        seg.i_term_leaf_pgno,
                        i_term_off,
                        n_term_idx
                    );
                    self.data_write(ctx, i_id, &page)?;
                    if n_term_idx == 0 {
                        self.secure_delete_idx_entry(ctx, i_segid, seg.i_term_leaf_pgno)?;
                    }
                }
            }
        }

        // Assuming no error has occurred, this block does final edits to the leaf page
        // before writing it back to disk. Input variables are:
        //
        //   nPg: Total initial size of leaf page.
        //   iPgIdx: Initial offset of page footer.
        //
        //   iOff: Offset to move data to
        //   iNextOff: Offset to move data from
        {
            let n_move = n_pg - i_next_off; // Number of bytes to move
            let n_shift = i_next_off - i_off; // Distance to move them
            let mut i_prev_key_out: i32 = 0;
            let mut i_key_in: i64 = 0;

            if n_move > 0 {
                a_pg.copy_within(
                    i_next_off as usize..(i_next_off + n_move) as usize,
                    i_off as usize,
                );
            }
            i_pg_idx -= n_shift;
            let mut n_new_pg = i_pg_idx;
            put_u16(&mut a_pg, 2, i_pg_idx as u16);

            let mut i_idx = 0;
            while i_idx < n_idx {
                let (i_val, nb) = get_varint32(&a_idx, i_idx as usize);
                i_idx += nb as i32;
                i_key_in += i64::from(i_val as u32);
                if i_key_in > i64::from(n_pg) {
                    return Err(self.corrupt_idx());
                }
                if i_key_in != i64::from(i_del_key_off) {
                    let i_key_out = (i_key_in
                        - if i_key_in > i64::from(i_off) {
                            i64::from(n_shift)
                        } else {
                            0
                        }) as i32;
                    n_new_pg += put_varint_at(
                        &mut a_pg,
                        n_new_pg as usize,
                        i64::from(i_key_out - i_prev_key_out) as u64,
                    ) as i32;
                    i_prev_key_out = i_key_out;
                }
            }

            if i_pg_idx == n_new_pg && n_idx > 0 && seg.i_leaf_pgno != 1 {
                self.secure_delete_idx_entry(ctx, i_segid, seg.i_leaf_pgno)?;
            }

            let n_new = (n_new_pg.max(0) as usize).min(a_pg.len());
            let page = a_pg[..n_new].to_vec();
            self.data_write(ctx, segment_rowid(i_segid, seg.i_leaf_pgno), &page)?;
        }
        Ok(())
    }

    /// `fts5FlushSecureDelete`.
    fn flush_secure_delete(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &Structure,
        term: &[u8],
        i_rowid: i64,
    ) -> Result<()> {
        // If the version number has not been set to SECUREDELETE, do so now.
        if self.config.i_version != FTS5_CURRENT_VERSION_SECUREDELETE {
            ctx.execute_sql_counted(&self.sql_config_version, &[])
                .map_err(rc_only)?;
            sqliters_types::dtrace!("TR|f5|w|tbl=config k=version v=5");
            self.config.i_cookie = self.config.i_cookie.wrapping_add(1);
            self.config.i_version = FTS5_CURRENT_VERSION_SECUREDELETE;
        }

        let mut mi = self.multi_iter_new_term(ctx, st, term)?;
        if !mi.b_eof {
            let i_this = mi.a_seg[mi.first()].i_rowid;
            if i_this < i_rowid {
                self.multi_iter_next_from(ctx, &mut mi, i_rowid)?;
            }
            if !mi.b_eof && i_rowid == mi.a_seg[mi.first()].i_rowid {
                let i_first = mi.first();
                let mut seg = std::mem::take(&mut mi.a_seg[i_first]);
                let r = self.do_secure_delete(ctx, &mut seg);
                mi.a_seg[i_first] = seg;
                r?;
            } else {
                sqliters_types::dtrace!("TR|f5|sd|miss rowid={}", i_rowid);
            }
        } else {
            sqliters_types::dtrace!("TR|f5|sd|miss rowid={}", i_rowid);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Flush
    // -----------------------------------------------------------------------------------

    /// `fts5FlushOneHash`.
    #[allow(clippy::too_many_lines)]
    fn flush_one_hash(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        entries: &[(Vec<u8>, HashEntry)],
    ) -> Result<()> {
        let mut pgno_last: i32 = 0; // Last leaf page number in segment

        // Obtain a reference to the index structure and allocate a new segment-id for
        // the new level-0 segment.
        let mut st = self.structure_take(ctx)?;

        if !entries.is_empty() {
            let i_segid = Self::allocate_segid(&st)?;
            let pgsz = i64::from(self.config.pgsz);
            let detail = self.config.detail;
            let b_secure_delete = self.config.secure_delete;
            let mut writer = self.write_init(i_segid);

            // Begin scanning through hash table entries. This loop runs once for each
            // term/doclist currently stored within the hash table.
            sqliters_types::dtrace!(
                "TR|f5|flush|segid={} terms={} sd={} pgsz={} pend={} rows={}",
                i_segid,
                entries.len(),
                u8::from(b_secure_delete),
                pgsz,
                self.n_pending_data,
                self.n_pending_row
            );
            for (z_term, entry) in entries {
                let doclist: &[u8] = &entry.data;
                let n_doclist = doclist.len();
                if !b_secure_delete {
                    self.write_append_term(ctx, &mut writer, z_term)?;
                }

                if !b_secure_delete
                    && pgsz
                        >= (writer.writer.buf.len() + writer.writer.pgidx.len() + n_doclist + 1)
                            as i64
                {
                    // The entire doclist will fit on the current leaf.
                    writer.writer.buf.extend_from_slice(doclist);
                } else {
                    let mut b_term_written = !b_secure_delete;
                    let mut i_rowid: i64 = 0;
                    let mut i_prev: i64 = 0;
                    let mut i_off: usize = 0;

                    // The entire doclist will not fit on this leaf. The following loop
                    // iterates through the poslists that make up the current doclist.
                    while i_off < n_doclist {
                        let (i_delta, nb) = get_varint(doclist, i_off);
                        i_off += nb;
                        i_rowid = i_rowid.wrapping_add(i_delta as i64);

                        // If in secure delete mode, and if this entry in the poslist is
                        // in fact a delete, then edit the existing segments directly
                        // using fts5FlushSecureDelete().
                        if b_secure_delete {
                            if detail == Detail::None {
                                if i_off < n_doclist && doclist[i_off] == 0x00 {
                                    self.flush_secure_delete(ctx, &st, z_term, i_rowid)?;
                                    i_off += 1;
                                    if i_off < n_doclist && doclist[i_off] == 0x00 {
                                        i_off += 1;
                                    } else {
                                        continue;
                                    }
                                }
                            } else if (byte_at(doclist, i_off) & 0x01) != 0 {
                                self.flush_secure_delete(ctx, &st, z_term, i_rowid)?;
                                if byte_at(doclist, i_off) == 0x01 {
                                    i_off += 1;
                                    continue;
                                }
                            }
                        }

                        if !b_term_written {
                            self.write_append_term(ctx, &mut writer, z_term)?;
                            b_term_written = true;
                        }

                        if writer.b_first_rowid_in_page {
                            let n = writer.writer.buf.len();
                            put_u16(&mut writer.writer.buf, 0, n as u16); // first rowid on page
                            put_varint(&mut writer.writer.buf, i_rowid as u64);
                            writer.b_first_rowid_in_page = false;
                            self.write_dlidx_append(ctx, &mut writer, i_rowid)?;
                        } else {
                            let d = (i_rowid as u64).wrapping_sub(i_prev as u64);
                            put_varint(&mut writer.writer.buf, d);
                        }
                        i_prev = i_rowid;

                        if detail == Detail::None {
                            if i_off < n_doclist && doclist[i_off] == 0 {
                                writer.writer.buf.push(0);
                                i_off += 1;
                                if i_off < n_doclist && doclist[i_off] == 0 {
                                    writer.writer.buf.push(0);
                                    i_off += 1;
                                }
                            }
                            if (writer.writer.buf.len() + writer.writer.pgidx.len()) as i64 >= pgsz
                            {
                                self.write_flush_leaf(ctx, &mut writer)?;
                            }
                        } else {
                            // fts5GetPoslistSize
                            let (n_sz, n_hdr) = get_varint32(doclist, i_off);
                            let n_pos = n_sz / 2;
                            let b_del = (n_sz & 1) != 0;
                            let mut n_copy = n_hdr as i64;
                            if b_del && b_secure_delete {
                                put_varint(&mut writer.writer.buf, (i64::from(n_pos) * 2) as u64);
                                i_off += n_copy as usize;
                                n_copy = i64::from(n_pos);
                            } else {
                                n_copy += i64::from(n_pos);
                            }
                            let n_copy_us = n_copy.max(0) as usize;
                            let end = (i_off + n_copy_us).min(n_doclist);
                            let poslist = &doclist[i_off.min(n_doclist)..end];
                            if (writer.writer.buf.len() + writer.writer.pgidx.len()) as i64 + n_copy
                                <= pgsz
                            {
                                // The entire poslist will fit on the current leaf. So
                                // copy it in one go.
                                writer.writer.buf.extend_from_slice(poslist);
                            } else {
                                // The entire poslist will not fit on this leaf. So it
                                // needs to be broken into sections. The only qualification
                                // being that each varint must be stored contiguously.
                                let mut i_pos: i64 = 0;
                                loop {
                                    let n_space = pgsz
                                        - writer.writer.buf.len() as i64
                                        - writer.writer.pgidx.len() as i64;
                                    let n: i64 = if (n_copy - i_pos) <= n_space {
                                        n_copy - i_pos
                                    } else {
                                        let from = (i_pos.max(0) as usize).min(poslist.len());
                                        i64::from(poslist_prefix(&poslist[from..], n_space as i32))
                                    };
                                    let from = (i_pos.max(0) as usize).min(poslist.len());
                                    let to = ((i_pos + n).max(0) as usize).min(poslist.len());
                                    writer.writer.buf.extend_from_slice(&poslist[from..to]);
                                    i_pos += n;
                                    if (writer.writer.buf.len() + writer.writer.pgidx.len()) as i64
                                        >= pgsz
                                    {
                                        self.write_flush_leaf(ctx, &mut writer)?;
                                    }
                                    if i_pos >= n_copy {
                                        break;
                                    }
                                }
                            }
                            i_off += n_copy_us;
                        }
                    }
                }
                // TODO2: Doclist terminator written here.
            }
            pgno_last = self.write_finish(ctx, &mut writer)?;

            if pgno_last > 0 {
                // Update the Fts5Structure. It is written back to the database by the
                // fts5StructureRelease() call below.
                if st.a_level.is_empty() {
                    st.a_level.push(StructureLevel::default());
                }
                let mut seg = StructureSegment {
                    i_segid,
                    pgno_first: 1,
                    pgno_last,
                    ..StructureSegment::default()
                };
                if st.n_origin_cntr > 0 {
                    seg.i_origin1 = st.n_origin_cntr;
                    seg.i_origin2 = st.n_origin_cntr;
                    seg.n_entry = self.n_pending_row as i64 as u64;
                    st.n_origin_cntr += 1;
                }
                st.a_level[0].a_seg.push(seg);
                st.n_segment += 1;
                Self::structure_promote(0, &mut st);
            }
            sqliters_types::dtrace!("TR|f5|flushed|segid={} last={}", i_segid, pgno_last);
        }

        self.index_automerge(ctx, &mut st, pgno_last + self.n_contentless_delete)?;
        self.index_crisismerge(ctx, &mut st)?;
        self.structure_write(ctx, &st)?;
        Ok(())
    }

    /// `fts5IndexFlush`.
    fn index_flush(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        // Unless it is empty, flush the hash table to disk
        if let Some(e) = &self.flush_rc {
            return Err(e.clone());
        }
        if self.n_pending_data != 0 || self.n_contentless_delete != 0 {
            let mut hash = match self.hash.take() {
                Some(h) => h,
                None => Fts5Hash::new(self.config.detail),
            };
            let entries = hash.take_sorted();
            self.idx_cache.clear();
            self.idx_cache_on = true;
            let res = self.flush_one_hash(ctx, &entries);
            self.idx_cache_on = false;
            self.idx_cache.clear();
            match res {
                Ok(()) => {
                    drop(entries);
                    self.hash = Some(hash);
                    self.n_pending_data = 0;
                    self.n_pending_row = 0;
                    self.n_contentless_delete = 0;
                }
                Err(e) => {
                    hash.restore(entries);
                    self.hash = Some(hash);
                    self.flush_rc = Some(e.clone());
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// `fts5IndexOptimizeStruct`: `None` when there is nothing to optimise.
    fn index_optimize_struct(st: &Structure) -> Option<Structure> {
        let n_seg = st.n_segment;
        // Figure out if this structure requires optimization. A structure does not
        // require optimization if either:
        //
        //  1. it consists of fewer than two segments, or
        //  2. all segments are on the same level, or
        //  3. all segments except one are currently inputs to a merge operation.
        //
        // In the first case, if there are no tombstone hash pages, return NULL. In the
        // second, increment the ref-count on *pStruct and return a copy of the pointer
        // to it.
        if n_seg == 0 {
            return None;
        }
        for lvl in &st.a_level {
            let n_this = lvl.a_seg.len() as i32;
            let n_merge = lvl.n_merge;
            if n_this > 0 && (n_this == n_seg || (n_this == n_seg - 1 && n_merge == n_this)) {
                if n_seg == 1 && n_this == 1 && lvl.a_seg[0].n_pg_tombstone == 0 {
                    return None;
                }
                return Some(st.clone());
            }
        }
        let n_level = (st.a_level.len() + 1).min(FTS5_MAX_LEVEL);
        let mut new = Structure {
            n_write_counter: st.n_write_counter,
            n_origin_cntr: st.n_origin_cntr,
            n_segment: n_seg,
            a_level: vec![StructureLevel::default(); n_level],
        };
        // Iterate through all segments, from oldest to newest. Add them to the new
        // Fts5Level object so that pLvl->aSeg[0] is the oldest segment in the usual
        // sense - with the oldest data.
        let mut segs = Vec::with_capacity(n_seg.max(0) as usize);
        for lvl in st.a_level.iter().rev() {
            for seg in &lvl.a_seg {
                segs.push(seg.clone());
            }
        }
        new.a_level[n_level - 1].a_seg = segs;
        Some(new)
    }

    // -----------------------------------------------------------------------------------
    // Tombstones (contentless_delete=1)
    // -----------------------------------------------------------------------------------

    /// `fts5IndexTombstoneRehash`.
    #[allow(clippy::too_many_arguments)]
    fn tombstone_rehash(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &StructureSegment,
        data1: Option<&Data>,
        i_pg1: i32,
        sz_key: usize,
        ap_out: &mut [Data],
    ) -> Result<i32> {
        let n_out = ap_out.len() as i32;
        let mut res = 0;
        // Initialize the headers of all the output pages
        for out in ap_out.iter_mut() {
            out.p[0] = sz_key as u8;
            put_u32(&mut out.p, 4, 0);
        }
        // Loop through the current pages of the hash table.
        let mut ii = 0;
        while res == 0 && ii < seg.n_pg_tombstone {
            let loaded;
            let data: Option<&Data> = if i_pg1 == ii {
                data1
            } else {
                loaded = self.data_read(ctx, tombstone_rowid(seg.i_segid, ii))?;
                Some(&loaded)
            };
            if let Some(data) = data {
                let sz_key_in: usize = if byte_at(&data.p, 0) == 4 { 4 } else { 8 };
                let n_slot_in = (i64::from(data.nn) - 8) / sz_key_in as i64;
                for i_in in 0..n_slot_in.max(0) as usize {
                    // Read the value from slot iIn of the input page into iVal.
                    let off = 8 + i_in * sz_key_in;
                    let i_val: u64 = if sz_key_in == 4 {
                        u64::from(get_u32(&data.p, off))
                    } else {
                        get_u64(&data.p, off)
                    };
                    // If iVal is not 0 at this point, insert it into the new hash table
                    if i_val != 0 {
                        let pg = &mut ap_out[(i_val % n_out as u64) as usize];
                        res = tombstone_add_to_page(pg, false, n_out, i_val);
                        if res != 0 {
                            break;
                        }
                    }
                }
                // If this is page 0 of the old hash, copy the rowid-0-flag from the old
                // hash to the new.
                if ii == 0 {
                    ap_out[0].p[1] = byte_at(&data.p, 1);
                }
            }
            ii += 1;
        }
        Ok(res)
    }

    /// `fts5IndexTombstoneRebuild`: the output pages.
    fn tombstone_rebuild(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &StructureSegment,
        data1: Option<&Data>,
        i_pg1: i32,
        sz_key: usize,
    ) -> Result<Vec<Data>> {
        const MINSLOT: i64 = 32;
        let n_slot_per_page = MINSLOT.max((i64::from(self.config.pgsz) - 8) / sz_key as i64);
        let mut n_slot: i64 = 0; // Number of slots in each output page
        let mut n_out: i64 = 0;

        // Figure out how many output pages (nOut) and how many slots per page (nSlot).
        // There are three possibilities:
        //
        //   1. The hash table does not yet exist. In this case the new hash table will
        //      consist of a single page with MINSLOT slots.
        //
        //   2. The hash table exists but is currently a single page. In this case an
        //      attempt is made to grow the page to accommodate the new entry. The page
        //      is allowed to grow up to nSlotPerPage (see above) slots.
        //
        //   3. The hash table already consists of more than one page, or of a single
        //      page already so large that it cannot be grown. In this case the new hash
        //      consists of (nPg*2+1) pages of nSlotPerPage slots each.
        if seg.n_pg_tombstone == 0 {
            // Case 1.
            n_out = 1;
            n_slot = MINSLOT;
        } else if seg.n_pg_tombstone == 1 {
            // Case 2.
            let n_elem = data1.map_or(0, |d| get_u32(&d.p, 4));
            if i64::from(n_elem) > n_slot_per_page / 4 {
                n_out = 0;
            } else {
                n_out = 1;
                n_slot = (i64::from(n_elem) * 4).max(MINSLOT);
            }
        }
        if n_out == 0 {
            // Case 3.
            n_out = i64::from(seg.n_pg_tombstone) * 2 + 1;
            n_slot = n_slot_per_page;
        }

        // Allocate the required array and output pages
        loop {
            let sz_page = (8 + n_slot * sz_key as i64) as usize;
            let mut ap_out: Vec<Data> = Vec::with_capacity(n_out as usize);
            for _ in 0..n_out {
                ap_out.push(Data {
                    p: vec![0u8; sz_page],
                    nn: sz_page as i32,
                    sz_leaf: 0,
                });
            }
            // Rebuild the hash table.
            let res = self.tombstone_rehash(ctx, seg, data1, i_pg1, sz_key, &mut ap_out)?;
            sqliters_types::dtrace!(
                "TR|f5|tombreb|segid={} out={} slot={} key={} res={}",
                seg.i_segid,
                n_out,
                n_slot,
                sz_key,
                res
            );
            if res == 0 {
                return Ok(ap_out);
            }
            // If control flows to here, it was not possible to rebuild the hash table.
            // Free all buffers and then try again with more pages.
            n_slot = n_slot_per_page;
            n_out = n_out * 2 + 1;
        }
    }

    /// `fts5IndexTombstoneAdd` on segment (i_lvl, i_seg) of the cached structure.
    fn tombstone_add(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_lvl: usize,
        i_seg: usize,
        i_rowid: u64,
    ) -> Result<()> {
        let Some(seg) = self
            .p_struct
            .as_ref()
            .and_then(|s| s.a_level.get(i_lvl))
            .and_then(|l| l.a_seg.get(i_seg))
            .cloned()
        else {
            return Ok(());
        };
        let mut pg: Option<Data> = None;
        let mut i_pg: i32 = -1;
        self.n_contentless_delete += 1;

        if seg.n_pg_tombstone > 0 {
            i_pg = (i_rowid % seg.n_pg_tombstone as u64) as i32;
            let mut d = self.data_read(ctx, tombstone_rowid(seg.i_segid, i_pg))?;
            if 0 == tombstone_add_to_page(&mut d, false, seg.n_pg_tombstone, i_rowid) {
                let page = d.p[..d.nn as usize].to_vec();
                sqliters_types::dtrace!("TR|f5|tomb|segid={} pg={} o=add", seg.i_segid, i_pg);
                self.data_write(ctx, tombstone_rowid(seg.i_segid, i_pg), &page)?;
                return Ok(());
            }
            pg = Some(d);
        }

        // Have to rebuild the hash table. First figure out the key-size (4 or 8).
        let mut sz_key: usize = match pg.as_ref() {
            Some(d) => {
                if byte_at(&d.p, 0) == 4 {
                    4
                } else {
                    8
                }
            }
            None => 4,
        };
        if i_rowid > 0xFFFF_FFFF {
            sz_key = 8;
        }

        // Rebuild the hash table
        let mut ap_hash = self.tombstone_rebuild(ctx, &seg, pg.as_ref(), i_pg, sz_key)?;

        // If all has succeeded, write the new rowid into one of the new hash table
        // pages, then write them all out to disk.
        let n_hash = ap_hash.len() as i32;
        if n_hash > 0 {
            let idx = (i_rowid % n_hash as u64) as usize;
            tombstone_add_to_page(&mut ap_hash[idx], true, n_hash, i_rowid);
            for (ii, h) in ap_hash.iter().enumerate() {
                let page = h.p[..h.nn as usize].to_vec();
                self.data_write(ctx, tombstone_rowid(seg.i_segid, ii as i32), &page)?;
            }
            if let Some(st) = self.p_struct.as_mut() {
                st.a_level[i_lvl].a_seg[i_seg].n_pg_tombstone = n_hash;
            }
            if let Some(st) = self.p_struct.take() {
                let r = self.structure_write(ctx, &st);
                self.p_struct = Some(st);
                r?;
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------------------
    // Public interface (`sqlite3Fts5Index*`)
    // -----------------------------------------------------------------------------------

    /// `sqlite3Fts5IndexBeginWrite`.
    pub(crate) fn begin_write(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        b_delete: bool,
        i_rowid: i64,
    ) -> Result<()> {
        // Allocate the hash table if it has not already been allocated
        if self.hash.is_none() {
            self.hash = Some(Fts5Hash::new(self.config.detail));
        }
        // Flush the hash table to disk if required
        let c1 = i_rowid < self.i_write_rowid;
        let c2 = i_rowid == self.i_write_rowid && !self.b_delete;
        let c3 = self.n_pending_data > i64::from(self.config.n_hash_size);
        sqliters_types::dtrace!(
            "TR|f5|begin|del={} flush={} c={}{}{}",
            u8::from(b_delete),
            u8::from(c1 || c2 || c3),
            u8::from(c1),
            u8::from(c2),
            u8::from(c3)
        );
        if c1 || c2 || c3 {
            self.index_flush(ctx)?;
        }
        self.i_write_rowid = i_rowid;
        self.b_delete = b_delete;
        if !b_delete {
            self.n_pending_row += 1;
        }
        Ok(())
    }

    /// `sqlite3Fts5IndexWrite`.
    pub(crate) fn write(&mut self, i_col: i32, i_pos: i32, token: &[u8]) {
        let Some(hash) = self.hash.as_mut() else {
            return;
        };
        // Add the entry to the main terms index.
        let mut n = hash.write(self.i_write_rowid, i_col, i_pos, FTS5_MAIN_PREFIX, token);
        for (i, &n_char) in self.config.prefixes.iter().enumerate() {
            let n_byte = charlen_to_bytelen(token, n_char);
            if n_byte != 0 {
                n += hash.write(
                    self.i_write_rowid,
                    i_col,
                    i_pos,
                    FTS5_MAIN_PREFIX + i as u8 + 1,
                    &token[..n_byte],
                );
            }
        }
        // (`*pHash->pnByte` is a C int)
        self.n_pending_data = i64::from((self.n_pending_data + n) as i32);
    }

    /// `sqlite3Fts5IndexSync`.
    pub(crate) fn sync(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        sqliters_types::dtrace!(
            "TR|f5|sync|pend={} cdel={}",
            self.n_pending_data,
            self.n_contentless_delete
        );
        self.index_flush(ctx)
    }

    /// `fts5IndexDiscardData`.
    fn discard_data(&mut self) {
        if let Some(h) = self.hash.as_mut() {
            h.clear();
            self.n_pending_data = 0;
            self.n_pending_row = 0;
            self.flush_rc = None;
        }
        self.n_contentless_delete = 0;
    }

    /// `sqlite3Fts5IndexRollback`.
    pub(crate) fn rollback(&mut self) {
        sqliters_types::dtrace!("TR|f5|rollback|");
        self.discard_data();
        self.structure_invalidate();
    }

    /// `sqlite3Fts5IndexReinit`.
    pub(crate) fn reinit(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.structure_invalidate();
        self.discard_data();
        let st = Structure {
            n_origin_cntr: u64::from(self.config.contentless_delete),
            ..Structure::default()
        };
        self.data_write(ctx, FTS5_AVERAGES_ROWID, &[])?;
        self.structure_write(ctx, &st)
    }

    /// `sqlite3Fts5IndexOptimize`.
    pub(crate) fn optimize(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.index_flush(ctx)?;
        let st = self.structure_take(ctx)?;
        let new = Self::index_optimize_struct(&st);
        sqliters_types::dtrace!("TR|f5|optimize|work={}", u8::from(new.is_some()));
        if let Some(mut new) = new {
            let mut i_lvl = 0;
            while i_lvl < new.a_level.len() && new.a_level[i_lvl].a_seg.is_empty() {
                i_lvl += 1;
            }
            while i_lvl < new.a_level.len() && !new.a_level[i_lvl].a_seg.is_empty() {
                let mut n_rem = FTS5_OPT_WORK_UNIT;
                self.index_merge_level(ctx, &mut new, i_lvl, Some(&mut n_rem))?;
            }
            self.structure_write(ctx, &new)?;
        }
        Ok(())
    }

    /// `sqlite3Fts5IndexMerge`.
    pub(crate) fn merge(&mut self, ctx: &mut dyn VTabHostContext, n_merge: i32) -> Result<()> {
        self.index_flush(ctx)?;
        let st = self.structure_take(ctx)?;
        let mut n_min = self.config.n_usermerge;
        let mut n_merge = n_merge;
        let mut cur: Option<Structure> = Some(st);
        if n_merge < 0 {
            cur = cur.as_ref().and_then(Self::index_optimize_struct);
            n_min = 1;
            n_merge = if n_merge == i32::MIN {
                i32::MAX
            } else {
                -n_merge
            };
        }
        sqliters_types::dtrace!(
            "TR|f5|usermerge|n={} min={} st={}",
            n_merge,
            n_min,
            u8::from(cur.is_some())
        );
        if let Some(mut st) = cur
            && !st.a_level.is_empty()
            && self.index_merge(ctx, &mut st, n_merge, n_min)?
        {
            self.structure_write(ctx, &st)?;
        }
        Ok(())
    }

    /// `sqlite3Fts5IndexGetAverages`: (nRow, anSize).
    pub(crate) fn get_averages(
        &mut self,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<(i64, Vec<i64>)> {
        let n_col = self.config.n_col;
        let mut n_row = 0i64;
        let mut an_size = vec![0i64; n_col];
        let data = self.data_read(ctx, FTS5_AVERAGES_ROWID)?;
        if data.nn != 0 {
            let mut i = 0usize;
            let (v, n) = get_varint(&data.p, i);
            n_row = v as i64;
            i += n;
            let mut i_col = 0;
            while (i as i32) < data.nn && i_col < n_col {
                let (v, n) = get_varint(&data.p, i);
                an_size[i_col] = v as i64;
                i += n;
                i_col += 1;
            }
        }
        Ok((n_row, an_size))
    }

    /// `sqlite3Fts5IndexSetAverages`.
    pub(crate) fn set_averages(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        data: &[u8],
    ) -> Result<()> {
        self.data_write(ctx, FTS5_AVERAGES_ROWID, data)
    }

    /// `sqlite3Fts5IndexSetCookie`: an in-place write of the first four bytes of the
    /// structure record (`sqlite3_blob_write`; not a statement, so not counted).
    pub(crate) fn set_cookie(&mut self, ctx: &mut dyn VTabHostContext, i_new: i32) -> Result<()> {
        sqliters_types::dtrace!("TR|f5|cookie|v={}", i_new);
        self.write_gen = self.write_gen.wrapping_add(1);
        let ok = ctx.shadow_blob_write(
            &self.data_tbl,
            &mut self.data_hint,
            FTS5_STRUCTURE_ROWID,
            1,
            0,
            &(i_new as u32).to_be_bytes(),
        )?;
        if ok {
            Ok(())
        } else {
            // sqlite3_blob_open() fails with SQLITE_ERROR when the row is missing
            Err(SqliteError::new(ErrorCode::Error, "SQL logic error"))
        }
    }

    /// `sqlite3Fts5IndexLoadConfig`.
    pub(crate) fn load_config(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.structure_read(ctx)?;
        Ok(())
    }

    /// `sqlite3Fts5IndexGetOrigin`.
    pub(crate) fn get_origin(&mut self, ctx: &mut dyn VTabHostContext) -> Result<i64> {
        Ok(self.structure_read(ctx)?.n_origin_cntr as i64)
    }

    /// `sqlite3Fts5IndexContentlessDelete`.
    pub(crate) fn contentless_delete(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_origin: i64,
        i_rowid: i64,
    ) -> Result<()> {
        self.structure_read(ctx)?;
        let n_level = self.p_struct.as_ref().map_or(0, |s| s.a_level.len());
        let mut b_found = false; // True after pSeg->nEntryTombstone incr.
        for i_lvl in (0..n_level).rev() {
            let n_seg = self
                .p_struct
                .as_ref()
                .map_or(0, |s| s.a_level[i_lvl].a_seg.len());
            for i_seg in (0..n_seg).rev() {
                let hit = self.p_struct.as_ref().is_some_and(|s| {
                    let seg = &s.a_level[i_lvl].a_seg[i_seg];
                    seg.i_origin1 <= i_origin as u64 && seg.i_origin2 >= i_origin as u64
                });
                if hit {
                    if !b_found {
                        if let Some(s) = self.p_struct.as_mut() {
                            let seg = &mut s.a_level[i_lvl].a_seg[i_seg];
                            seg.n_entry_tombstone = seg.n_entry_tombstone.wrapping_add(1);
                        }
                        b_found = true;
                    }
                    self.tombstone_add(ctx, i_lvl, i_seg, i_rowid as u64)?;
                }
            }
        }
        Ok(())
    }

    /// True when the pending-terms hash exists (`p->pHash!=0`): queries then read it as the
    /// newest source.
    pub(crate) fn has_hash(&self) -> bool {
        self.hash.is_some()
    }

    /// `fts5SegIterHashInit` for a term lookup: the pending doclist of `key`.
    pub(crate) fn hash_query(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.hash.as_ref().and_then(|h| h.query(key))
    }

    /// The pending entries whose key starts with `prefix`, in key order (no side effect).
    pub(crate) fn hash_scan_copy(&self, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        match self.hash.as_ref() {
            Some(h) => h.scan_copy(prefix),
            None => Vec::new(),
        }
    }

    /// The side effects of `fts5SegIterHashInit` for a scan over the keys starting with
    /// `prefix` (`pTerm==0 || (flags & FTS5INDEX_QUERY_SCAN)`): the scan fixes the size
    /// field of every position list it visits and `bDelete` is cleared, so that the next
    /// write to the row most recently written flushes the hash first.
    pub(crate) fn hash_scan_touch(&mut self, prefix: &[u8]) {
        if let Some(h) = self.hash.as_mut() {
            h.scan_touch(prefix);
            self.b_delete = false;
        }
    }
}

/// `sqlite3Fts5IndexCharlenToBytelen`.
pub(crate) fn charlen_to_bytelen(p: &[u8], n_char: usize) -> usize {
    let n_byte = p.len();
    let mut n = 0usize;
    for i in 0..n_char {
        if n >= n_byte {
            return 0; // Input contains fewer than nChar chars
        }
        let c = p[n];
        n += 1;
        if c >= 0xc0 {
            if n >= n_byte {
                return 0;
            }
            while (p[n] & 0xc0) == 0x80 {
                n += 1;
                if n >= n_byte {
                    if i + 1 == n_char {
                        break;
                    }
                    return 0;
                }
            }
        }
    }
    n
}

// ---------------------------------------------------------------------------------------
// Integrity check (`sqlite3Fts5IndexIntegrityCheck`)
// ---------------------------------------------------------------------------------------

/// `sqlite3Fts5IndexEntryCksum`. `i_idx < 0` leaves the index byte out (the term then
/// already starts with it). The term bytes are added as C `char` values (signed).
pub(crate) fn entry_cksum(i_rowid: i64, i_col: i32, i_pos: i32, i_idx: i32, term: &[u8]) -> u64 {
    let mut ret: u64 = i_rowid as u64;
    ret = ret.wrapping_add((ret << 3).wrapping_add(i64::from(i_col) as u64));
    ret = ret.wrapping_add((ret << 3).wrapping_add(i64::from(i_pos) as u64));
    if i_idx >= 0 {
        ret = ret.wrapping_add(
            (ret << 3).wrapping_add((i64::from(FTS5_MAIN_PREFIX) + i64::from(i_idx)) as u64),
        );
    }
    for &b in term {
        ret = ret.wrapping_add((ret << 3).wrapping_add(i64::from(b as i8) as u64));
    }
    ret
}

impl Fts5Index {
    /// `sqlite3Fts5HashScanNext` + `sqlite3Fts5HashScanEntry` for a hash iterator: moves to
    /// the next entry of the scan; false at the end.
    fn seg_iter_hash_advance(it: &mut SegIter) -> bool {
        let Some(entries) = it.hash_entries.as_ref() else {
            return false;
        };
        let next = it.hash_pos + 1;
        let Some((key, list)) = entries.get(next) else {
            return false;
        };
        let mut d = Data::from_blob(list.clone());
        d.sz_leaf = d.nn;
        let (rowid, n) = get_varint(&d.p, 0);
        it.term.clear();
        it.term.extend_from_slice(key);
        it.i_rowid = rowid as i64;
        it.i_leaf_offset = n as i64;
        it.leaf = Some(d);
        it.hash_pos = next;
        true
    }

    /// `fts5SegIterHashInit(p, 0, 0, flags, pIter)`: an iterator over the whole pending-terms
    /// hash. The scan fixes the pending position lists and clears `bDelete`.
    fn seg_iter_hash_init(&mut self, it: &mut SegIter) {
        *it = SegIter::default();
        let entries = match self.hash.as_mut() {
            Some(h) => {
                h.scan_touch(&[]);
                h.scan_copy(&[])
            }
            None => Vec::new(),
        };
        self.b_delete = false;
        if let Some((key, list)) = entries.first() {
            let mut d = Data::from_blob(list.clone());
            d.sz_leaf = d.nn;
            let (rowid, n) = get_varint(&d.p, 0);
            it.term.extend_from_slice(key);
            it.i_rowid = rowid as i64;
            it.i_leaf_offset = n as i64;
            it.i_endof_doclist = d.nn;
            it.leaf = Some(d);
        }
        it.hash_entries = Some(entries);
        it.hash_pos = 0;
        if it.leaf.is_some() {
            self.seg_iter_load_npos(it);
        }
    }

    /// `fts5MultiIterNew(p, pStruct, FTS5INDEX_QUERY_NOOUTPUT, 0, 0, 0, -1, 0, ..)`: every
    /// entry of the index, the hash included.
    fn multi_iter_new_all(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &Structure,
    ) -> Result<MultiIter> {
        let with_hash = self.hash.is_some();
        let n_seg = st.n_segment.max(0) as usize + usize::from(with_hash);
        let mut mi = MultiIter::alloc(n_seg);
        let mut i_iter = 0;
        if with_hash {
            let mut it = SegIter::default();
            self.seg_iter_hash_init(&mut it);
            mi.a_seg[i_iter] = it;
            i_iter += 1;
        }
        for (i_lvl, lvl) in st.a_level.iter().enumerate() {
            for i_seg in (0..lvl.a_seg.len()).rev() {
                let mut it = SegIter::default();
                self.seg_iter_init(ctx, (i_lvl, i_seg), &lvl.a_seg[i_seg], &mut it)?;
                if i_iter < mi.a_seg.len() {
                    mi.a_seg[i_iter] = it;
                }
                i_iter += 1;
            }
        }
        self.multi_iter_finish_setup(ctx, &mut mi)?;
        Ok(mi)
    }

    /// `fts5SegiterPoslist(p, pSeg, 0, pBuf)`: the position list of the current entry.
    fn seg_iter_poslist(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &mut SegIter,
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        let mut n_rem = i64::from(seg.n_pos);
        let Some(leaf) = seg.leaf.as_ref() else {
            return Ok(());
        };
        let start = (seg.i_leaf_offset.max(0) as usize).min(leaf.p.len());
        let n_chunk = n_rem
            .min(i64::from(leaf.sz_leaf) - seg.i_leaf_offset)
            .max(0) as usize;
        let end = (start + n_chunk).min(leaf.p.len());
        buf.extend_from_slice(&leaf.p[start..end]);
        n_rem -= n_chunk as i64;
        let mut pgno = seg.i_leaf_pgno;
        let pgno_save = pgno + 1;
        while n_rem > 0 {
            if seg.hash_entries.is_some() {
                return Err(self.corrupt_idx());
            }
            pgno += 1;
            let data = self.leaf_read(ctx, segment_rowid(seg.seg.i_segid, pgno))?;
            let n_chunk = n_rem.min(i64::from(data.sz_leaf) - 4).max(0) as usize;
            let end = (4 + n_chunk).min(data.p.len());
            buf.extend_from_slice(&data.p[4..end]);
            n_rem -= n_chunk as i64;
            if pgno == pgno_save {
                seg.next_leaf = Some(data);
            }
        }
        Ok(())
    }

    /// `fts5IndexIntegrityCheckEmpty`.
    fn integrity_check_empty(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &StructureSegment,
        i_first: i32,
        i_no_rowid: i32,
        i_last: i32,
    ) -> Result<()> {
        // Now check that the iter.nEmpty leaves following the current leaf (a) exist and
        // (b) contain no terms.
        let mut i = i_first;
        while i <= i_last {
            let rowid = segment_rowid(seg.i_segid, i);
            let leaf = self.leaf_read(ctx, rowid)?;
            if !leaf.is_termless() || (i >= i_no_rowid && 0 != leaf.first_rowid_off()) {
                return Err(self.corrupt_rowid(rowid));
            }
            i += 1;
        }
        Ok(())
    }

    /// `fts5IntegrityCheckPgidx`.
    fn integrity_check_pgidx(&self, i_rowid: i64, leaf: &Data) -> Result<()> {
        let mut i_term_off: i64 = 0;
        let mut buf1: Vec<u8> = Vec::new();
        let mut buf2: Vec<u8> = Vec::new();
        let mut ii = leaf.sz_leaf;
        while ii < leaf.nn {
            let (n_incr, nb) = get_varint32(&leaf.p, ii as usize);
            ii += nb as i32;
            i_term_off += i64::from(n_incr);
            let mut i_off = i_term_off;
            if i_off >= i64::from(leaf.sz_leaf) {
                return Err(self.corrupt_rowid(i_rowid));
            } else if i_term_off == i64::from(n_incr) {
                let (n_byte, nb) = get_varint32(&leaf.p, i_off as usize);
                i_off += nb as i64;
                if i_off + i64::from(n_byte) > i64::from(leaf.sz_leaf) || n_byte < 0 {
                    return Err(self.corrupt_rowid(i_rowid));
                }
                buf1.clear();
                buf1.extend_from_slice(
                    &leaf.p[i_off as usize..(i_off + i64::from(n_byte)) as usize],
                );
            } else {
                let (n_keep, nb) = get_varint32(&leaf.p, i_off as usize);
                i_off += nb as i64;
                let (n_byte, nb) = get_varint32(&leaf.p, i_off as usize);
                i_off += nb as i64;
                if i64::from(n_keep) > buf1.len() as i64
                    || i_off + i64::from(n_byte) > i64::from(leaf.sz_leaf)
                    || n_keep < 0
                    || n_byte < 0
                {
                    return Err(self.corrupt_rowid(i_rowid));
                }
                buf1.truncate(n_keep as usize);
                buf1.extend_from_slice(
                    &leaf.p[i_off as usize..(i_off + i64::from(n_byte)) as usize],
                );
                if buf1.as_slice() <= buf2.as_slice() {
                    return Err(self.corrupt_rowid(i_rowid));
                }
            }
            buf2.clear();
            buf2.extend_from_slice(&buf1);
        }
        Ok(())
    }

    /// `fts5IndexIntegrityCheckSegment`.
    fn integrity_check_segment(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &StructureSegment,
    ) -> Result<()> {
        let b_secure_delete = self.config.i_version == FTS5_CURRENT_VERSION_SECUREDELETE;
        let mut i_idx_prev_leaf = seg.pgno_first - 1;
        let mut i_dlidx_prev_leaf = seg.pgno_last;
        if seg.pgno_first == 0 {
            return Ok(());
        }
        let sql = format!(
            "SELECT segid, term, (pgno>>1), (pgno&1) FROM {}.{} WHERE segid={} ORDER BY 1, 2",
            q_ident(&self.db_name),
            q_ident(&self.idx_tbl),
            seg.i_segid
        );
        let rows = match ctx.query_sql(&sql, &[]) {
            Ok(r) => r,
            Err(e) if e.code == ErrorCode::Error => return Err(corrupt()),
            Err(e) => return Err(e),
        };
        // Iterate through the b-tree hierarchy.
        for r in rows {
            let idx_term: Vec<u8> = match r.get(1) {
                Some(Value::Blob(b)) => b.clone(),
                Some(Value::Text(t)) => t.as_bytes().to_vec(),
                _ => Vec::new(),
            };
            let n_idx_term = idx_term.len() as i32;
            let i_idx_leaf = r.get(2).map_or(0, Value::to_integer_lossy) as i32;
            let b_idx_dlidx = r.get(3).map_or(0, Value::to_integer_lossy) as i32 != 0;

            // If the leaf in question has already been trimmed from the segment, ignore
            // this b-tree entry. Otherwise, load it into memory.
            if i_idx_leaf < seg.pgno_first {
                continue;
            }
            let i_row = segment_rowid(seg.i_segid, i_idx_leaf);
            let leaf = self.leaf_read(ctx, i_row)?;

            // Check that the leaf contains at least one term, and that it is equal to or
            // larger than the split-key in zIdxTerm. Also check that if there is also a
            // rowid pointer within the leaf page header, it points to a location before the
            // term.
            if leaf.nn <= leaf.sz_leaf {
                if n_idx_term == 0 && b_secure_delete && leaf.nn == leaf.sz_leaf && leaf.nn == 4 {
                    // special case - the very first page in a segment keeps its %_idx entry
                    // even if all the terms are removed from it by secure-delete operations.
                } else {
                    return Err(self.corrupt_rowid(i_row));
                }
            } else {
                let mut i_off = leaf.first_term_off();
                let i_rowid_off = leaf.first_rowid_off();
                if i_rowid_off >= i_off || i_off >= leaf.sz_leaf {
                    return Err(self.corrupt_rowid(i_row));
                }
                let (n_term, nb) = get_varint32(&leaf.p, i_off as usize);
                i_off += nb as i32;
                if i64::from(i_off) + i64::from(n_term) > i64::from(leaf.sz_leaf) || n_term < 0 {
                    return Err(self.corrupt_rowid(i_row));
                }
                let n_cmp = n_term.min(n_idx_term) as usize;
                let a = &leaf.p[i_off as usize..i_off as usize + n_cmp];
                let b = &idx_term[..n_cmp];
                let mut res = match a.cmp(b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                if res == 0 {
                    res = n_term - n_idx_term;
                }
                if res < 0 {
                    return Err(self.corrupt_rowid(i_row));
                }
                self.integrity_check_pgidx(i_row, &leaf)?;
            }

            // Now check that the iter.nEmpty leaves following the current leaf (a) exist and
            // (b) contain no terms.
            self.integrity_check_empty(
                ctx,
                seg,
                i_idx_prev_leaf + 1,
                i_dlidx_prev_leaf + 1,
                i_idx_leaf - 1,
            )?;

            // If there is a doclist-index, check that it looks right.
            if b_idx_dlidx {
                let mut i_prev_leaf = i_idx_leaf;
                let i_segid = seg.i_segid;
                let mut i_pg = 0;
                let mut dl = self.dlidx_iter_init(ctx, i_segid, i_idx_leaf)?;
                while !dl.a_lvl[0].b_eof {
                    // Check any rowid-less pages that occur before the current leaf.
                    i_pg = i_prev_leaf + 1;
                    while i_pg < dl.a_lvl[0].i_leaf_pgno {
                        let i_key = segment_rowid(i_segid, i_pg);
                        let pg = self.leaf_read(ctx, i_key)?;
                        if pg.first_rowid_off() != 0 {
                            return Err(self.corrupt_rowid(i_key));
                        }
                        i_pg += 1;
                    }
                    i_prev_leaf = dl.a_lvl[0].i_leaf_pgno;

                    // Check that the leaf page indicated by the iterator really does
                    // contain the rowid suggested by the same.
                    let i_key = segment_rowid(i_segid, i_prev_leaf);
                    let pg = self.leaf_read(ctx, i_key)?;
                    let i_rowid_off = pg.first_rowid_off();
                    if i_rowid_off >= pg.sz_leaf {
                        return Err(self.corrupt_rowid(i_key));
                    } else if !b_secure_delete || i_rowid_off > 0 {
                        let i_dl_rowid = dl.a_lvl[0].i_rowid;
                        let (v, _) = get_varint(&pg.p, i_rowid_off as usize);
                        let i_rowid = v as i64;
                        if i_rowid < i_dl_rowid || (!b_secure_delete && i_rowid != i_dl_rowid) {
                            return Err(self.corrupt_rowid(i_key));
                        }
                    }
                    self.dlidx_iter_next_r(ctx, &mut dl, 0)?;
                }
                i_dlidx_prev_leaf = i_pg;
            } else {
                i_dlidx_prev_leaf = seg.pgno_last;
            }
            i_idx_prev_leaf = i_idx_leaf;
        }
        Ok(())
    }

    /// `sqlite3Fts5IndexIntegrityCheck`.
    pub(crate) fn integrity_check(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        cksum: u64,
        b_use_cksum: bool,
    ) -> Result<()> {
        let detail = self.config.detail;
        let mut cksum2: u64 = 0;
        // Load the FTS index structure
        let st = self.structure_read(ctx)?.clone();

        // Check that the internal nodes of each segment match the leaves
        for lvl in &st.a_level {
            for seg in &lvl.a_seg {
                self.integrity_check_segment(ctx, seg)?;
            }
        }

        // The cksum argument passed to this function is a checksum calculated based on
        // the contents of the "content" table. Here cksum2 is calculated by iterating
        // through the whole index.
        let mut mi = self.multi_iter_new_all(ctx, &st)?;
        let mut poslist: Vec<u8> = Vec::new();
        while !mi.b_eof {
            let i_first = mi.first();
            let i_rowid = mi.a_seg[i_first].i_rowid;
            if detail == Detail::None {
                let s = &mi.a_seg[i_first];
                let is_empty = s.leaf.is_some() && s.n_pos == 0;
                if !is_empty {
                    cksum2 ^= entry_cksum(i_rowid, 0, 0, -1, &s.term);
                }
            } else {
                poslist.clear();
                let mut seg = std::mem::take(&mut mi.a_seg[i_first]);
                let r = self.seg_iter_poslist(ctx, &mut seg, &mut poslist);
                mi.a_seg[i_first] = seg;
                r?;
                poslist.extend_from_slice(&[0, 0, 0, 0]);
                let term = &mi.a_seg[i_first].term;
                // sqlite3Fts5PoslistNext64
                let n = poslist.len();
                let mut i = 0usize;
                let mut i_pos: i64 = 0;
                while i < n {
                    let (v, nb) = get_varint32(&poslist, i);
                    i += nb;
                    let mut i_val = v as u32;
                    if i_val <= 1 {
                        if i_val != 0 {
                            let (v, nb) = get_varint32(&poslist, i);
                            i += nb;
                            let i_off = i64::from(v as u32) << 32;
                            let (v, nb) = get_varint32(&poslist, i);
                            i += nb;
                            i_val = v as u32;
                            if i_val < 2 {
                                // This is a corrupt record. So stop parsing it here.
                                break;
                            }
                            i_pos = i_off + i64::from((i_val - 2) & 0x7FFF_FFFF);
                        }
                    } else {
                        i_pos = (i_pos & (0x7FFF_FFFFi64 << 32))
                            + ((i_pos + i64::from(i_val - 2)) & 0x7FFF_FFFF);
                    }
                    let i_col = ((i_pos >> 32) & 0x7FFF_FFFF) as i32;
                    let i_tok_off = (i_pos & 0x7FFF_FFFF) as i32;
                    cksum2 ^= entry_cksum(i_rowid, i_col, i_tok_off, -1, term);
                }
            }
            self.multi_iter_next(ctx, &mut mi, false, 0)?;
        }

        if b_use_cksum && cksum != cksum2 {
            return Err(SqliteError::with_extended(
                FTS5_CORRUPT,
                format!("fts5: checksum mismatch for table \"{}\"", self.name),
            ));
        }
        Ok(())
    }
}
