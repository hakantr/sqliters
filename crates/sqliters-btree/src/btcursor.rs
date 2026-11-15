//! `BtCursor`: the cursor of `btree.c`, for the port of C's virtual machine (`sqliters-vdbe`,
//! module `vdbe`).
//!
//! One traversal finds the data and holds the page references C holds: the cursor keeps the
//! page stack `apPage[0..=iPage]` with one pager reference per page (`getAndInitPage` /
//! `releasePage`), `aiIdx[]`, `ix`, `eState`, `curFlags`, the parsed cell (`info`), the saved
//! key of `CURSOR_REQUIRESEEK` and the overflow page list cache (`aOverflow`). The functions
//! are ports of the functions of the same name in `btree.c` (3.54.1), in C's order of
//! operations; they are methods of [`Btree`] that take the cursor as an argument because the
//! cursor is owned by the machine (`VdbeCursor.uc.pCursor`).
//!
//! Scope: the reading half (`sqlite3BtreeCursor`, `First`, `Last`, `Next`, `Previous`,
//! `TableMoveto`, `IndexMoveto`, `Payload`, `PayloadFetch`, `PayloadSize`, `IntegerKey`,
//! `Count`, `CursorHasMoved`, `CursorRestore`, `Eof`, `IsEmpty`, `saveCursorPosition`).
//! Writes still go through the functions of `insert.rs` / `lib.rs`; the machine saves the
//! positions of its cursors around them (`saveAllCursors`).

use sqliters_types::{Pgno, Result, SqliteError};

use crate::Btree;

/// `BTCURSOR_MAX_DEPTH`
pub const BTCURSOR_MAX_DEPTH: usize = 20;

/// `CURSOR_VALID` …: `BtCursor.eState`.
pub const CURSOR_VALID: u8 = 0;
pub const CURSOR_INVALID: u8 = 1;
pub const CURSOR_SKIPNEXT: u8 = 2;
pub const CURSOR_REQUIRESEEK: u8 = 3;
pub const CURSOR_FAULT: u8 = 4;

/// `BTCF_*`: `BtCursor.curFlags`.
pub const BTCF_WRITE_FLAG: u8 = 0x01;
pub const BTCF_VALID_NKEY: u8 = 0x02;
pub const BTCF_VALID_OVFL: u8 = 0x04;
pub const BTCF_AT_LAST: u8 = 0x08;
pub const BTCF_INCRBLOB: u8 = 0x10;
pub const BTCF_MULTIPLE: u8 = 0x20;
pub const BTCF_PINNED: u8 = 0x40;

/// `BTREE_SEEK_EQ`, `BTREE_BULKLOAD`: `sqlite3BtreeCursorHintFlags`.
pub const BTREE_SEEK_EQ: u8 = 0x02;
pub const BTREE_BULKLOAD: u8 = 0x01;

/// The fields of `MemPage` a cursor reads (`decodeFlags`, `btreeInitPage`).
#[derive(Clone, Copy, Debug, Default)]
struct PageMeta {
    pgno: Pgno,
    /// `hdrOffset`: 100 for page 1, 0 otherwise
    hdr: u16,
    leaf: bool,
    int_key: bool,
    /// `childPtrSize`: 0 on a leaf, 4 otherwise
    child_ptr_size: u8,
    n_cell: u16,
    /// `cellOffset`: offset of the cell pointer array
    cell_offset: u16,
    max_local: u16,
    min_local: u16,
    max1byte_payload: u8,
}

/// `struct CellInfo`.
#[derive(Clone, Copy, Debug, Default)]
struct CellInfo {
    /// `nKey`: the key for INTKEY tables, the payload size otherwise
    n_key: i64,
    /// Offset of `pPayload` in the page
    payload: u32,
    n_payload: u32,
    n_local: u16,
    /// `nSize`: size of the cell content on the main b-tree page; 0 = "not parsed"
    n_size: u16,
}

/// The comparison `sqlite3BtreeIndexMoveto` runs against the cells it probes
/// (`xRecordCompare(nCell, pCell, pIdxKey)` with the `errCode` of the `UnpackedRecord`).
pub trait IdxKeyCompare {
    /// `xRecordCompare(nKey1, pKey1, pIdxKey)`: <0, 0, >0 as the cell key is less than, equal
    /// to or greater than the search key.
    fn compare(&mut self, cell_key: &[u8]) -> i32;
    /// `sqlite3VdbeRecordCompare` (the general comparator, used for keys read from overflow
    /// pages).
    fn compare_general(&mut self, cell_key: &[u8]) -> i32 {
        self.compare(cell_key)
    }
    /// `pIdxKey->errCode != 0`
    fn has_error(&self) -> bool;
    /// `pIdxKey->errCode = 0`
    fn clear_error(&mut self);
}

/// `struct BtCursor`.
#[derive(Clone, Debug)]
pub struct BtCursor {
    /// `eState`
    pub e_state: u8,
    /// `curFlags`
    pub cur_flags: u8,
    /// `hints`
    pub hints: u8,
    /// `skipNext`
    pub skip_next: i32,
    /// `pgnoRoot`
    pub pgno_root: Pgno,
    /// `curIntKey`
    pub cur_int_key: bool,
    /// `pKeyInfo != 0`: the cursor was opened on an index b-tree
    pub has_key_info: bool,
    /// `iPage`: index of the current page in `ap`, -1 when the cursor holds no page
    i_page: i8,
    /// `ix`
    ix: u16,
    /// `aiIdx[]`
    ai_idx: [u16; BTCURSOR_MAX_DEPTH],
    /// `apPage[]` and `pPage` (`ap[i_page]`): one pager reference each
    ap: [PageMeta; BTCURSOR_MAX_DEPTH],
    info: CellInfo,
    /// `nKey`: saved integer key / size of `p_key`
    n_key: i64,
    /// `pKey`: saved key of an index cursor
    p_key: Option<Vec<u8>>,
    /// `aOverflow[]`
    a_overflow: Vec<Pgno>,
    /// Page-cache epoch at which the references were taken.
    epoch: u32,
}

#[inline]
fn get2(d: &[u8], off: usize) -> usize {
    (usize::from(d[off]) << 8) | usize::from(d[off + 1])
}
#[inline]
fn get4(d: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}

#[cold]
fn corrupt() -> SqliteError {
    SqliteError::corrupt("database disk image is malformed")
}

/// `getVarint32` at `off`: `(value, bytes)`. `None` when the varint runs off the page.
#[inline]
fn varint32(d: &[u8], off: usize) -> Option<(u32, usize)> {
    let b0 = *d.get(off)?;
    if b0 < 0x80 {
        return Some((u32::from(b0), 1));
    }
    let (v, n) = varint64(d, off)?;
    Some((if v > u64::from(u32::MAX) { u32::MAX } else { v as u32 }, n))
}

/// `sqlite3GetVarint` at `off`.
#[inline]
fn varint64(d: &[u8], off: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..9 {
        let b = *d.get(off + i)?;
        if i == 8 {
            return Some(((v << 8) | u64::from(b), 9));
        }
        v = (v << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

impl BtCursor {
    /// A cursor that is not open on any b-tree (`sqlite3BtreeCursorZero`).
    pub fn detached() -> BtCursor {
        BtCursor {
            e_state: CURSOR_INVALID,
            cur_flags: 0,
            hints: 0,
            skip_next: 0,
            pgno_root: 0,
            cur_int_key: true,
            has_key_info: false,
            i_page: -1,
            ix: 0,
            ai_idx: [0; BTCURSOR_MAX_DEPTH],
            ap: [PageMeta::default(); BTCURSOR_MAX_DEPTH],
            info: CellInfo::default(),
            n_key: 0,
            p_key: None,
            a_overflow: Vec::new(),
            epoch: 0,
        }
    }

    /// `sqlite3BtreeCursorIsValidNN`
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.e_state == CURSOR_VALID
    }

    /// `sqlite3BtreeCursorHasMoved`: "CURSOR_VALID != eState".
    #[inline]
    pub fn has_moved(&self) -> bool {
        self.e_state != CURSOR_VALID
    }

    /// `sqlite3BtreeEof`
    #[inline]
    pub fn eof(&self) -> bool {
        self.e_state != CURSOR_VALID
    }

    /// `sqlite3BtreeCursorHintFlags`
    #[inline]
    pub fn hint_flags(&mut self, x: u8) {
        self.hints = x;
    }

    /// `sqlite3BtreeCursorHasHint`
    #[inline]
    pub fn has_hint(&self, mask: u8) -> bool {
        self.hints & mask != 0
    }

    /// `sqlite3BtreeClearCursor`
    #[inline]
    pub fn clear_cursor(&mut self) {
        self.p_key = None;
        self.e_state = CURSOR_INVALID;
    }

    /// Page number of the page the cursor is on (0: none) — for traces.
    #[inline]
    pub fn page_no(&self) -> Pgno {
        if self.i_page < 0 { 0 } else { self.ap[self.i_page as usize].pgno }
    }

    /// `ix` — for traces.
    #[inline]
    pub fn cell_index(&self) -> u16 {
        self.ix
    }

    #[inline]
    fn pg(&self) -> &PageMeta {
        &self.ap[self.i_page as usize]
    }
}

impl Btree {
    /// `sqlite3BtreeCursor(p, iTable, wrFlag, pKeyInfo, pCur)`: no page is fetched.
    pub fn bc_cursor(&mut self, i_table: Pgno, wr_flag: bool, has_key_info: bool) -> BtCursor {
        // btreeCursor: "if( iTable<=1 ){ ... else if( btreePagecount(pBt)==0 ){ iTable = 0; } }":
        // the schema table of an empty database file has no root page.
        let i_table = if i_table <= 1 && self.pager.page_count() == 0 { 0 } else { i_table };
        BtCursor {
            e_state: CURSOR_INVALID,
            cur_flags: if wr_flag { BTCF_WRITE_FLAG } else { 0 },
            hints: 0,
            skip_next: 0,
            pgno_root: i_table,
            cur_int_key: !has_key_info,
            has_key_info,
            i_page: -1,
            ix: 0,
            ai_idx: [0; BTCURSOR_MAX_DEPTH],
            ap: [PageMeta::default(); BTCURSOR_MAX_DEPTH],
            info: CellInfo::default(),
            n_key: 0,
            p_key: None,
            a_overflow: Vec::new(),
            epoch: self.pager.pcache_epoch(),
        }
    }

    /// The references of the cursor are still the ones the cache knows about.
    #[inline]
    fn bc_live(&mut self, cur: &mut BtCursor) -> bool {
        let e = self.pager.pcache_epoch();
        if cur.epoch != e {
            cur.epoch = e;
            cur.i_page = -1;
            if cur.e_state == CURSOR_VALID || cur.e_state == CURSOR_SKIPNEXT {
                cur.e_state = CURSOR_INVALID;
            }
            cur.cur_flags &= !(BTCF_VALID_NKEY | BTCF_VALID_OVFL | BTCF_AT_LAST);
            cur.info.n_size = 0;
            return false;
        }
        true
    }

    /// `btreeReleaseAllCursorPages`
    fn bc_release_all_cursor_pages(&mut self, cur: &mut BtCursor) {
        if self.bc_live(cur) && cur.i_page >= 0 {
            for i in 0..cur.i_page as usize {
                self.pager.page_unref(cur.ap[i].pgno);
            }
            self.pager.page_unref(cur.ap[cur.i_page as usize].pgno);
        }
        cur.i_page = -1;
    }

    /// `sqlite3BtreeCloseCursor`
    pub fn bc_close_cursor(&mut self, cur: &mut BtCursor) {
        self.bc_release_all_cursor_pages(cur);
        cur.p_key = None;
        cur.a_overflow = Vec::new();
        cur.e_state = CURSOR_INVALID;
    }

    /// `getAndInitPage` + `btreeInitPage` (`decodeFlags`): a reference on `pgno` and its
    /// `MemPage` fields.
    fn bc_get_and_init_page(&mut self, pgno: Pgno) -> Result<PageMeta> {
        if pgno == 0 || pgno > self.pager.page_count() {
            return Err(corrupt());
        }
        self.pager.page_ref_btree(pgno)?;
        let usable = self.pager.usable_size();
        let page_size = self.pager.page_size();
        let hdr: usize = if pgno == 1 { 100 } else { 0 };
        let d = match self.pager.read_page(pgno) {
            Ok(d) => d,
            Err(e) => {
                self.pager.page_unref(pgno);
                return Err(e);
            }
        };
        let mut ok = d.len() >= hdr + 12;
        let mut m = PageMeta { pgno, hdr: hdr as u16, ..PageMeta::default() };
        if ok {
            let flag = d[hdr];
            m.leaf = flag & 0x08 != 0;
            m.child_ptr_size = if m.leaf { 0 } else { 4 };
            let idx_max_local = (usable - 12) * 64 / 255 - 23;
            let min_local = (usable - 12) * 32 / 255 - 23;
            match flag & !0x08 {
                0x05 => {
                    m.int_key = true;
                    m.max_local = (usable - 35) as u16;
                    m.min_local = min_local as u16;
                }
                0x02 => {
                    m.int_key = false;
                    m.max_local = idx_max_local as u16;
                    m.min_local = min_local as u16;
                }
                _ => ok = false,
            }
            m.max1byte_payload = if idx_max_local > 127 { 127 } else { idx_max_local as u8 };
            m.cell_offset = (hdr + 8 + usize::from(m.child_ptr_size)) as u16;
            let n_cell = get2(d, hdr + 3);
            // MX_CELL(pBt) = (pageSize-8)/6
            if n_cell > (page_size as usize - 8) / 6 {
                ok = false;
            }
            m.n_cell = n_cell as u16;
            if usize::from(m.cell_offset) + 2 * n_cell > d.len() {
                ok = false;
            }
        }
        if !ok {
            self.pager.page_unref(pgno);
            return Err(corrupt());
        }
        Ok(m)
    }

    /// `findCell(pPage, iCell)`: offset of cell `i`.
    #[inline]
    fn bc_find_cell(d: &[u8], m: &PageMeta, i: usize) -> usize {
        get2(d, usize::from(m.cell_offset) + 2 * i) & (d.len() - 1)
    }

    /// `btreeParseCell(pPage, iCell, pInfo)`.
    fn bc_parse_cell(&mut self, m: PageMeta, i_cell: usize) -> Result<CellInfo> {
        let usable = self.pager.usable_size();
        let d = self.pager.read_page(m.pgno)?;
        if i_cell >= usize::from(m.n_cell) {
            return Err(corrupt());
        }
        let cell = Self::bc_find_cell(d, &m, i_cell);
        let mut info = CellInfo::default();
        if m.int_key && !m.leaf {
            // btreeParseCellPtrNoPayload
            let (k, n) = varint64(d, cell + 4).ok_or_else(corrupt)?;
            info.n_key = k as i64;
            info.n_payload = 0;
            info.n_local = 0;
            info.payload = 0;
            info.n_size = (4 + n) as u16;
            return Ok(info);
        }
        let mut p = cell + usize::from(m.child_ptr_size);
        let (n_payload, n) = varint32(d, p).ok_or_else(corrupt)?;
        p += n;
        if m.int_key {
            // btreeParseCellPtr
            let (k, n) = varint64(d, p).ok_or_else(corrupt)?;
            p += n;
            info.n_key = k as i64;
        } else {
            // btreeParseCellPtrIndex
            info.n_key = i64::from(n_payload);
        }
        info.n_payload = n_payload;
        info.payload = p as u32;
        if n_payload <= u32::from(m.max_local) {
            let mut sz = n_payload as usize + (p - cell);
            if sz < 4 {
                sz = 4;
            }
            info.n_size = sz as u16;
            info.n_local = n_payload as u16;
        } else {
            // btreeParseCellAdjustSizeForOverflow
            let min_local = u32::from(m.min_local);
            let surplus = min_local + (n_payload - min_local) % (usable - 4);
            info.n_local = if surplus <= u32::from(m.max_local) { surplus as u16 } else { min_local as u16 };
            info.n_size = ((p + usize::from(info.n_local) - cell) + 4) as u16;
        }
        Ok(info)
    }

    /// `getCellInfo`
    #[inline]
    fn bc_get_cell_info(&mut self, cur: &mut BtCursor) -> Result<()> {
        if cur.info.n_size == 0 {
            cur.cur_flags |= BTCF_VALID_NKEY;
            cur.info = self.bc_parse_cell(*cur.pg(), usize::from(cur.ix))?;
        }
        Ok(())
    }

    /// `sqlite3BtreeIntegerKey`
    #[inline]
    pub fn bc_integer_key(&mut self, cur: &mut BtCursor) -> Result<i64> {
        self.bc_get_cell_info(cur)?;
        Ok(cur.info.n_key)
    }

    /// `sqlite3BtreePayloadSize`
    #[inline]
    pub fn bc_payload_size(&mut self, cur: &mut BtCursor) -> Result<u32> {
        self.bc_get_cell_info(cur)?;
        Ok(cur.info.n_payload)
    }

    /// `sqlite3BtreeOffset`
    pub fn bc_offset(&mut self, cur: &mut BtCursor) -> Result<i64> {
        self.bc_get_cell_info(cur)?;
        Ok(i64::from(self.pager.page_size()) * (i64::from(cur.pg().pgno) - 1) + i64::from(cur.info.payload))
    }

    /// `sqlite3BtreeMaxRecordSize`
    pub fn bc_max_record_size(&mut self) -> i64 {
        i64::from(self.pager.page_size()) * i64::from(self.pager.page_count())
    }

    /// `sqlite3BtreePayloadFetch`: the part of the payload that is on the b-tree page.
    /// (`getCellInfo` must have run: call [`Self::bc_payload_size`] first, as C does.)
    pub fn bc_payload_fetch(&mut self, cur: &BtCursor) -> Result<&[u8]> {
        let m = *cur.pg();
        let info = cur.info;
        let d = self.pager.read_page(m.pgno)?;
        let start = info.payload as usize;
        let mut amt = usize::from(info.n_local);
        if start > d.len() {
            return Ok(&[]);
        }
        if amt > d.len() - start {
            amt = d.len() - start;
        }
        Ok(&d[start..start + amt])
    }

    /// `getOverflowPage(pBt, ovfl, 0, &next)`: the number of the overflow page after `ovfl`.
    fn bc_get_overflow_page(&mut self, ovfl: Pgno) -> Result<Pgno> {
        if self.is_auto_vacuum() {
            let usable = self.pager.usable_size();
            let lock_page = self.pager.lock_byte_page();
            let mut i_guess = ovfl + 1;
            while crate::ptrmap::is_ptrmap_page(i_guess, usable, lock_page) || i_guess == lock_page {
                i_guess += 1;
            }
            if i_guess <= self.pager.page_count() {
                let (e_type, pgno) = self.ptrmap_get(i_guess)?;
                if e_type == crate::ptrmap::PtrmapType::Overflow2 && pgno == ovfl {
                    return Ok(i_guess);
                }
            }
        }
        self.pager.page_ref(ovfl)?;
        let next = match self.pager.read_page_unchecked(ovfl) {
            Ok(d) => get4(d, 0),
            Err(e) => {
                self.pager.page_unref(ovfl);
                return Err(e);
            }
        };
        self.pager.page_unref(ovfl);
        Ok(next)
    }

    /// `accessPayload(pCur, offset, amt, pBuf, 0)`: reads `buf.len()` bytes of the payload of
    /// the entry the cursor is on, starting `offset` bytes in.
    fn bc_access_payload(&mut self, cur: &mut BtCursor, mut offset: u32, buf: &mut [u8]) -> Result<()> {
        let m = *cur.pg();
        if cur.ix >= m.n_cell {
            return Err(corrupt());
        }
        self.bc_get_cell_info(cur)?;
        let usable = self.pager.usable_size();
        let info = cur.info;
        if info.payload > usable - u32::from(info.n_local) {
            return Err(corrupt());
        }
        let n_local = u32::from(info.n_local);
        let mut amt = buf.len() as u32;
        let mut out = 0usize;
        let mut next_page: Pgno = 0;
        {
            let d = self.pager.read_page(m.pgno)?;
            let pl = info.payload as usize;
            if offset < n_local {
                let mut a = amt;
                if a + offset > n_local {
                    a = n_local - offset;
                }
                let s = pl + offset as usize;
                if s + a as usize > d.len() {
                    return Err(corrupt());
                }
                buf[..a as usize].copy_from_slice(&d[s..s + a as usize]);
                offset = 0;
                out += a as usize;
                amt -= a;
            } else {
                offset -= n_local;
            }
            if amt > 0 {
                if pl + n_local as usize + 4 > d.len() {
                    return Err(corrupt());
                }
                next_page = get4(d, pl + n_local as usize);
            }
        }
        if amt > 0 {
            let ovfl_size = usable - 4;
            let mut i_idx: usize = 0;
            if cur.cur_flags & BTCF_VALID_OVFL == 0 {
                let n_ovfl = ((u64::from(info.n_payload) - u64::from(n_local) + u64::from(ovfl_size) - 1)
                    / u64::from(ovfl_size)) as usize;
                cur.a_overflow.clear();
                cur.a_overflow.resize(n_ovfl + 1, 0);
                cur.cur_flags |= BTCF_VALID_OVFL;
            } else if cur.a_overflow.get((offset / ovfl_size) as usize).copied().unwrap_or(0) != 0 {
                i_idx = (offset / ovfl_size) as usize;
                next_page = cur.a_overflow[i_idx];
                offset %= ovfl_size;
            }
            let n_page = self.pager.page_count();
            while next_page != 0 {
                if next_page > n_page {
                    return Err(corrupt());
                }
                if i_idx + 1 >= cur.a_overflow.len() {
                    cur.a_overflow.resize(i_idx + 2, 0);
                }
                cur.a_overflow[i_idx] = next_page;
                if offset >= ovfl_size {
                    // The only reason to read this page is to obtain the page number of the
                    // next page in the overflow chain.
                    if cur.a_overflow[i_idx + 1] != 0 {
                        next_page = cur.a_overflow[i_idx + 1];
                    } else {
                        next_page = self.bc_get_overflow_page(next_page)?;
                    }
                    offset -= ovfl_size;
                } else {
                    let mut a = amt;
                    if a + offset > ovfl_size {
                        a = ovfl_size - offset;
                    }
                    // SQLITE_DIRECT_OVERFLOW_READ: (1) read (2) offset==0 (3,4,5) the pager
                    // allows it (6) at least 4 bytes have already been written to the buffer.
                    let direct = offset == 0 && out >= 4 && self.pager.direct_overflow_read_ok(next_page);
                    if !direct {
                        self.pager.page_ref(next_page)?;
                    }
                    let res = match self.pager.read_page_unchecked(next_page) {
                        Ok(d) => {
                            let s = offset as usize + 4;
                            if s + a as usize > d.len() {
                                Err(corrupt())
                            } else {
                                buf[out..out + a as usize].copy_from_slice(&d[s..s + a as usize]);
                                Ok(get4(d, 0))
                            }
                        }
                        Err(e) => Err(e),
                    };
                    if !direct {
                        self.pager.page_unref(next_page);
                    }
                    next_page = res?;
                    offset = 0;
                    amt -= a;
                    if amt == 0 {
                        return Ok(());
                    }
                    out += a as usize;
                }
                i_idx += 1;
            }
        }
        if amt > 0 {
            return Err(corrupt());
        }
        Ok(())
    }

    /// `sqlite3BtreePayload(pCur, offset, amt, pBuf)` (`amt` = `buf.len()`).
    #[inline]
    pub fn bc_payload(&mut self, cur: &mut BtCursor, offset: u32, buf: &mut [u8]) -> Result<()> {
        self.bc_access_payload(cur, offset, buf)
    }

    /// `moveToChild`
    fn bc_move_to_child(&mut self, cur: &mut BtCursor, new_pgno: Pgno) -> Result<()> {
        if cur.i_page as usize >= BTCURSOR_MAX_DEPTH - 1 {
            return Err(corrupt());
        }
        cur.info.n_size = 0;
        cur.cur_flags &= !(BTCF_VALID_NKEY | BTCF_VALID_OVFL);
        cur.ai_idx[cur.i_page as usize] = cur.ix;
        cur.ix = 0;
        let m = self.bc_get_and_init_page(new_pgno)?;
        if m.n_cell < 1 || m.int_key != cur.cur_int_key {
            self.pager.page_unref(new_pgno);
            return Err(corrupt());
        }
        cur.i_page += 1;
        cur.ap[cur.i_page as usize] = m;
        Ok(())
    }

    /// `moveToParent`
    fn bc_move_to_parent(&mut self, cur: &mut BtCursor) {
        cur.info.n_size = 0;
        cur.cur_flags &= !(BTCF_VALID_NKEY | BTCF_VALID_OVFL);
        cur.ix = cur.ai_idx[cur.i_page as usize - 1];
        let leaf = cur.ap[cur.i_page as usize].pgno;
        cur.i_page -= 1;
        self.pager.page_unref(leaf);
    }

    /// `moveToRoot`: `Ok(false)` is `SQLITE_EMPTY`.
    fn bc_move_to_root(&mut self, cur: &mut BtCursor) -> Result<bool> {
        self.bc_live(cur);
        let mut skip_init = false;
        if cur.i_page >= 0 {
            if cur.i_page > 0 {
                self.pager.page_unref(cur.ap[cur.i_page as usize].pgno);
                cur.i_page -= 1;
                while cur.i_page > 0 {
                    self.pager.page_unref(cur.ap[cur.i_page as usize].pgno);
                    cur.i_page -= 1;
                }
                skip_init = true;
            }
        } else if cur.pgno_root == 0 {
            cur.e_state = CURSOR_INVALID;
            return Ok(false);
        } else {
            if cur.e_state >= CURSOR_REQUIRESEEK {
                if cur.e_state == CURSOR_FAULT {
                    return Err(corrupt());
                }
                cur.clear_cursor();
            }
            match self.bc_get_and_init_page(cur.pgno_root) {
                Ok(m) => {
                    cur.ap[0] = m;
                    cur.i_page = 0;
                    cur.cur_int_key = m.int_key;
                }
                Err(e) => {
                    cur.e_state = CURSOR_INVALID;
                    return Err(e);
                }
            }
        }
        let root = cur.ap[0];
        if !skip_init && cur.has_key_info == root.int_key {
            return Err(corrupt());
        }
        cur.ix = 0;
        cur.info.n_size = 0;
        cur.cur_flags &= !(BTCF_AT_LAST | BTCF_VALID_NKEY | BTCF_VALID_OVFL);
        if root.n_cell > 0 {
            cur.e_state = CURSOR_VALID;
            Ok(true)
        } else if !root.leaf {
            if root.pgno != 1 {
                return Err(corrupt());
            }
            let subpage = {
                let d = self.pager.read_page(root.pgno)?;
                get4(d, usize::from(root.hdr) + 8)
            };
            cur.e_state = CURSOR_VALID;
            self.bc_move_to_child(cur, subpage)?;
            Ok(true)
        } else {
            cur.e_state = CURSOR_INVALID;
            Ok(false)
        }
    }

    /// The child pointer of cell `i` (`get4byte(findCell(pPage, i))`), or the right-most
    /// pointer when `i == nCell`.
    #[inline]
    fn bc_child(&mut self, m: PageMeta, i: usize) -> Result<Pgno> {
        let d = self.pager.read_page(m.pgno)?;
        if i >= usize::from(m.n_cell) {
            Ok(get4(d, usize::from(m.hdr) + 8))
        } else {
            let c = Self::bc_find_cell(d, &m, i);
            if c + 4 > d.len() {
                return Err(corrupt());
            }
            Ok(get4(d, c))
        }
    }

    /// `moveToLeftmost`
    fn bc_move_to_leftmost(&mut self, cur: &mut BtCursor) -> Result<()> {
        loop {
            let m = *cur.pg();
            if m.leaf {
                return Ok(());
            }
            let pgno = self.bc_child(m, usize::from(cur.ix))?;
            self.bc_move_to_child(cur, pgno)?;
        }
    }

    /// `moveToRightmost`
    fn bc_move_to_rightmost(&mut self, cur: &mut BtCursor) -> Result<()> {
        loop {
            let m = *cur.pg();
            if m.leaf {
                cur.ix = m.n_cell - 1;
                return Ok(());
            }
            let pgno = self.bc_child(m, usize::from(m.n_cell))?;
            cur.ix = m.n_cell;
            self.bc_move_to_child(cur, pgno)?;
        }
    }

    /// `sqlite3BtreeFirst`: returns `*pRes` (true: the table is empty).
    pub fn bc_first(&mut self, cur: &mut BtCursor) -> Result<bool> {
        if self.bc_move_to_root(cur)? {
            self.bc_move_to_leftmost(cur)?;
            Ok(false)
        } else {
            Ok(true)
        }
    }

    /// `sqlite3BtreeIsEmpty`
    pub fn bc_is_empty(&mut self, cur: &mut BtCursor) -> Result<bool> {
        if cur.e_state == CURSOR_VALID {
            return Ok(false);
        }
        Ok(!self.bc_move_to_root(cur)?)
    }

    /// `sqlite3BtreeLast`: returns `*pRes` (true: the table is empty).
    pub fn bc_last(&mut self, cur: &mut BtCursor) -> Result<bool> {
        if cur.e_state == CURSOR_VALID && cur.cur_flags & BTCF_AT_LAST != 0 && self.bc_live(cur) {
            return Ok(false);
        }
        // btreeLast
        if self.bc_move_to_root(cur)? {
            match self.bc_move_to_rightmost(cur) {
                Ok(()) => {
                    cur.cur_flags |= BTCF_AT_LAST;
                    Ok(false)
                }
                Err(e) => {
                    cur.cur_flags &= !BTCF_AT_LAST;
                    Err(e)
                }
            }
        } else {
            Ok(true)
        }
    }

    /// The integer key of cell `idx` of table page `m` (the `nCellKey` of the binary search of
    /// `sqlite3BtreeTableMoveto`).
    #[inline]
    fn bc_cell_int_key(d: &[u8], m: &PageMeta, idx: usize) -> Result<i64> {
        let mut p = Self::bc_find_cell(d, m, idx) + usize::from(m.child_ptr_size);
        if m.leaf {
            // skip the payload size
            loop {
                let b = *d.get(p).ok_or_else(corrupt)?;
                p += 1;
                if b < 0x80 {
                    break;
                }
                if p >= d.len() {
                    return Err(corrupt());
                }
            }
        }
        let (v, _) = varint64(d, p).ok_or_else(corrupt)?;
        Ok(v as i64)
    }

    /// `sqlite3BtreeTableMoveto(pCur, intKey, biasRight, &res)`.
    pub fn bc_table_moveto(&mut self, cur: &mut BtCursor, int_key: i64, bias_right: bool) -> Result<i32> {
        // If the cursor is already positioned at the point we are trying to move to, then
        // just return without doing any work.
        if cur.e_state == CURSOR_VALID && cur.cur_flags & BTCF_VALID_NKEY != 0 && self.bc_live(cur) {
            if cur.info.n_key == int_key {
                return Ok(0);
            }
            if cur.info.n_key < int_key {
                if cur.cur_flags & BTCF_AT_LAST != 0 {
                    return Ok(-1);
                }
                // If the requested key is one more than the previous key, then try to get
                // there using sqlite3BtreeNext() rather than a full binary search.
                if cur.info.n_key.wrapping_add(1) == int_key {
                    if self.bc_next(cur)? {
                        self.bc_get_cell_info(cur)?;
                        if cur.info.n_key == int_key {
                            return Ok(0);
                        }
                    }
                }
            }
        }
        if !self.bc_move_to_root(cur)? {
            return Ok(-1);
        }
        loop {
            let m = *cur.pg();
            let mut lwr: i32 = 0;
            let mut upr: i32 = i32::from(m.n_cell) - 1;
            let mut idx: i32 = if bias_right { upr } else { upr >> 1 };
            let c: i32;
            let mut found_interior = false;
            {
                let d = self.pager.read_page(m.pgno)?;
                loop {
                    let n_cell_key = Self::bc_cell_int_key(d, &m, idx as usize)?;
                    if n_cell_key < int_key {
                        lwr = idx + 1;
                        if lwr > upr {
                            c = -1;
                            break;
                        }
                    } else if n_cell_key > int_key {
                        upr = idx - 1;
                        if lwr > upr {
                            c = 1;
                            break;
                        }
                    } else {
                        cur.ix = idx as u16;
                        if !m.leaf {
                            lwr = idx;
                            found_interior = true;
                            c = 0;
                            break;
                        }
                        cur.cur_flags |= BTCF_VALID_NKEY;
                        cur.info.n_key = n_cell_key;
                        cur.info.n_size = 0;
                        return Ok(0);
                    }
                    idx = (lwr + upr) >> 1;
                }
            }
            if !found_interior && m.leaf {
                cur.ix = idx as u16;
                cur.info.n_size = 0;
                return Ok(c);
            }
            // moveto_table_next_layer
            let chld = self.bc_child(m, lwr as usize)?;
            cur.ix = lwr as u16;
            if let Err(e) = self.bc_move_to_child(cur, chld) {
                cur.info.n_size = 0;
                return Err(e);
            }
        }
    }

    /// `indexCellCompare`: 99 when the cell does not lie entirely on the page.
    fn bc_index_cell_compare(&mut self, m: PageMeta, idx: usize, key: &mut dyn IdxKeyCompare) -> Result<i32> {
        let d = self.pager.read_page(m.pgno)?;
        let cell = Self::bc_find_cell(d, &m, idx) + usize::from(m.child_ptr_size);
        if cell + 2 > d.len() {
            return Ok(99);
        }
        let mut n_cell = usize::from(d[cell]);
        if n_cell <= usize::from(m.max1byte_payload) {
            if cell + n_cell >= d.len() {
                return Ok(99);
            }
            Ok(key.compare(&d[cell + 1..cell + 1 + n_cell]))
        } else if d[cell + 1] & 0x80 == 0 && {
            n_cell = ((n_cell & 0x7f) << 7) + usize::from(d[cell + 1]);
            n_cell <= usize::from(m.max_local)
        } {
            if cell + n_cell >= d.len() {
                return Ok(99);
            }
            Ok(key.compare(&d[cell + 2..cell + 2 + n_cell]))
        } else {
            Ok(99)
        }
    }

    /// `cursorOnLastPage`
    fn bc_cursor_on_last_page(cur: &BtCursor) -> bool {
        for i in 0..cur.i_page as usize {
            if cur.ai_idx[i] < cur.ap[i].n_cell {
                return false;
            }
        }
        true
    }

    /// `sqlite3BtreeIndexMoveto(pCur, pIdxKey, &res)`. The caller checks the error state of
    /// the key afterwards where C does.
    pub fn bc_index_moveto(&mut self, cur: &mut BtCursor, key: &mut dyn IdxKeyCompare) -> Result<i32> {
        key.clear_error();
        let mut bypass = false;
        // Check to see if we can skip a lot of work.
        if cur.e_state == CURSOR_VALID && self.bc_live(cur) && cur.pg().leaf && Self::bc_cursor_on_last_page(cur) {
            let m = *cur.pg();
            if cur.ix == m.n_cell - 1 {
                let c = self.bc_index_cell_compare(m, usize::from(cur.ix), key)?;
                if c <= 0 && !key.has_error() {
                    return Ok(c);
                }
            }
            if cur.i_page > 0 && self.bc_index_cell_compare(m, 0, key)? <= 0 && !key.has_error() {
                cur.cur_flags &= !(BTCF_VALID_OVFL | BTCF_AT_LAST);
                bypass = true;
            } else {
                key.clear_error();
            }
        }
        if !bypass && !self.bc_move_to_root(cur)? {
            return Ok(-1);
        }
        loop {
            let m = *cur.pg();
            let mut lwr: i32 = 0;
            let mut upr: i32 = i32::from(m.n_cell) - 1;
            let mut idx: i32 = upr >> 1;
            let mut c: i32;
            loop {
                // Some(c): compared on the page; None: the record flows over onto overflow
                // pages.
                let on_page: Option<i32> = {
                    let d = self.pager.read_page(m.pgno)?;
                    let cell = Self::bc_find_cell(d, &m, idx as usize) + usize::from(m.child_ptr_size);
                    if cell + 2 > d.len() {
                        cur.info.n_size = 0;
                        return Err(corrupt());
                    }
                    let mut n_cell = usize::from(d[cell]);
                    if n_cell <= usize::from(m.max1byte_payload) {
                        if cell + n_cell >= d.len() {
                            cur.info.n_size = 0;
                            return Err(corrupt());
                        }
                        Some(key.compare(&d[cell + 1..cell + 1 + n_cell]))
                    } else if d[cell + 1] & 0x80 == 0
                        && {
                            n_cell = ((n_cell & 0x7f) << 7) + usize::from(d[cell + 1]);
                            n_cell <= usize::from(m.max_local)
                        }
                        && cell + n_cell < d.len()
                    {
                        Some(key.compare(&d[cell + 2..cell + 2 + n_cell]))
                    } else {
                        None
                    }
                };
                c = match on_page {
                    Some(c) => c,
                    None => {
                        cur.info = match self.bc_parse_cell(m, idx as usize) {
                            Ok(i) => i,
                            Err(e) => {
                                cur.info.n_size = 0;
                                return Err(e);
                            }
                        };
                        let n_cell = cur.info.n_key;
                        let usable = i64::from(self.pager.usable_size());
                        if n_cell < 2 || n_cell / usable > i64::from(self.pager.page_count()) {
                            cur.info.n_size = 0;
                            return Err(corrupt());
                        }
                        let mut cell_key = vec![0u8; n_cell as usize];
                        cur.ix = idx as u16;
                        let rc = self.bc_access_payload(cur, 0, &mut cell_key);
                        cur.cur_flags &= !BTCF_VALID_OVFL;
                        if let Err(e) = rc {
                            cur.info.n_size = 0;
                            return Err(e);
                        }
                        key.compare_general(&cell_key)
                    }
                };
                if c < 0 {
                    lwr = idx + 1;
                } else if c > 0 {
                    upr = idx - 1;
                } else {
                    cur.ix = idx as u16;
                    cur.info.n_size = 0;
                    if key.has_error() {
                        return Err(corrupt());
                    }
                    return Ok(0);
                }
                if lwr > upr {
                    break;
                }
                idx = (lwr + upr) >> 1;
            }
            if m.leaf {
                cur.ix = idx as u16;
                cur.info.n_size = 0;
                return Ok(c);
            }
            let chld = self.bc_child(m, lwr as usize)?;
            // (an in-lined moveToChild with aiIdx = lwr)
            cur.ix = lwr as u16;
            if let Err(e) = self.bc_move_to_child(cur, chld) {
                cur.info.n_size = 0;
                return Err(e);
            }
        }
    }

    /// `btreeNext`: `Ok(false)` is `SQLITE_DONE`.
    fn bc_btree_next(&mut self, cur: &mut BtCursor) -> Result<bool> {
        if cur.e_state != CURSOR_VALID {
            self.bc_restore_cursor_position(cur)?;
            if cur.e_state == CURSOR_INVALID {
                return Ok(false);
            }
            if cur.e_state == CURSOR_SKIPNEXT {
                cur.e_state = CURSOR_VALID;
                if cur.skip_next > 0 {
                    return Ok(true);
                }
            }
        }
        if !self.bc_live(cur) || cur.i_page < 0 {
            cur.e_state = CURSOR_INVALID;
            return Ok(false);
        }
        let mut m = *cur.pg();
        cur.ix += 1;
        let idx = cur.ix;
        if idx >= m.n_cell {
            if !m.leaf {
                let child = self.bc_child(m, usize::from(m.n_cell))?;
                self.bc_move_to_child(cur, child)?;
                return self.bc_move_to_leftmost(cur).map(|()| true);
            }
            loop {
                if cur.i_page == 0 {
                    cur.e_state = CURSOR_INVALID;
                    return Ok(false);
                }
                self.bc_move_to_parent(cur);
                m = *cur.pg();
                if cur.ix < m.n_cell {
                    break;
                }
            }
            if m.int_key {
                return self.bc_next(cur);
            }
            return Ok(true);
        }
        if m.leaf {
            Ok(true)
        } else {
            self.bc_move_to_leftmost(cur).map(|()| true)
        }
    }

    /// `sqlite3BtreeNext(pCur, flags)`: `Ok(true)` is `SQLITE_OK`, `Ok(false)` is
    /// `SQLITE_DONE` (the cursor is then `CURSOR_INVALID`).
    #[inline]
    pub fn bc_next(&mut self, cur: &mut BtCursor) -> Result<bool> {
        cur.info.n_size = 0;
        cur.cur_flags &= !(BTCF_VALID_NKEY | BTCF_VALID_OVFL);
        if cur.e_state != CURSOR_VALID {
            return self.bc_btree_next(cur);
        }
        let m = *cur.pg();
        cur.ix += 1;
        if cur.ix >= m.n_cell {
            cur.ix -= 1;
            return self.bc_btree_next(cur);
        }
        if m.leaf { Ok(true) } else { self.bc_move_to_leftmost(cur).map(|()| true) }
    }

    /// `btreePrevious`
    fn bc_btree_previous(&mut self, cur: &mut BtCursor) -> Result<bool> {
        if cur.e_state != CURSOR_VALID {
            self.bc_restore_cursor_position(cur)?;
            if cur.e_state == CURSOR_INVALID {
                return Ok(false);
            }
            if cur.e_state == CURSOR_SKIPNEXT {
                cur.e_state = CURSOR_VALID;
                if cur.skip_next < 0 {
                    return Ok(true);
                }
            }
        }
        if !self.bc_live(cur) || cur.i_page < 0 {
            cur.e_state = CURSOR_INVALID;
            return Ok(false);
        }
        let m = *cur.pg();
        if !m.leaf {
            let child = self.bc_child(m, usize::from(cur.ix))?;
            self.bc_move_to_child(cur, child)?;
            self.bc_move_to_rightmost(cur)?;
            Ok(true)
        } else {
            while cur.ix == 0 {
                if cur.i_page == 0 {
                    cur.e_state = CURSOR_INVALID;
                    return Ok(false);
                }
                self.bc_move_to_parent(cur);
            }
            cur.ix -= 1;
            let m = *cur.pg();
            if m.int_key && !m.leaf { self.bc_previous(cur) } else { Ok(true) }
        }
    }

    /// `sqlite3BtreePrevious(pCur, flags)`: `Ok(false)` is `SQLITE_DONE`.
    #[inline]
    pub fn bc_previous(&mut self, cur: &mut BtCursor) -> Result<bool> {
        cur.cur_flags &= !(BTCF_AT_LAST | BTCF_VALID_OVFL | BTCF_VALID_NKEY);
        cur.info.n_size = 0;
        if cur.e_state != CURSOR_VALID || cur.ix == 0 || !cur.pg().leaf {
            return self.bc_btree_previous(cur);
        }
        cur.ix -= 1;
        Ok(true)
    }

    /// `sqlite3BtreeCount`
    pub fn bc_count(&mut self, cur: &mut BtCursor) -> Result<i64> {
        let mut n_entry: i64 = 0;
        if !self.bc_move_to_root(cur)? {
            return Ok(0);
        }
        loop {
            self.check_interrupted()?;
            let mut m = *cur.pg();
            if m.leaf || !m.int_key {
                n_entry += i64::from(m.n_cell);
            }
            if m.leaf {
                loop {
                    if cur.i_page == 0 {
                        self.bc_move_to_root(cur)?;
                        return Ok(n_entry);
                    }
                    self.bc_move_to_parent(cur);
                    if cur.ix < cur.pg().n_cell {
                        break;
                    }
                }
                cur.ix += 1;
                m = *cur.pg();
            }
            let child = self.bc_child(m, usize::from(cur.ix))?;
            self.bc_move_to_child(cur, child)?;
        }
    }

    /// `saveCursorPosition` (with `saveCursorKey`): the cursor gives its pages back and
    /// remembers the key it was on (`CURSOR_REQUIRESEEK`).
    pub fn bc_save_cursor_position(&mut self, cur: &mut BtCursor) -> Result<()> {
        if cur.e_state == CURSOR_SKIPNEXT {
            cur.e_state = CURSOR_VALID;
        } else {
            cur.skip_next = 0;
        }
        // saveCursorKey
        if cur.cur_int_key {
            cur.n_key = self.bc_integer_key(cur)?;
        } else {
            let n = self.bc_payload_size(cur)?;
            cur.n_key = i64::from(n);
            let mut k = vec![0u8; n as usize];
            self.bc_access_payload(cur, 0, &mut k)?;
            cur.p_key = Some(k);
        }
        self.bc_release_all_cursor_pages(cur);
        cur.e_state = CURSOR_REQUIRESEEK;
        cur.cur_flags &= !(BTCF_VALID_NKEY | BTCF_VALID_OVFL | BTCF_AT_LAST);
        Ok(())
    }

    /// The part of `saveAllCursors` that concerns one other cursor on the b-tree that is
    /// about to be written: a positioned cursor saves its position, any other gives its
    /// pages back.
    pub fn bc_save_for_write(&mut self, cur: &mut BtCursor) -> Result<()> {
        if cur.e_state == CURSOR_VALID || cur.e_state == CURSOR_SKIPNEXT {
            if self.bc_live(cur) {
                return self.bc_save_cursor_position(cur);
            }
            Ok(())
        } else {
            self.bc_release_all_cursor_pages(cur);
            Ok(())
        }
    }

    /// The cursor a write went through, when the write was done by a function that does not
    /// know this cursor: its pages are given back and it is `CURSOR_INVALID`.
    pub fn bc_invalidate(&mut self, cur: &mut BtCursor) {
        self.bc_release_all_cursor_pages(cur);
        cur.p_key = None;
        cur.e_state = CURSOR_INVALID;
        cur.info.n_size = 0;
        cur.cur_flags &= !(BTCF_VALID_NKEY | BTCF_VALID_OVFL | BTCF_AT_LAST);
    }

    /// The saved key of a cursor in `CURSOR_REQUIRESEEK`: `(pKey, nKey)`.
    pub fn bc_saved_key<'a>(&self, cur: &'a BtCursor) -> (Option<&'a [u8]>, i64) {
        (cur.p_key.as_deref(), cur.n_key)
    }

    /// `btreeRestoreCursorPosition` for a table cursor; an index cursor is restored by the
    /// machine through [`Self::bc_restore_index_with`], which has the key comparator.
    fn bc_restore_cursor_position(&mut self, cur: &mut BtCursor) -> Result<()> {
        if cur.e_state < CURSOR_REQUIRESEEK {
            return Ok(());
        }
        if cur.e_state == CURSOR_FAULT {
            return Err(corrupt());
        }
        if cur.p_key.is_some() {
            // An index cursor: the owner restores it (it holds the KeyInfo); a cursor that
            // was not restored behaves as one whose entry is gone.
            cur.e_state = CURSOR_INVALID;
            return Ok(());
        }
        cur.e_state = CURSOR_INVALID;
        let skip_next = self.bc_table_moveto(cur, cur.n_key, false)?;
        cur.p_key = None;
        if skip_next != 0 {
            cur.skip_next = skip_next;
        }
        if cur.skip_next != 0 && cur.e_state == CURSOR_VALID {
            cur.e_state = CURSOR_SKIPNEXT;
        }
        Ok(())
    }

    /// `btreeRestoreCursorPosition` of an index cursor: `moveto` runs
    /// `sqlite3BtreeIndexMoveto` for the saved key (the owner of the cursor has the `KeyInfo`
    /// to unpack it with: `btreeMoveto`).
    pub fn bc_restore_index_with(
        &mut self,
        cur: &mut BtCursor,
        moveto: &mut dyn FnMut(&mut Btree, &mut BtCursor, &[u8]) -> Result<i32>,
    ) -> Result<()> {
        if cur.e_state < CURSOR_REQUIRESEEK {
            return Ok(());
        }
        if cur.e_state == CURSOR_FAULT {
            return Err(corrupt());
        }
        let Some(k) = cur.p_key.take() else {
            return self.bc_restore_cursor_position(cur);
        };
        cur.e_state = CURSOR_INVALID;
        let skip_next = match moveto(self, cur, &k) {
            Ok(r) => r,
            Err(e) => {
                cur.p_key = Some(k);
                cur.e_state = CURSOR_REQUIRESEEK;
                return Err(e);
            }
        };
        if skip_next != 0 {
            cur.skip_next = skip_next;
        }
        if cur.skip_next != 0 && cur.e_state == CURSOR_VALID {
            cur.e_state = CURSOR_SKIPNEXT;
        }
        Ok(())
    }

    /// `sqlite3BtreeCursorRestore(pCur, &differentRow)` of a table cursor (see
    /// [`Self::bc_restore_index_with`] for index cursors): returns `*pDifferentRow`.
    pub fn bc_cursor_restore(&mut self, cur: &mut BtCursor) -> Result<bool> {
        self.bc_restore_cursor_position(cur)?;
        Ok(cur.e_state != CURSOR_VALID)
    }

    /// `sqlite3BtreeRowCountEst`
    pub fn bc_row_count_est(&self, cur: &BtCursor) -> i64 {
        if cur.e_state != CURSOR_VALID {
            return 0;
        }
        if !cur.pg().leaf {
            return -1;
        }
        let mut n = i64::from(cur.pg().n_cell);
        for i in 0..cur.i_page as usize {
            n *= i64::from(cur.ap[i].n_cell) + 1;
        }
        n
    }

    /// True when a cursor in `CURSOR_REQUIRESEEK` carries an index key.
    #[inline]
    pub fn bc_needs_index_restore(&self, cur: &BtCursor) -> bool {
        cur.e_state == CURSOR_REQUIRESEEK && cur.p_key.is_some()
    }
}
