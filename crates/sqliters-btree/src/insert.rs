//! Port of `sqlite3BtreeInsert()` of C SQLite 3.54.1 (`sqlite/src/btree.c`) and of what it
//! calls up to `balance()`: `fillInCell` (local / overflow split of the payload, overflow
//! chain allocation with its `nearby` hints and pointer-map entries), the overwrite
//! optimisation (`btreeOverwriteCell`, `btreeOverwriteContent`, `btreeOverwriteOverflowCell`),
//! the replace path (`BTREE_CLEAR_CELL`, the same-size in-place copy, `dropCell`) and
//! `insertCellFast` → `allocateSpace` (freeblock first-fit, gap, `defragmentPage`).
//!
//! C works on a `BtCursor`; here every entry point first finds the page and the cell index the
//! cursor would be on (`sqlite3BtreeTableMoveto` / `sqlite3BtreeIndexMoveto`, or the position a
//! caller already holds), recording the page path in a [`BalCursor`], and then performs C's
//! steps on the raw page. A cell that does not fit becomes an overflow cell of the page and
//! [`Btree::balance`] (`balance.rs`) redistributes — `balance_quick`, `balance_nonroot`,
//! `balance_deeper` exactly as for C.
//!
//! The common case — a new cell that fits into the gap of a page without freeblocks — is, as
//! in C, nothing but `insertCell`: `allocateSpace` then returns `top - size`, the cell is
//! written there and its pointer is inserted. That case is answered without computing the free
//! space of the page (`MemPage.nFree`): a cell that fits into the gap cannot be an overflow
//! cell. Only when the gap is too small is `nFree` computed, to choose between an overflow
//! cell and `defragmentPage` the way C does.

#![allow(unexpected_cfgs)]

use super::*;
use crate::balance::{
    BalCursor, CellInfo, OvflCells, PgKind, SLOT_GAP, allocate_space, compute_free_space,
    drop_cell, get2, get4, hdr_of, parse_cell, put2,
};

#[cold]
fn corrupt(msg: &'static str) -> SqliteError {
    SqliteError::corrupt(msg)
}

/// A cell as `fillInCell` builds it in `pBt->pTmpSpace`: header (child pointer on interior
/// pages, payload size, rowid on table leaves), the local part of the payload and the number
/// of the first overflow page.
pub(crate) struct NewCell<'a> {
    head: [u8; 24],
    n_head: usize,
    local: &'a [u8],
    /// First overflow page, 0 when the whole payload is local.
    ovfl: Pgno,
    /// Bytes the cell takes on a page (`*pnSize`, at least 4).
    size: usize,
}

impl NewCell<'_> {
    /// Copies the cell to `dst` (`dst.len() == self.size`).
    #[inline]
    fn write(&self, dst: &mut [u8]) {
        dst[..self.n_head].copy_from_slice(&self.head[..self.n_head]);
        let mut p = self.n_head;
        dst[p..p + self.local.len()].copy_from_slice(self.local);
        p += self.local.len();
        if self.ovfl != 0 {
            dst[p..p + 4].copy_from_slice(&self.ovfl.to_be_bytes());
            p += 4;
        }
        // `if( n<4 ){ n = 4; pPayload[nPayload] = 0; }`
        dst[p..].fill(0);
    }
}

/// Bytes of an `n_payload`-byte payload that stay on a page of kind `k`
/// (`btreePayloadToLocal`).
#[inline]
fn local_len(k: &PgKind, usable: u32, n_payload: u32) -> u32 {
    if n_payload <= k.max_local {
        n_payload
    } else {
        let surplus = k.min_local + (n_payload - k.min_local) % (usable - 4);
        if surplus <= k.max_local {
            surplus
        } else {
            k.min_local
        }
    }
}

/// `sqlite3BtreeTableMoveto` on one leaf: `(idx, found)` — the cell holding `rowid`, or the
/// index a new cell for `rowid` gets. The last cell is looked at first (appends).
#[inline]
fn table_leaf_search(
    d: &[u8],
    hdr: usize,
    usable: usize,
    n_cell: usize,
    rowid: i64,
) -> Result<(usize, bool)> {
    if n_cell == 0 {
        return Ok((0, false));
    }
    let ptr0 = hdr + 8;
    let ptr_end = ptr0 + 2 * n_cell;
    if ptr_end > usable || d.len() < usable {
        return Err(corrupt("cell pointer array past page end"));
    }
    let key_at = |i: usize| -> Result<i64> {
        let off = get2(d, ptr0 + 2 * i);
        if off < ptr_end || off >= usable {
            return Err(corrupt("table leaf cell offset out of range"));
        }
        // Skip the payload-size varint (at most 9 bytes), then read the rowid.
        let c = &d[off..usable];
        let mut n = 0;
        while n < 8 && n < c.len() && c[n] & 0x80 != 0 {
            n += 1;
        }
        n += 1;
        let Some(rest) = c.get(n..) else {
            return Err(corrupt("table leaf cell past page end"));
        };
        Ok(get_varint_i64(rest)?.0)
    };
    let last = key_at(n_cell - 1)?;
    if rowid > last {
        return Ok((n_cell, false));
    }
    if rowid == last {
        return Ok((n_cell - 1, true));
    }
    let mut lo = 0usize;
    let mut hi = n_cell - 1;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let k = key_at(mid)?;
        match k.cmp(&rowid) {
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid,
            Ordering::Equal => return Ok((mid, true)),
        }
    }
    Ok((lo, false))
}

/// `sqlite3BtreeIndexMoveto` on one index page, comparing on the raw page: `(idx, found)` —
/// the cell equal to the key, or (C's `lwr` when the search ends) the slot of a new cell on a
/// leaf / the child to descend into on an interior page. `None` when a cell cannot be compared
/// on the page (it continues on overflow pages, or does not parse).
#[inline]
fn index_page_search_raw(
    d: &[u8],
    hdr: usize,
    usable: usize,
    k: &PgKind,
    key: &RawRecordKey<'_>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<(usize, bool)>> {
    let n_cell = get2(d, hdr + 3);
    if n_cell == 0 {
        return Ok(Some((0, false)));
    }
    let ptr0 = hdr + 8 + k.child_ptr;
    let ptr_end = ptr0 + 2 * n_cell;
    if ptr_end > usable || d.len() < usable {
        return Ok(None);
    }
    let mut lwr = 0isize;
    let mut upr = n_cell as isize - 1;
    loop {
        let idx = ((lwr + upr) >> 1) as usize;
        let off = get2(d, ptr0 + 2 * idx) + k.child_ptr;
        if off < ptr_end || off >= usable {
            return Ok(None);
        }
        // Payload size: one byte in the common case.
        let b0 = d[off];
        let (n_payload, n1) = if b0 < 0x80 {
            (u32::from(b0), 1)
        } else {
            match get_varint(&d[off..usable]) {
                Ok((v, n)) => match u32::try_from(v) {
                    Ok(v) => (v, n),
                    Err(_) => return Ok(None),
                },
                Err(_) => return Ok(None),
            }
        };
        if n_payload > k.max_local {
            return Ok(None);
        }
        let start = off + n1;
        let end = start + n_payload as usize;
        if end > usable {
            return Ok(None);
        }
        let c = match key.compare_stored(&d[start..end], orders, enc) {
            Ok(c) => c,
            Err(e) if e.code == ErrorCode::Corrupt => return Ok(None),
            Err(e) => return Err(e),
        };
        match c {
            Ordering::Less => lwr = idx as isize + 1,
            Ordering::Greater => upr = idx as isize - 1,
            Ordering::Equal => return Ok(Some((idx, true))),
        }
        if lwr > upr {
            return Ok(Some((lwr as usize, false)));
        }
    }
}

/// What the caller of a table insert knows (the state of C's cursor when `OP_Insert` runs).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableInsertMode {
    /// Insert or replace. `overwrite`: when the row exists, C's cursor is on it with its cell
    /// parsed (`BTCF_ValidNKey`, `info.nSize != 0`), so a record of the same size is written
    /// over the old one (`btreeOverwriteCell`).
    Upsert { overwrite: bool },
    /// Insert only when the rowid is absent (the engine's `OP_NotExists` + `OP_Insert` pair).
    Unique,
}

/// A write cursor that only appends (C: a cursor driven by `OP_SeekEnd` followed by an insert
/// with `OPFLAG_USESEEKRESULT`). It stays on the right-most leaf between appends and is
/// re-seated from the root after an append that had to balance, as C's cursor is
/// (`CURSOR_INVALID` after `balance()`, then `sqlite3BtreeLast`). While it is in use nothing
/// else may modify its tree.
pub struct AppendCursor {
    root: Pgno,
    cur: BalCursor,
    valid: bool,
    /// The read cursors on the tree have been saved (none can be opened in between).
    saved: bool,
}

impl AppendCursor {
    /// `bulk`: the cursor carries `BTREE_BULKLOAD` (`OPFLAG_BULKCSR`).
    pub fn new(root: Pgno, bulk: bool) -> Self {
        let mut cur = BalCursor::new(root);
        cur.bulk = bulk;
        Self {
            root,
            cur,
            valid: false,
            saved: false,
        }
    }
}

impl Btree {
    /// Decision trace only: `ins` inside an insert, `del` inside a delete (C: which of
    /// `sqlite3BtreeInsert` / `sqlite3BtreeDelete` is running).
    #[inline]
    #[allow(dead_code)]
    pub(crate) const fn tr_ctx(&self) -> &'static str {
        if self.tr_ins { "ins" } else { "del" }
    }

    /// The overflow part of `fillInCell`: stores `rest` (the payload after its local part) on
    /// newly allocated overflow pages and returns the first one. Every page is allocated near
    /// the previous page of the chain (`nearby` = 0 for the first; in auto-vacuum mode the page
    /// after it, pointer-map and pending-byte pages skipped) and gets its pointer-map entry at
    /// once — the first page as `PTRMAP_OVERFLOW1` of page 0, corrected by `insertCell` (or by
    /// the balancer) once the page of the cell is known.
    fn fill_overflow_chain(&mut self, rest: &[u8]) -> Result<Pgno> {
        let usable = self.pager.usable_size();
        let chunk_cap = usable as usize - 4;
        let auto_vac = self.is_auto_vacuum();
        let lock_pg = self.lock_byte_page();
        let mut first: Pgno = 0;
        let mut prev: Pgno = 0;
        // `sqlite3BtreeTransferRow` (`BTREE_PREFORMAT`): every overflow page of a transferred
        // cell is allocated with `nearby = 0`.
        let transfer = self.preformat;
        // `pToRelease`: the overflow page being filled stays referenced until the next one is
        // allocated ("releasePage(pToRelease); pToRelease = pOvfl").
        let mut to_release: Pgno = 0;
        for chunk in rest.chunks(chunk_cap) {
            let mut nearby = prev;
            if transfer {
                nearby = 0;
            } else if auto_vac {
                nearby += 1;
                while is_ptrmap_page(nearby, usable, lock_pg) || nearby == lock_pg {
                    nearby += 1;
                }
            }
            let pg = self.allocate_btree_page(AllocMode::Near(nearby))?;
            if auto_vac {
                let e_type = if prev != 0 {
                    PtrmapType::Overflow2
                } else {
                    PtrmapType::Overflow1
                };
                self.ptrmap_put(pg, e_type, prev)?;
            }
            if prev == 0 {
                first = pg;
            } else {
                self.pager.write_page(prev)?[..4].copy_from_slice(&pg.to_be_bytes());
            }
            if to_release != 0 {
                self.unpin(to_release);
            }
            to_release = pg;
            let d = self.pager.write_page(pg)?;
            d[..4].fill(0);
            d[4..4 + chunk.len()].copy_from_slice(chunk);
            prev = pg;
        }
        if to_release != 0 {
            self.unpin(to_release);
        }
        Ok(first)
    }

    /// `fillInCell(pPage, pCell, pX, &szNew)` for a page of kind `k`. `child` is the left child
    /// pointer of the cell being replaced on an interior index page.
    fn fill_in_cell<'a>(
        &mut self,
        k: &PgKind,
        child: [u8; 4],
        rowid: i64,
        payload: &'a [u8],
    ) -> Result<NewCell<'a>> {
        let usable = self.pager.usable_size();
        let n_payload = u32::try_from(payload.len())
            .map_err(|_| SqliteError::new(ErrorCode::TooBig, "row payload exceeds 4GiB"))?;
        let mut head = [0u8; 24];
        let mut n = 0;
        if !k.leaf {
            head[..4].copy_from_slice(&child);
            n = 4;
        }
        n += put_varint(&mut head[n..], u64::from(n_payload));
        if k.int_key {
            n += put_varint(&mut head[n..], rowid as u64);
        }
        let n_local = local_len(k, usable, n_payload) as usize;
        if self.preformat && self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|xf|row|pay={} local={} o={}",
                n_payload,
                n_local,
                if n_local == payload.len() {
                    "local"
                } else {
                    "overflow"
                }
            );
        }
        if n_local == payload.len() {
            return Ok(NewCell {
                head,
                n_head: n,
                local: payload,
                ovfl: 0,
                size: (n + n_local).max(4),
            });
        }
        let first = self.fill_overflow_chain(&payload[n_local..])?;
        Ok(NewCell {
            head,
            n_head: n,
            local: &payload[..n_local],
            ovfl: first,
            size: n + n_local + 4,
        })
    }

    /// `insertCellFast(pPage, i, pCell, sz)` on page `pgno` holding `n_cell` cells. Returns
    /// `false` when the cell does not fit (`sz+2 > pPage->nFree`): the page is then not
    /// touched and the caller makes the cell an overflow cell.
    fn page_insert_cell(
        &mut self,
        pgno: Pgno,
        k: &PgKind,
        n_cell: usize,
        i: usize,
        cell: &NewCell<'_>,
    ) -> Result<bool> {
        let usable = self.pager.usable_size() as usize;
        let hdr = hdr_of(pgno);
        let sz = cell.size;
        let ptr0 = hdr + 8 + k.child_ptr;
        if i > n_cell {
            return Err(corrupt("insertCell index out of range"));
        }
        // Read-only part: does the cell fit, and where would `allocateSpace` look?
        let (gap_only, n_free) = {
            let d = self.pager.read_page(pgno)?;
            if d.len() < usable || ptr0 + 2 * n_cell + 2 > d.len() {
                return Err(corrupt("b-tree page buffer too small"));
            }
            let gap = ptr0 + 2 * n_cell;
            let mut top = get2(d, hdr + 5);
            if top == 0 && usable == 65_536 {
                top = 65_536;
            }
            let has_freeblock = d[hdr + 1] != 0 || d[hdr + 2] != 0;
            if gap + 2 + sz <= top && top <= usable {
                // The gap alone takes the cell, so `sz+2 <= nFree`; `allocateSpace` only
                // needs `nFree` when it has to defragment.
                (!has_freeblock, 0)
            } else {
                let n_free = compute_free_space(d, hdr, k.child_ptr, n_cell, usable)?;
                if sz as i32 + 2 > n_free {
                    if self.dtrace_persistent() {
                        sqliters_types::dtrace!(
                            "TR|ins|slot|ctx={} pg={} i={} sz={} o=overflow",
                            self.tr_ctx(),
                            pgno,
                            i,
                            sz
                        );
                    }
                    return Ok(false);
                }
                (false, n_free)
            }
        };
        let mut tmp = std::mem::take(&mut self.bal_tmp);
        let res = (|| -> Result<(usize, u8)> {
            let d = self.pager.write_page(pgno)?;
            let (off, how) = if gap_only {
                let mut top = get2(d, hdr + 5);
                if top == 0 {
                    top = 65_536;
                }
                let off = top - sz;
                put2(d, hdr + 5, off);
                (off, SLOT_GAP)
            } else {
                allocate_space(d, hdr, k, usable, n_cell, n_free, sz, &mut tmp)?
            };
            if off + sz > d.len() {
                return Err(corrupt("allocateSpace returned a slot past page end"));
            }
            cell.write(&mut d[off..off + sz]);
            let ins = ptr0 + 2 * i;
            d.copy_within(ins..ptr0 + 2 * n_cell, ins + 2);
            put2(d, ins, off);
            put2(d, hdr + 3, n_cell + 1);
            Ok((off, how))
        })();
        self.bal_tmp = tmp;
        let (_off, _how) = res?;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|ins|slot|ctx={} pg={} i={} sz={} o={} off={}",
                self.tr_ctx(),
                pgno,
                i,
                sz,
                crate::balance::slot_name(_how),
                _off
            );
        }
        if cell.ovfl != 0 && self.is_auto_vacuum() {
            // `ptrmapPutOvflPtr`: the overflow chain now belongs to this page.
            self.ptrmap_put(cell.ovfl, PtrmapType::Overflow1, pgno)?;
        }
        Ok(true)
    }

    /// The end of `sqlite3BtreeInsert` for a new entry: `insertCellFast` at index `idx` of the
    /// page `cur` is on and, when the page then has an overflow cell, `balance()`.
    fn insert_new_cell(
        &mut self,
        cur: &mut BalCursor,
        k: &PgKind,
        n_cell: usize,
        idx: usize,
        cell: &NewCell<'_>,
    ) -> Result<()> {
        let pgno = cur.page();
        if self.page_insert_cell(pgno, k, n_cell, idx, cell)? {
            return Ok(());
        }
        let mut ovfl = OvflCells::default();
        ovfl.push_with(idx, cell.size, |dst| cell.write(dst))?;
        cur.set_ovfl(cur.i_page, ovfl);
        self.balance(cur)
    }

    /// `btreeOverwriteCell`: the cell at `off` of page `pgno` (parsed as `info`) has a payload
    /// of exactly `payload.len()` bytes; write `payload` over it, local part and overflow pages,
    /// making a page writable only when its bytes change (`btreeOverwriteContent`).
    fn overwrite_cell(
        &mut self,
        pgno: Pgno,
        cell_offset: usize,
        off: usize,
        info: &CellInfo,
        payload: &[u8],
    ) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let n_local = info.n_local as usize;
        let pay = off + info.n_hdr;
        let (same, ovfl) = {
            let d = self.pager.read_page(pgno)?;
            // `pCur->info.pPayload + pCur->info.nLocal > pPage->aDataEnd
            //  || pCur->info.pPayload < pPage->aData + pPage->cellOffset`
            if pay + n_local > d.len() || pay < cell_offset || n_local > payload.len() {
                return Err(corrupt("cell to overwrite lies outside its page"));
            }
            let same = d[pay..pay + n_local] == payload[..n_local];
            let ovfl = if n_local == payload.len() {
                0
            } else {
                if pay + n_local + 4 > d.len() {
                    return Err(corrupt("cell to overwrite lies outside its page"));
                }
                get4(d, pay + n_local)
            };
            (same, ovfl)
        };
        if !same {
            self.pager.write_page(pgno)?[pay..pay + n_local].copy_from_slice(&payload[..n_local]);
        }
        if n_local == payload.len() {
            return Ok(());
        }
        // `btreeOverwriteOverflowCell`
        let n_total = payload.len();
        let chunk = usable - 4;
        let page_count = self.pager.page_count();
        let mut i_offset = n_local;
        let mut ovfl_pgno = ovfl;
        loop {
            if ovfl_pgno < 2 || ovfl_pgno > page_count {
                return Err(SqliteError::corrupt(format!(
                    "overflow page {ovfl_pgno} out of range"
                )));
            }
            let more = i_offset + chunk < n_total;
            let n = if more { chunk } else { n_total - i_offset };
            let (next, same) = {
                let raw = self.pager.read_page_unchecked(ovfl_pgno)?;
                (
                    get4(raw, 0),
                    raw[4..4 + n] == payload[i_offset..i_offset + n],
                )
            };
            if !same {
                self.pager.write_page(ovfl_pgno)?[4..4 + n]
                    .copy_from_slice(&payload[i_offset..i_offset + n]);
            }
            if more {
                ovfl_pgno = next;
            }
            i_offset += n;
            if i_offset >= n_total {
                break;
            }
        }
        Ok(())
    }

    /// Records that the table row `rowid` of `root_pgno` was replaced where it was, without
    /// `balance()` ([`Btree::in_place_row`]).
    #[inline(never)]
    fn note_in_place_row(&mut self, k: &PgKind, root_pgno: Pgno, rowid: i64) {
        if !k.int_key {
            return;
        }
        self.in_place_rows.retain(|e| e.0 != root_pgno);
        self.in_place_serial += 1;
        self.in_place_rows
            .push((root_pgno, rowid, self.in_place_serial));
    }

    /// The `loc==0` part of `sqlite3BtreeInsert`: cell `idx` of page `pgno` (a page of kind
    /// `k`) holds the key being inserted.
    ///
    /// * `overwrite` and a payload of the same size → `btreeOverwriteCell`.
    /// * Otherwise `fillInCell` builds the new cell (allocating its overflow pages), the old
    ///   cell's overflow chain is freed (`BTREE_CLEAR_CELL`), and the new cell either takes the
    ///   place of the old one (same cell size, no old overflow, and — in auto-vacuum mode — no
    ///   new overflow) or the old cell is dropped and the new one inserted; a page that then
    ///   has an overflow cell is balanced.
    ///
    /// With `cur == None` the caller has no page path: `Ok(false)` is returned, with nothing
    /// modified, when the replacement could need `balance()`.
    #[allow(clippy::too_many_arguments)]
    fn replace_cell(
        &mut self,
        cur: Option<&mut BalCursor>,
        root_pgno: Pgno,
        pgno: Pgno,
        k: &PgKind,
        idx: usize,
        rowid: i64,
        payload: &[u8],
        overwrite: bool,
    ) -> Result<bool> {
        let usable = self.pager.usable_size() as usize;
        let hdr = hdr_of(pgno);
        let ptr0 = hdr + 8 + k.child_ptr;
        let auto_vac = self.is_auto_vacuum();
        let n_payload_new = u32::try_from(payload.len())
            .map_err(|_| SqliteError::new(ErrorCode::TooBig, "row payload exceeds 4GiB"))?;
        let (mut n_cell, off, info, child, same_size) = {
            let d = self.pager.read_page(pgno)?;
            if d.len() < usable || hdr + 8 > usable {
                return Err(corrupt("b-tree page buffer too small"));
            }
            let n_cell = get2(d, hdr + 3);
            if idx >= n_cell || ptr0 + 2 * n_cell > usable {
                return Err(corrupt("cell index out of range in insert"));
            }
            let off = get2(d, ptr0 + 2 * idx);
            if off < ptr0 + 2 * n_cell || off + 4 > usable {
                return Err(corrupt("cell offset out of range in insert"));
            }
            let info = parse_cell(k, usable as u32, d, off)?;
            if overwrite && info.n_payload == n_payload_new {
                (n_cell, off, info, [0u8; 4], true)
            } else {
                let child = [d[off], d[off + 1], d[off + 2], d[off + 3]];
                // Size of the cell `fillInCell` is going to build.
                let n_local_new = local_len(k, usable as u32, n_payload_new) as usize;
                let n_head = k.child_ptr
                    + varint_len(u64::from(n_payload_new))
                    + if k.int_key {
                        varint_len(rowid as u64)
                    } else {
                        0
                    };
                let sz_new = if n_local_new == payload.len() {
                    (n_head + n_local_new).max(4)
                } else {
                    n_head + n_local_new + 4
                };
                let in_place = info.n_size == sz_new
                    && info.n_local == info.n_payload
                    && (!auto_vac || (sz_new as u32) < k.min_local);
                if !in_place && cur.is_none() {
                    // Would `dropCell` + `insertCell` leave an overflow cell? After the drop
                    // the page has `nFree + old size + 2` free bytes (an emptied page is reset).
                    let fits = if n_cell == 1 {
                        sz_new + 2 <= usable - ptr0
                    } else {
                        let gap = ptr0 + 2 * n_cell;
                        let top = match get2(d, hdr + 5) {
                            0 => 65_536,
                            t => t,
                        };
                        if top >= gap && sz_new <= top - gap + info.n_size {
                            true
                        } else {
                            let n_free = compute_free_space(d, hdr, k.child_ptr, n_cell, usable)?;
                            sz_new as i32 <= n_free + info.n_size as i32
                        }
                    };
                    if !fits {
                        return Ok(false);
                    }
                }
                (n_cell, off, info, child, false)
            }
        };
        if same_size {
            if self.dtrace_persistent() {
                sqliters_types::dtrace!(
                    "TR|ins|cell|root={} pg={} idx={} o=overwrite n={}",
                    root_pgno,
                    pgno,
                    idx,
                    info.n_payload
                );
            }
            self.overwrite_cell(pgno, ptr0, off, &info, payload)?;
            self.note_in_place_row(k, root_pgno, rowid);
            return Ok(true);
        }

        let cell = self.fill_in_cell(k, child, rowid, payload)?;
        let sz_new = cell.size;
        self.pager.write_page(pgno)?;
        // BTREE_CLEAR_CELL
        if info.n_local != info.n_payload {
            if off + info.n_size > usable {
                return Err(corrupt("cell extends past end of page"));
            }
            let first = get4(self.pager.read_page(pgno)?, off + info.n_size - 4);
            let n_ovfl = (info.n_payload - info.n_local).div_ceil(usable as u32 - 4);
            self.free_overflow_chain_n(first, n_ovfl)?;
        }
        if info.n_size == sz_new
            && info.n_local == info.n_payload
            && (!auto_vac || (sz_new as u32) < k.min_local)
        {
            // "Overwrite the old cell with the new if they are the same size."
            if off < hdr + 10 || off + sz_new > self.pager.page_size() as usize {
                return Err(corrupt("cell to replace lies outside its page"));
            }
            if self.dtrace_persistent() {
                sqliters_types::dtrace!(
                    "TR|ins|cell|root={} pg={} idx={} o=replace-same sz={}",
                    root_pgno,
                    pgno,
                    idx,
                    sz_new
                );
            }
            let d = self.pager.write_page(pgno)?;
            cell.write(&mut d[off..off + sz_new]);
            self.note_in_place_row(k, root_pgno, rowid);
            return Ok(true);
        }
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|ins|cell|root={} pg={} idx={} o=replace-drop sz={} old={}",
                root_pgno,
                pgno,
                idx,
                sz_new,
                info.n_size
            );
        }
        {
            let secure = self.zero_freed_cells();
            let d = self.pager.write_page(pgno)?;
            let mut n_free_unused = 0i32;
            drop_cell(
                d,
                hdr,
                k.child_ptr,
                usable,
                &mut n_cell,
                &mut n_free_unused,
                idx,
                info.n_size,
                secure,
            )?;
        }
        if self.page_insert_cell(pgno, k, n_cell, idx, &cell)? {
            self.note_in_place_row(k, root_pgno, rowid);
            return Ok(true);
        }
        let Some(cur) = cur else {
            return Err(corrupt("replaced cell does not fit its page"));
        };
        let mut ovfl = OvflCells::default();
        ovfl.push_with(idx, cell.size, |dst| cell.write(dst))?;
        cur.set_ovfl(cur.i_page, ovfl);
        self.balance(cur)?;
        Ok(true)
    }

    /// `true` when cell `idx` of page `leaf` is a table-leaf cell holding `rowid` (the check
    /// that stands for C's `BTCF_ValidNKey && pX->nKey==pCur->info.nKey`).
    fn table_position_holds(&mut self, leaf: Pgno, idx: usize, rowid: i64) -> Result<bool> {
        if leaf == 0 || leaf > self.pager.page_count() || leaf == self.lock_byte_page() {
            return Ok(false);
        }
        let usable = self.pager.usable_size();
        let raw = self.pager.read_page(leaf)?;
        Ok(page::table_leaf_cell_has_rowid(
            leaf, usable, raw, idx, rowid, false,
        ))
    }

    /// `sqlite3BtreeInsert` on a table b-tree. `position` is the `(leaf, cell index)` of a
    /// cursor that is on `rowid` (re-validated; it only saves the descent). Returns `false`
    /// only in [`TableInsertMode::Unique`] when the rowid exists (nothing is modified).
    pub(crate) fn insert_table_c(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
        position: Option<(Pgno, usize)>,
        mode: TableInsertMode,
    ) -> Result<bool> {
        self.tr_ins = true;
        if mode != TableInsertMode::Unique && self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|ins|root={} rowid={}", root_pgno, rowid);
        }
        self.vc_keep = self.vc_open(root_pgno, crate::pins::VC_WRITE);
        let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
        self.vc_keep = usize::MAX;
        saved?;
        let usable = self.pager.usable_size() as usize;
        let overwrite = matches!(mode, TableInsertMode::Upsert { overwrite: true });

        if let Some((leaf, idx)) = position
            && mode != TableInsertMode::Unique
            && self.table_position_holds(leaf, idx, rowid)?
        {
            let k = PgKind::decode(0x0d, usable as u32)?;
            if self.replace_cell(None, root_pgno, leaf, &k, idx, rowid, payload, overwrite)? {
                return Ok(true);
            }
        }

        // sqlite3BtreeTableMoveto — unless the statement's write cursor is already where the
        // row goes: `OP_NewRowid` left it on the last leaf (`sqlite3BtreeLast`) and `OP_Insert`
        // uses that position (`OPFLAG_USESEEKRESULT`).
        let vc = self.vc_open(root_pgno, crate::pins::VC_WRITE);
        let mut cur = BalCursor::new(root_pgno);
        cur.vc = vc;
        // (Also: "The cursor is pointing to the entry that is to be overwritten" —
        // `BTCF_ValidNKey` with `info.nKey == pX->nKey` — the scan of a one-pass UPDATE or
        // `OP_NotExists` left it there.)
        let use_seek_result = self.new_rowid_hint.take() == Some((root_pgno, rowid))
            || (self.vcursors[vc].valid
                && self.vcursors[vc].nkey == Some(rowid)
                && self.vc_depth(vc) > 0
                && self.vc_cell_rowid_is(vc, rowid)?);
        let nkey_was = self.vcursors[vc].nkey;
        if !(use_seek_result && self.vc_resume(vc, &mut cur)?) {
            self.vc_move_to_root(vc)?;
        }
        // Size of the cell when its payload is local (0 = payload spills).
        let sz_local = if payload.len() <= usable - 35 {
            (varint_len(payload.len() as u64) + varint_len(rowid as u64) + payload.len()).max(4)
        } else {
            0
        };
        let (leaf, n_cell, idx, found, gap_top) = loop {
            let pgno = cur.page();
            let hdr = hdr_of(pgno);
            let d = self.pager.read_page(pgno)?;
            if hdr + 12 > d.len() || d.len() < usable {
                return Err(corrupt("b-tree page buffer too small"));
            }
            match d[hdr] {
                0x05 => {
                    let (i, child, _) =
                        find_table_interior_child_append_hint(pgno, usable as u32, d, rowid)?;
                    cur.descend(i, child)?;
                    self.vc_move_to_child_at(vc, child, i)?;
                }
                0x0d => {
                    let n_cell = get2(d, hdr + 3);
                    let (idx, found) = table_leaf_search(d, hdr, usable, n_cell, rowid)?;
                    // insertCell when the page has no freeblock and the gap takes the cell:
                    // `allocateSpace` answers `top - sz`.
                    let mut gap_top = 0usize;
                    if !found && sz_local != 0 && d[hdr + 1] == 0 && d[hdr + 2] == 0 {
                        let top = match get2(d, hdr + 5) {
                            0 if usable == 65_536 => 65_536,
                            t => t,
                        };
                        if hdr + 8 + 2 * n_cell + 2 + sz_local <= top && top <= usable {
                            gap_top = top;
                        }
                    }
                    break (pgno, n_cell, idx, found, gap_top);
                }
                _ => {
                    return Err(SqliteError::corrupt(format!(
                        "page {pgno} is not part of a table b-tree"
                    )));
                }
            }
        };

        if !found {
            if mode == TableInsertMode::Unique && self.dtrace_persistent() {
                sqliters_types::dtrace!("TR|w|ins|root={} rowid={}", root_pgno, rowid);
            }
            if gap_top != 0 {
                let hdr = hdr_of(leaf);
                let off = gap_top - sz_local;
                let d = self.pager.write_page(leaf)?;
                let mut p = off;
                p += put_varint(&mut d[p..], payload.len() as u64);
                p += put_varint(&mut d[p..], rowid as u64);
                d[p..p + payload.len()].copy_from_slice(payload);
                p += payload.len();
                d[p..off + sz_local].fill(0);
                let ptr0 = hdr + 8;
                let ins = ptr0 + 2 * idx;
                d.copy_within(ins..ptr0 + 2 * n_cell, ins + 2);
                put2(d, ins, off);
                put2(d, hdr + 3, n_cell + 1);
                put2(d, hdr + 5, off);
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                        root_pgno,
                        leaf,
                        idx,
                        sz_local
                    );
                    sqliters_types::dtrace!(
                        "TR|ins|slot|ctx=ins pg={} i={} sz={} o=gap off={}",
                        leaf,
                        idx,
                        sz_local,
                        off
                    );
                }
                // (the cursor stays valid on the new cell)
                self.vcursors[vc].valid = true;
                self.vcursors[vc].ix = idx as u16;
                self.vcursors[vc].nkey = None;
                return Ok(true);
            }
            let k = PgKind::decode(0x0d, usable as u32)?;
            let cell = self.fill_in_cell(&k, [0; 4], rowid, payload)?;
            if self.dtrace_persistent() {
                sqliters_types::dtrace!(
                    "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                    root_pgno,
                    leaf,
                    idx,
                    cell.size
                );
            }
            self.insert_new_cell(&mut cur, &k, n_cell, idx, &cell)?;
            if cur.i_page + 1 == self.vc_depth(vc) && self.vc_is_on(vc, leaf) && cur.ovfl.is_empty()
            {
                self.vcursors[vc].valid = true;
                self.vcursors[vc].ix = idx as u16;
                self.vcursors[vc].nkey = None;
            }
            return Ok(true);
        }
        if mode == TableInsertMode::Unique {
            return Ok(false);
        }
        let k = PgKind::decode(0x0d, usable as u32)?;
        let done = self.replace_cell(
            Some(&mut cur),
            root_pgno,
            leaf,
            &k,
            idx,
            rowid,
            payload,
            overwrite,
        )?;
        let _ = nkey_was;
        if cur.i_page + 1 == self.vc_depth(vc) && self.vc_is_on(vc, leaf) && cur.ovfl.is_empty() {
            // no balance: the cursor stays on the row (`BTCF_ValidNKey` as the seek left it)
            let c = &mut self.vcursors[vc];
            c.valid = true;
            c.ix = idx as u16;
            c.nkey = Some(rowid);
        } else if self.c_save_position == Some(root_pgno) {
            // "if( (flags & BTREE_SAVEPOSITION) && rc==SQLITE_OK ){
            //    btreeReleaseAllCursorPages(pCur); ... pCur->eState = CURSOR_REQUIRESSEEK;
            //    pCur->nKey = pX->nKey; }"
            self.vc_release_all(vc);
            self.vcursors[vc].seek_rowid = Some(rowid);
        }
        Ok(done)
    }

    /// The table insert for a caller that knows `rowid` is absent and that `leaf_pgno` is the
    /// leaf it belongs on (C: `OP_NotExists` left the cursor there and `OP_Insert` reuses the
    /// seek). Returns `false`, with nothing modified, unless the insert is the plain
    /// `insertCell` case (local payload, no freeblock on the leaf, room in the gap).
    pub(crate) fn insert_table_on_leaf_c(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
        leaf: Pgno,
    ) -> Result<bool> {
        let usable = self.pager.usable_size() as usize;
        if payload.len() > usable - 35 || leaf == 0 || leaf > self.pager.page_count() {
            return Ok(false);
        }
        let sz =
            (varint_len(payload.len() as u64) + varint_len(rowid as u64) + payload.len()).max(4);
        let hdr = hdr_of(leaf);
        let (n_cell, idx, top) = {
            let d = self.pager.read_page(leaf)?;
            if d.len() < usable || d[hdr] != 0x0d || d[hdr + 1] != 0 || d[hdr + 2] != 0 {
                return Ok(false);
            }
            let n_cell = get2(d, hdr + 3);
            let top = match get2(d, hdr + 5) {
                0 if usable == 65_536 => 65_536,
                t => t,
            };
            if hdr + 8 + 2 * n_cell + 2 + sz > top || top > usable {
                return Ok(false);
            }
            let (idx, found) = table_leaf_search(d, hdr, usable, n_cell, rowid)?;
            if found {
                return Ok(false);
            }
            (n_cell, idx, top)
        };
        self.tr_ins = true;
        // (the statement's write cursor is the one that inserts: `OP_Insert` after
        // `OP_NotExists`, `OPFLAG_USESEEKRESULT`)
        self.vc_keep = self.vc_open(root_pgno, crate::pins::VC_WRITE);
        let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
        self.vc_keep = usize::MAX;
        saved?;
        self.new_rowid_hint = None;
        let off = top - sz;
        let d = self.pager.write_page(leaf)?;
        let mut p = off;
        p += put_varint(&mut d[p..], payload.len() as u64);
        p += put_varint(&mut d[p..], rowid as u64);
        d[p..p + payload.len()].copy_from_slice(payload);
        p += payload.len();
        d[p..off + sz].fill(0);
        let ptr0 = hdr + 8;
        let ins = ptr0 + 2 * idx;
        d.copy_within(ins..ptr0 + 2 * n_cell, ins + 2);
        put2(d, ins, off);
        put2(d, hdr + 3, n_cell + 1);
        put2(d, hdr + 5, off);
        {
            // the write cursor is on the new cell
            let vc = self.vc_open(root_pgno, crate::pins::VC_WRITE);
            if self.vc_is_on(vc, leaf) {
                let c = &mut self.vcursors[vc];
                c.valid = true;
                c.ix = idx as u16;
            } else {
                self.vcursors[vc].valid = false;
            }
            self.vcursors[vc].nkey = None;
        }
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|ins|root={} rowid={}", root_pgno, rowid);
            sqliters_types::dtrace!(
                "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                root_pgno,
                leaf,
                idx,
                sz
            );
            sqliters_types::dtrace!(
                "TR|ins|slot|ctx=ins pg={} i={} sz={} o=gap off={}",
                leaf,
                idx,
                sz,
                off
            );
        }
        Ok(true)
    }

    /// `sqlite3BtreeIndexMoveto` on page `pgno` for cells that cannot be compared on the raw
    /// page: every probed cell is read in full (overflow pages included) and compared as a
    /// record. Same probe sequence and same answer as [`index_page_search_raw`].
    pub(crate) fn index_page_search_full(
        &mut self,
        pgno: Pgno,
        k: &PgKind,
        record: &[u8],
        orders: &[KeySortOrder],
        enc: TextEncoding,
        refs: bool,
    ) -> Result<(usize, bool)> {
        let usable = self.pager.usable_size() as usize;
        let hdr = hdr_of(pgno);
        let page: Vec<u8> = self.pager.read_page(pgno)?.to_vec();
        let n_cell = get2(&page, hdr + 3);
        if n_cell == 0 {
            return Ok((0, false));
        }
        let ptr0 = hdr + 8 + k.child_ptr;
        if ptr0 + 2 * n_cell > usable {
            return Err(corrupt("cell pointer array past page end"));
        }
        let max_fields = if orders.is_empty() {
            None
        } else {
            Some(orders.len())
        };
        let mut lwr = 0isize;
        let mut upr = n_cell as isize - 1;
        loop {
            let idx = ((lwr + upr) >> 1) as usize;
            let off = get2(&page, ptr0 + 2 * idx);
            if off < ptr0 + 2 * n_cell || off + 4 > usable {
                return Err(corrupt("index cell offset out of range"));
            }
            let info = parse_cell(k, usable as u32, &page, off)?;
            let pay = off + info.n_hdr;
            let n_local = info.n_local as usize;
            if pay + n_local > usable {
                return Err(corrupt("index cell extends past end of page"));
            }
            let ovfl = if info.n_local == info.n_payload {
                None
            } else {
                if pay + n_local + 4 > page.len() {
                    return Err(corrupt("index cell extends past end of page"));
                }
                Some(get4(&page, pay + n_local))
            };
            if refs && let Some(first) = ovfl {
                // (C's cursor reads the key through the page cache: `accessPayload`)
                self.c_overflow_refs(first, (info.n_payload - info.n_local) as usize)?;
            }
            let rec = self.read_full_payload(info.n_payload, &page[pay..pay + n_local], ovfl)?;
            let c = compare_raw_records_with_encoding(&rec, record, max_fields, orders, enc)?;
            match c {
                Ordering::Less => lwr = idx as isize + 1,
                Ordering::Greater => upr = idx as isize - 1,
                Ordering::Equal => return Ok((idx, true)),
            }
            if lwr > upr {
                return Ok((lwr as usize, false));
            }
        }
    }

    /// Whether cell `idx` of index page `pgno` continues on overflow pages.
    fn index_cell_overflows(&mut self, pgno: Pgno, k: &PgKind, idx: usize) -> Result<bool> {
        let usable = self.pager.usable_size() as usize;
        let hdr = hdr_of(pgno);
        let d = self.pager.read_page(pgno)?;
        let n_cell = get2(d, hdr + 3);
        let ptr0 = hdr + 8 + k.child_ptr;
        if idx >= n_cell || ptr0 + 2 * n_cell > usable || d.len() < usable {
            return Ok(false);
        }
        let off = get2(d, ptr0 + 2 * idx);
        if off < ptr0 + 2 * n_cell || off + 4 > usable {
            return Ok(false);
        }
        let info = parse_cell(k, usable as u32, d, off)?;
        Ok(info.n_local != info.n_payload)
    }

    /// The start of `sqlite3BtreeIndexMoveto` on cursor `vc` for a search key `record`:
    /// "Check to see if we can skip a lot of work. Two cases: (1) If the cursor is already
    /// pointing to the very last cell in the table and the pIdxKey search key is greater than
    /// or equal to that last cell, then no movement is required. (2) If the cursor is on the
    /// last page of the table and the first cell on that last page is less than or equal to
    /// the pIdxKey search key, then we can start the search on the current page without
    /// needing to go back to root." Returns `true` in those cases, with `cur` on the page the
    /// cursor is on; otherwise `moveToRoot` ran and `cur` is on the root.
    pub(crate) fn index_moveto_start(
        &mut self,
        vc: usize,
        cur: &mut BalCursor,
        record: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        let enc = self.encoding();
        let usable = self.pager.usable_size() as usize;
        let mut bypass = false;
        if self.vcursors[vc].valid && self.vc_depth(vc) > 0 {
            let leaf_kind = PgKind::decode(0x0a, usable as u32)?;
            let key = RawRecordKey::for_index_orders(record, orders);
            let n = self.vc_depth(vc);
            let leaf = self.vc_path(vc)[n - 1];
            let ix = usize::from(self.vcursors[vc].ix);
            let mut ai = [0u16; BTCURSOR_MAX_DEPTH];
            if self.vc_on_last_page(vc, &mut ai)? {
                let hdr = hdr_of(leaf);
                let probe = {
                    let d = self.pager.read_page(leaf)?;
                    if d.len() >= usable && hdr + 8 <= d.len() && d[hdr] == 0x0a {
                        let n_cell = get2(d, hdr + 3);
                        Some((
                            n_cell,
                            index_page_search_raw(d, hdr, usable, &leaf_kind, &key, orders, enc)?,
                        ))
                    } else {
                        None
                    }
                };
                if let Some((n_cell, hit)) = probe {
                    let (idx, found) = match hit {
                        Some(h) => h,
                        None => self
                            .index_page_search_full(leaf, &leaf_kind, record, orders, enc, false)?,
                    };
                    let last_le = n_cell > 0 && (idx == n_cell || (found && idx == n_cell - 1));
                    let first_le = n_cell > 0 && (idx > 0 || found);
                    // (`indexCellCompare`: "If the record extends into overflow pages, do
                    // not attempt the optimization")
                    let cond1 = n_cell > 0
                        && ix == n_cell - 1
                        && last_le
                        && !self.index_cell_overflows(leaf, &leaf_kind, n_cell - 1)?;
                    let cond2 = !cond1
                        && n > 1
                        && first_le
                        && !self.index_cell_overflows(leaf, &leaf_kind, 0)?;
                    // (case 1: C returns at once — "Cursor already pointing at the correct
                    // spot" — and compares no other cell of the page)
                    self.c_idx_search_quiet = cond1;
                    if cond1 || cond2 {
                        let path: Vec<Pgno> = self.vc_path(vc).to_vec();
                        cur.pg[..n].copy_from_slice(&path);
                        cur.ai[..n].copy_from_slice(&ai[..n]);
                        cur.i_page = n - 1;
                        bypass = true;
                    }
                }
            }
        }
        if !bypass {
            self.vc_move_to_root(vc)?;
        }
        self.vcursors[vc].valid = false;
        cur.vc = vc;
        Ok(bypass)
    }

    /// `sqlite3BtreeIndexMoveto(pCur, pIdxKey, &res)` on cursor `vc` with the record `record`
    /// as the key: `0` — the cursor is on an entry equal to it; `-1` / `1` — it is on the
    /// entry before / after the place the key belongs (`-1` also for an empty tree, with the
    /// cursor invalid).
    pub(crate) fn vc_index_moveto(
        &mut self,
        vc: usize,
        record: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<i32> {
        if self.pager.page_count() == 0 {
            return Ok(-1);
        }
        let enc = self.encoding();
        let usable = self.pager.usable_size() as usize;
        let key = RawRecordKey::for_index_orders(record, orders);
        let root = self.vcursors[vc].root;
        let mut cur = BalCursor::new(root);
        self.index_moveto_start(vc, &mut cur, record, orders)?;
        let leaf_kind = PgKind::decode(0x0a, usable as u32)?;
        let int_kind = PgKind::decode(0x02, usable as u32)?;
        loop {
            let pgno = cur.page();
            let hdr = hdr_of(pgno);
            let (k, n_cell, hit) = {
                let d = self.pager.read_page(pgno)?;
                if hdr + 12 > d.len() || d.len() < usable {
                    return Err(corrupt("b-tree page buffer too small"));
                }
                let k = match d[hdr] {
                    0x0a => leaf_kind,
                    0x02 => int_kind,
                    _ => return Err(corrupt("expected index page in index seek")),
                };
                let n_cell = get2(d, hdr + 3);
                (k, n_cell, index_page_search_raw(d, hdr, usable, &k, &key, orders, enc)?)
            };
            let refs = !std::mem::take(&mut self.c_idx_search_quiet);
            let (idx, found) = match hit {
                Some(h) => h,
                None => self.index_page_search_full(pgno, &k, record, orders, enc, refs)?,
            };
            if found {
                let c = &mut self.vcursors[vc];
                c.ix = idx as u16;
                c.valid = true;
                return Ok(0);
            }
            if k.leaf {
                let c = &mut self.vcursors[vc];
                if n_cell == 0 {
                    c.valid = false;
                    return Ok(-1);
                }
                c.valid = true;
                if idx >= n_cell {
                    c.ix = (n_cell - 1) as u16;
                    return Ok(-1);
                }
                c.ix = idx as u16;
                return Ok(1);
            }
            let child = {
                let d = self.pager.read_page(pgno)?;
                if idx < n_cell {
                    let off = get2(d, hdr + 12 + 2 * idx);
                    if off + 4 > usable {
                        return Err(corrupt("index cell offset out of range"));
                    }
                    get4(d, off)
                } else {
                    get4(d, hdr + 8)
                }
            };
            if child == 0 || child > self.pager.page_count() {
                return Err(corrupt("index interior page with a bad child"));
            }
            self.bc_descend(&mut cur, idx, child)?;
        }
    }

    /// Whether cell `ix` of index page `pgno` compares equal to the key `record`.
    pub(crate) fn index_cell_equals(
        &mut self,
        pgno: Pgno,
        ix: usize,
        record: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        let enc = self.encoding();
        let usable = self.pager.usable_size() as usize;
        let hdr = hdr_of(pgno);
        let key = RawRecordKey::for_index_orders(record, orders);
        let (k, hit) = {
            let d = self.pager.read_page(pgno)?;
            if hdr + 12 > d.len() || d.len() < usable {
                return Ok(false);
            }
            let k = match d[hdr] {
                0x0a => PgKind::decode(0x0a, usable as u32)?,
                0x02 => PgKind::decode(0x02, usable as u32)?,
                _ => return Ok(false),
            };
            (k, index_page_search_raw(d, hdr, usable, &k, &key, orders, enc)?)
        };
        let (idx, found) = match hit {
            Some(h) => h,
            None => self.index_page_search_full(pgno, &k, record, orders, enc, false)?,
        };
        Ok(found && idx == ix)
    }

    /// Whether the leaf the index cursor `vc` is on is where `record` belongs: the stack
    /// still describes the tree (each page leads to the next through the recorded child
    /// slot) and the key lies between the dividers around that path. Fills `ai` with the
    /// child slots (`aiIdx[]`).
    fn vc_index_leaf_takes(
        &mut self,
        vc: usize,
        record: &[u8],
        orders: &[KeySortOrder],
        ai: &mut [u16],
    ) -> Result<bool> {
        let enc = self.encoding();
        let usable = self.pager.usable_size() as usize;
        let key = RawRecordKey::for_index_orders(record, orders);
        let n = self.vc_depth(vc);
        let path: Vec<Pgno> = self.vc_path(vc).to_vec();
        let int_kind = PgKind::decode(0x02, usable as u32)?;
        for i in 0..n {
            let pg = path[i];
            if pg == 0 || pg > self.pager.page_count() {
                return Ok(false);
            }
            let hdr = hdr_of(pg);
            let d = self.pager.read_page(pg)?;
            if d.len() < usable || hdr + 12 > d.len() {
                return Ok(false);
            }
            if i + 1 == n {
                return Ok(d[hdr] == 0x0a);
            }
            if d[hdr] != 0x02 {
                return Ok(false);
            }
            // the child the key leads to must be the next page of the stack
            let n_cell = get2(d, hdr + 3);
            let Some((idx, found)) =
                index_page_search_raw(d, hdr, usable, &int_kind, &key, orders, enc)?
            else {
                return Ok(false);
            };
            if found {
                return Ok(false);
            }
            let child = if idx < n_cell {
                let off = get2(d, hdr + 12 + 2 * idx);
                if off + 4 > usable {
                    return Ok(false);
                }
                get4(d, off)
            } else {
                get4(d, hdr + 8)
            };
            if child != path[i + 1] {
                return Ok(false);
            }
            ai[i] = idx as u16;
        }
        Ok(false)
    }

    /// Whether `OP_NoConflict` placed the write cursor of index `root` for the insert that is
    /// starting (the note is used up).
    fn idx_seek_hint_take(&mut self, root: Pgno) -> bool {
        match self.idx_seek_hint.iter().position(|&r| r == root) {
            Some(i) => {
                self.idx_seek_hint.swap_remove(i);
                true
            }
            None => false,
        }
    }

    /// `moveToChild` of a balance path whose pages the cursor `cur.vc` holds.
    #[inline]
    pub(crate) fn bc_descend(&mut self, cur: &mut BalCursor, idx: usize, child: Pgno) -> Result<()> {
        cur.descend(idx, child)?;
        if cur.vc != usize::MAX {
            self.vc_move_to_child_at(cur.vc, child, idx)?;
        }
        Ok(())
    }

    /// `sqlite3BtreeInsert` on an index b-tree (an index or a WITHOUT ROWID table): `record`
    /// is the key. `bulk` is `BTREE_BULKLOAD` of the cursor (`balance_nonroot(bBulk=1)`).
    pub(crate) fn insert_index_c(
        &mut self,
        root_pgno: Pgno,
        record: &[u8],
        orders: &[KeySortOrder],
        bulk: bool,
    ) -> Result<()> {
        self.tr_ins = true;
        self.vc_keep = self.vc_open(root_pgno, crate::pins::VC_WRITE);
        let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
        self.vc_keep = usize::MAX;
        saved?;
        let enc = self.encoding();
        let usable = self.pager.usable_size() as usize;
        let key = RawRecordKey::for_index_orders(record, orders);
        let vc = self.vc_open(root_pgno, crate::pins::VC_WRITE);
        self.vcursors[vc].is_index = true;
        let mut cur = BalCursor::new(root_pgno);
        cur.bulk = bulk;
        cur.vc = vc;
        if bulk && self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|xf|ins|root={} app=0 bulk=1 pre=0", root_pgno);
        }
        let leaf_kind = PgKind::decode(0x0a, usable as u32)?;
        let int_kind = PgKind::decode(0x02, usable as u32)?;
        // Size of the cell on a leaf when its payload is local (0 = payload spills).
        let sz_local = if record.len() <= leaf_kind.max_local as usize {
            (varint_len(record.len() as u64) + record.len()).max(4)
        } else {
            0
        };

        // sqlite3BtreeIndexMoveto. "Check to see if we can skip a lot of work. Two cases: (1)
        // If the cursor is already pointing to the very last cell in the table and the
        // pIdxKey search key is greater than or equal to that last cell, then no movement is
        // required. (2) If the cursor is on the last page of the table and the first cell on
        // that last page is less than or equal to the pIdxKey search key, then we can start
        // the search on the current page without needing to go back to root." Either way the
        // search below starts on the page the cursor is on; otherwise: moveToRoot, then down.
        let mut bypass = false;
        // `OPFLAG_USESEEKRESULT`: `OP_NoConflict` left the cursor where the key belongs and
        // `sqlite3BtreeInsert` does not search again.
        if self.idx_seek_hint_take(root_pgno)
            && self.vcursors[vc].valid
            && self.vc_depth(vc) > 0
        {
            let mut ai = [0u16; BTCURSOR_MAX_DEPTH];
            if self.vc_index_leaf_takes(vc, record, orders, &mut ai)? {
                let n = self.vc_depth(vc);
                let path: Vec<Pgno> = self.vc_path(vc).to_vec();
                cur.pg[..n].copy_from_slice(&path);
                cur.ai[..n].copy_from_slice(&ai[..n]);
                cur.i_page = n - 1;
                bypass = true;
            }
        }
        if !bypass && self.vcursors[vc].valid && self.vc_depth(vc) > 0 {
            let n = self.vc_depth(vc);
            let leaf = self.vc_path(vc)[n - 1];
            let ix = usize::from(self.vcursors[vc].ix);
            let mut ai = [0u16; BTCURSOR_MAX_DEPTH];
            if self.vc_on_last_page(vc, &mut ai)? {
                let hdr = hdr_of(leaf);
                let probe = {
                    let d = self.pager.read_page(leaf)?;
                    if d.len() >= usable && hdr + 8 <= d.len() && d[hdr] == 0x0a {
                        let n_cell = get2(d, hdr + 3);
                        Some((
                            n_cell,
                            index_page_search_raw(d, hdr, usable, &leaf_kind, &key, orders, enc)?,
                        ))
                    } else {
                        None
                    }
                };
                if let Some((n_cell, hit)) = probe {
                    let (idx, found) = match hit {
                        Some(h) => h,
                        None => self
                            .index_page_search_full(leaf, &leaf_kind, record, orders, enc, false)?,
                    };
                    // last cell <= key / first cell <= key
                    let last_le = n_cell > 0 && (idx == n_cell || (found && idx == n_cell - 1));
                    let first_le = n_cell > 0 && (idx > 0 || found);
                    let cond1 = n_cell > 0
                        && ix == n_cell - 1
                        && last_le
                        && !self.index_cell_overflows(leaf, &leaf_kind, n_cell - 1)?;
                    let cond2 = !cond1
                        && n > 1
                        && first_le
                        && !self.index_cell_overflows(leaf, &leaf_kind, 0)?;
                    // (case 1: C returns at once — "Cursor already pointing at the correct
                    // spot" — and compares no other cell of the page)
                    self.c_idx_search_quiet = cond1;
                    if cond1 || cond2 {
                        let path: Vec<Pgno> = self.vc_path(vc).to_vec();
                        cur.pg[..n].copy_from_slice(&path);
                        cur.ai[..n].copy_from_slice(&ai[..n]);
                        cur.i_page = n - 1;
                        bypass = true;
                    }
                }
            }
        }
        if !bypass {
            self.vc_move_to_root(vc)?;
        }
        self.vcursors[vc].valid = false;
        let (pgno, k, n_cell, idx, found, gap_top) = loop {
            let pgno = cur.page();
            let hdr = hdr_of(pgno);
            let (k, n_cell, hit, gap_top) = {
                let d = self.pager.read_page(pgno)?;
                if hdr + 12 > d.len() || d.len() < usable {
                    return Err(corrupt("b-tree page buffer too small"));
                }
                let k = match d[hdr] {
                    0x0a => leaf_kind,
                    0x02 => int_kind,
                    _ => return Err(corrupt("expected index page in insert_index")),
                };
                let n_cell = get2(d, hdr + 3);
                let hit = index_page_search_raw(d, hdr, usable, &k, &key, orders, enc)?;
                let mut gap_top = 0usize;
                if k.leaf
                    && sz_local != 0
                    && d[hdr + 1] == 0
                    && d[hdr + 2] == 0
                    && matches!(hit, Some((_, false)))
                {
                    let top = match get2(d, hdr + 5) {
                        0 if usable == 65_536 => 65_536,
                        t => t,
                    };
                    if hdr + 8 + 2 * n_cell + 2 + sz_local <= top && top <= usable {
                        gap_top = top;
                    }
                }
                (k, n_cell, hit, gap_top)
            };
            let refs = !std::mem::take(&mut self.c_idx_search_quiet);
            let (idx, found) = match hit {
                Some(h) => h,
                None => self.index_page_search_full(pgno, &k, record, orders, enc, refs)?,
            };
            if found || k.leaf {
                break (pgno, k, n_cell, idx, found, gap_top);
            }
            // moveToChild(lwr)
            let child = {
                let d = self.pager.read_page(pgno)?;
                if idx < n_cell {
                    let off = get2(d, hdr + 12 + 2 * idx);
                    if off + 4 > usable {
                        return Err(corrupt("index cell offset out of range"));
                    }
                    get4(d, off)
                } else {
                    get4(d, hdr + 8)
                }
            };
            if child == 0 {
                return Err(corrupt("index interior page with a null child"));
            }
            cur.descend(idx, child)?;
            self.vc_move_to_child(vc, child)?;
        };

        if found {
            // `loc==0`: `if( pCur->info.nKey==pX->nKey ) return btreeOverwriteCell(...)`,
            // else the replace path.
            self.replace_cell(Some(&mut cur), root_pgno, pgno, &k, idx, 0, record, true)?;
            if cur.i_page + 1 == self.vc_depth(vc) && self.vc_is_on(vc, pgno) {
                // (no `balance()`: the cursor is still valid on the cell)
                self.vc_set_valid(vc, idx);
            }
            return Ok(());
        }
        if gap_top != 0 {
            let hdr = hdr_of(pgno);
            let off = gap_top - sz_local;
            let d = self.pager.write_page(pgno)?;
            let mut p = off;
            p += put_varint(&mut d[p..], record.len() as u64);
            d[p..p + record.len()].copy_from_slice(record);
            p += record.len();
            d[p..off + sz_local].fill(0);
            let ptr0 = hdr + 8;
            let ins = ptr0 + 2 * idx;
            d.copy_within(ins..ptr0 + 2 * n_cell, ins + 2);
            put2(d, ins, off);
            put2(d, hdr + 3, n_cell + 1);
            put2(d, hdr + 5, off);
            if self.dtrace_persistent() {
                sqliters_types::dtrace!(
                    "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                    root_pgno,
                    pgno,
                    idx,
                    sz_local
                );
                sqliters_types::dtrace!(
                    "TR|ins|slot|ctx=ins pg={} i={} sz={} o=gap off={}",
                    pgno,
                    idx,
                    sz_local,
                    off
                );
            }
            self.vc_set_valid(vc, idx);
            return Ok(());
        }
        let cell = self.fill_in_cell(&k, [0; 4], 0, record)?;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                root_pgno,
                pgno,
                idx,
                cell.size
            );
        }
        self.insert_new_cell(&mut cur, &k, n_cell, idx, &cell)?;
        if cur.i_page + 1 == self.vc_depth(vc) && self.vc_is_on(vc, pgno) && cur.ovfl.is_empty() {
            // (`insertCell` without `balance()`: the cursor stays valid on the new cell)
            self.vc_set_valid(vc, idx);
        }
        Ok(())
    }
    /// `OP_SeekEnd`: puts `ac` on the right-most leaf of its tree (C keeps the cursor where
    /// the previous insert left it unless that insert had to balance) and returns the leaf's
    /// cell count.
    fn append_seek_end(&mut self, ac: &mut AppendCursor, leaf_flag: u8) -> Result<usize> {
        let usable = self.pager.usable_size() as usize;
        // The statement's write cursor on the tree (`sqlite3BtreeLast`: a no-op while the
        // cursor is still on the last entry; otherwise `moveToRoot`, then down the right
        // edge).
        let vc = self.vc_open(ac.root, crate::pins::VC_WRITE);
        if leaf_flag == 0x0a {
            self.vcursors[vc].is_index = true;
        }
        if ac.valid
            && (self.vc_depth(vc) != ac.cur.i_page + 1 || !self.vc_is_on(vc, ac.cur.page()))
        {
            // (the cursor was moved or saved behind the append cursor's back)
            ac.valid = false;
        }
        if !ac.valid {
            let bulk = ac.cur.bulk;
            ac.cur = BalCursor::new(ac.root);
            ac.cur.bulk = bulk;
            ac.cur.vc = vc;
            self.vc_move_to_root(vc)?;
        }
        loop {
            let pgno = ac.cur.page();
            let hdr = hdr_of(pgno);
            let d = self.pager.read_page(pgno)?;
            if hdr + 12 > d.len() || d.len() < usable {
                return Err(corrupt("b-tree page buffer too small"));
            }
            let n_cell = get2(d, hdr + 3);
            if d[hdr] == leaf_flag {
                ac.valid = true;
                let c = &mut self.vcursors[vc];
                c.valid = n_cell > 0;
                c.at_last = n_cell > 0;
                c.ix = n_cell.saturating_sub(1) as u16;
                c.nkey = None;
                return Ok(n_cell);
            }
            if d[hdr] != (leaf_flag & !0x08) || ac.valid {
                return Err(corrupt(
                    "unexpected page kind on the right edge of a b-tree",
                ));
            }
            let child = get4(d, hdr + 8);
            if child == 0 {
                return Err(corrupt("interior page with a null right child"));
            }
            self.bc_descend(&mut ac.cur, n_cell, child)?;
        }
    }

    /// `OP_SeekEnd` + `OP_IdxInsert` with `OPFLAG_USESEEKRESULT` on an index cursor: `record`
    /// becomes the last entry of the tree whatever its key (`sqlite3BtreeInsert` with
    /// `seekResult < 0`: no search, `idx = ++pCur->ix`).
    pub(crate) fn append_index_c(&mut self, ac: &mut AppendCursor, record: &[u8]) -> Result<()> {
        self.append_index_inner(ac, record, true)
    }

    /// [`Self::append_index_c`]; `app`: C got here with `seekResult < 0` (decision trace only —
    /// `false` when the place was found by comparing with the last entry, see
    /// [`Self::insert_index_sorted_c`]).
    fn append_index_inner(
        &mut self,
        ac: &mut AppendCursor,
        record: &[u8],
        #[allow(unused_variables)] app: bool,
    ) -> Result<()> {
        self.tr_ins = true;
        let root_pgno = ac.root;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|ins|root={} len={}", root_pgno, record.len());
        }
        if !ac.saved {
            self.vc_keep = self.vc_open(root_pgno, crate::pins::VC_WRITE);
            let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
            self.vc_keep = usize::MAX;
            saved?;
            ac.saved = self.open_cursor_count == 0;
        }
        let usable = self.pager.usable_size() as usize;
        let k = PgKind::decode(0x0a, usable as u32)?;
        // `OP_RowCell` (`sqlite3BtreeTransferRow`) runs before the insert: the overflow pages
        // of a transferred cell are allocated first.
        // (a record that stays on the page needs no preparation: `sqlite3BtreeTransferRow`
        // then only copies it)
        let local = record.len() <= k.max_local as usize;
        let pre_cell = if self.preformat && !local {
            Some(self.fill_in_cell(&k, [0; 4], 0, record)?)
        } else {
            None
        };
        if self.dtrace_persistent() {
            if self.preformat && local {
                sqliters_types::dtrace!(
                    "TR|xf|row|pay={} local={} o=local",
                    record.len(),
                    record.len()
                );
            }
            sqliters_types::dtrace!(
                "TR|xf|ins|root={} app={} bulk={} pre={}",
                root_pgno,
                u8::from(app),
                u8::from(ac.cur.bulk),
                u8::from(self.preformat)
            );
        }
        let n_cell = self.append_seek_end(ac, 0x0a)?;
        let pgno = ac.cur.page();
        let hdr = hdr_of(pgno);
        if pre_cell.is_none() && local {
            let sz = (varint_len(record.len() as u64) + record.len()).max(4);
            let top = {
                let d = self.pager.read_page(pgno)?;
                if d[hdr + 1] == 0 && d[hdr + 2] == 0 {
                    match get2(d, hdr + 5) {
                        0 if usable == 65_536 => 65_536,
                        t => t,
                    }
                } else {
                    0
                }
            };
            if top != 0 && hdr + 8 + 2 * n_cell + 2 + sz <= top && top <= usable {
                let off = top - sz;
                let d = self.pager.write_page(pgno)?;
                let mut p = off;
                p += put_varint(&mut d[p..], record.len() as u64);
                d[p..p + record.len()].copy_from_slice(record);
                p += record.len();
                d[p..off + sz].fill(0);
                put2(d, hdr + 8 + 2 * n_cell, off);
                put2(d, hdr + 3, n_cell + 1);
                put2(d, hdr + 5, off);
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                        root_pgno,
                        pgno,
                        n_cell,
                        sz
                    );
                    sqliters_types::dtrace!(
                        "TR|ins|slot|ctx=ins pg={} i={} sz={} o=gap off={}",
                        pgno,
                        n_cell,
                        sz,
                        off
                    );
                }
                return Ok(());
            }
        }
        let cell = match pre_cell {
            Some(c) => c,
            None => {
                // (a local preformatted record was reported above: build its cell silently)
                let pre = self.preformat;
                self.preformat = pre && !local;
                let c = self.fill_in_cell(&k, [0; 4], 0, record);
                self.preformat = pre;
                c?
            }
        };
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                root_pgno,
                pgno,
                n_cell,
                cell.size
            );
        }
        if self.page_insert_cell(pgno, &k, n_cell, n_cell, &cell)? {
            return Ok(());
        }
        let mut ovfl = OvflCells::default();
        ovfl.push_with(n_cell, cell.size, |dst| cell.write(dst))?;
        ac.cur.set_ovfl(ac.cur.i_page, ovfl);
        ac.valid = false;
        self.balance(&mut ac.cur)
    }

    /// `OP_SeekEnd` + `OP_Insert` with `OPFLAG_APPEND|OPFLAG_USESEEKRESULT` on a table cursor:
    /// the row becomes the last cell of the right-most leaf without a search.
    pub(crate) fn append_table_c(
        &mut self,
        ac: &mut AppendCursor,
        rowid: i64,
        payload: &[u8],
    ) -> Result<()> {
        self.append_table_inner(ac, rowid, payload, true)
    }

    fn append_table_inner(
        &mut self,
        ac: &mut AppendCursor,
        rowid: i64,
        payload: &[u8],
        #[allow(unused_variables)] app: bool,
    ) -> Result<()> {
        self.tr_ins = true;
        let root_pgno = ac.root;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|ins|root={} rowid={}", root_pgno, rowid);
        }
        if !ac.saved {
            self.vc_keep = self.vc_open(root_pgno, crate::pins::VC_WRITE);
            let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
            self.vc_keep = usize::MAX;
            saved?;
            ac.saved = self.open_cursor_count == 0;
        }
        let usable = self.pager.usable_size() as usize;
        let k = PgKind::decode(0x0d, usable as u32)?;
        // `OP_RowCell` runs before `OP_Insert`: overflow pages first.
        let cell = self.fill_in_cell(&k, [0; 4], rowid, payload)?;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|xf|ins|root={} app={} bulk=0 pre={}",
                root_pgno,
                u8::from(app),
                u8::from(self.preformat)
            );
        }
        let n_cell = self.append_seek_end(ac, 0x0d)?;
        let pgno = ac.cur.page();
        if cell.ovfl == 0 {
            // insertCell into the gap of a page without freeblocks.
            let hdr = hdr_of(pgno);
            let sz = cell.size;
            let top = {
                let d = self.pager.read_page(pgno)?;
                if d[hdr + 1] == 0 && d[hdr + 2] == 0 {
                    match get2(d, hdr + 5) {
                        0 if usable == 65_536 => 65_536,
                        t => t,
                    }
                } else {
                    0
                }
            };
            if top != 0 && hdr + 8 + 2 * n_cell + 2 + sz <= top && top <= usable {
                let off = top - sz;
                let d = self.pager.write_page(pgno)?;
                cell.write(&mut d[off..off + sz]);
                put2(d, hdr + 8 + 2 * n_cell, off);
                put2(d, hdr + 3, n_cell + 1);
                put2(d, hdr + 5, off);
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                        root_pgno,
                        pgno,
                        n_cell,
                        sz
                    );
                    sqliters_types::dtrace!(
                        "TR|ins|slot|ctx=ins pg={} i={} sz={} o=gap off={}",
                        pgno,
                        n_cell,
                        sz,
                        off
                    );
                }
                return Ok(());
            }
        }
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                root_pgno,
                pgno,
                n_cell,
                cell.size
            );
        }
        if self.page_insert_cell(pgno, &k, n_cell, n_cell, &cell)? {
            return Ok(());
        }
        let mut ovfl = OvflCells::default();
        ovfl.push_with(n_cell, cell.size, |dst| cell.write(dst))?;
        ac.cur.set_ovfl(ac.cur.i_page, ovfl);
        ac.valid = false;
        self.balance(&mut ac.cur)
    }
    /// `OP_RowCell` + `OP_Insert` with `OPFLAG_APPEND|OPFLAG_PREFORMAT` on a table cursor: the
    /// cell is built first (`sqlite3BtreeTransferRow`: overflow pages allocated with
    /// `nearby = 0`), then `sqlite3BtreeInsert` finds the place of `rowid` — which must be
    /// absent — and stores the cell.
    pub(crate) fn insert_table_preformat_c(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
    ) -> Result<()> {
        self.tr_ins = true;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|ins|root={} rowid={}", root_pgno, rowid);
        }
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let usable = self.pager.usable_size() as usize;
        let k = PgKind::decode(0x0d, usable as u32)?;
        let cell = self.fill_in_cell(&k, [0; 4], rowid, payload)?;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|xf|ins|root={} app=0 bulk=0 pre=1", root_pgno);
        }
        // sqlite3BtreeTableMoveto
        let mut cur = BalCursor::new(root_pgno);
        let (_leaf, n_cell, idx) = loop {
            let pgno = cur.page();
            let hdr = hdr_of(pgno);
            let d = self.pager.read_page(pgno)?;
            if hdr + 12 > d.len() || d.len() < usable {
                return Err(corrupt("b-tree page buffer too small"));
            }
            match d[hdr] {
                0x05 => {
                    let (i, child, _) =
                        find_table_interior_child_append_hint(pgno, usable as u32, d, rowid)?;
                    cur.descend(i, child)?;
                }
                0x0d => {
                    let n_cell = get2(d, hdr + 3);
                    let (idx, found) = table_leaf_search(d, hdr, usable, n_cell, rowid)?;
                    if found {
                        return Err(corrupt("transferred row already exists"));
                    }
                    break (pgno, n_cell, idx);
                }
                _ => {
                    return Err(SqliteError::corrupt(format!(
                        "page {pgno} is not part of a table b-tree"
                    )));
                }
            }
        };
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                root_pgno,
                _leaf,
                idx,
                cell.size
            );
        }
        self.insert_new_cell(&mut cur, &k, n_cell, idx, &cell)
    }
    /// [`Self::append_index_c`] for a run of records: `next` hands out one record after the
    /// other (or an error, which ends the run with the records before it stored). The outcome
    /// is that of one `append_index_c` call per record; records that simply fit into the gap
    /// of the right-most leaf are written without leaving the page.
    pub(crate) fn append_index_run_c<'r>(
        &mut self,
        ac: &mut AppendCursor,
        next: &mut dyn FnMut() -> Result<Option<&'r [u8]>>,
    ) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let k = PgKind::decode(0x0a, usable as u32)?;
        let max_local = k.max_local as usize;
        let root_pgno = ac.root;
        #[allow(unused_variables)]
        let traced = self.dtrace_persistent();
        #[allow(unused_variables)]
        let (tr_bulk, tr_pre) = (u8::from(ac.cur.bulk), u8::from(self.preformat));
        loop {
            let Some(first) = next()? else {
                return Ok(());
            };
            // Does `first` simply fit on the right-most leaf?
            let mut pending = Some(first);
            if first.len() <= max_local {
                self.tr_ins = true;
                if !ac.saved {
                    self.save_cursors_for_mutation(Some(root_pgno), None)?;
                    ac.saved = self.open_cursor_count == 0;
                }
                let n_cell0 = self.append_seek_end(ac, 0x0a)?;
                let pgno = ac.cur.page();
                let hdr = hdr_of(pgno);
                let fits = {
                    let d = self.pager.read_page(pgno)?;
                    let sz = (varint_len(first.len() as u64) + first.len()).max(4);
                    let top = match get2(d, hdr + 5) {
                        0 if usable == 65_536 => 65_536,
                        t => t,
                    };
                    d[hdr + 1] == 0
                        && d[hdr + 2] == 0
                        && hdr + 8 + 2 * n_cell0 + 2 + sz <= top
                        && top <= usable
                };
                if fits {
                    let d = self.pager.write_page(pgno)?;
                    let mut top = match get2(d, hdr + 5) {
                        0 => 65_536,
                        t => t,
                    };
                    let mut n_cell = n_cell0;
                    let mut failed: Option<SqliteError> = None;
                    while let Some(rec) = pending {
                        if rec.len() > max_local {
                            break;
                        }
                        let sz = (varint_len(rec.len() as u64) + rec.len()).max(4);
                        if hdr + 8 + 2 * n_cell + 2 + sz > top {
                            break;
                        }
                        let off = top - sz;
                        let mut p = off;
                        p += put_varint(&mut d[p..], rec.len() as u64);
                        d[p..p + rec.len()].copy_from_slice(rec);
                        p += rec.len();
                        d[p..off + sz].fill(0);
                        put2(d, hdr + 8 + 2 * n_cell, off);
                        if traced {
                            sqliters_types::dtrace!(
                                "TR|w|ins|root={} len={}",
                                root_pgno,
                                rec.len()
                            );
                            if tr_pre != 0 {
                                sqliters_types::dtrace!(
                                    "TR|xf|row|pay={} local={} o=local",
                                    rec.len(),
                                    rec.len()
                                );
                            }
                            sqliters_types::dtrace!(
                                "TR|xf|ins|root={} app=1 bulk={} pre={}",
                                root_pgno,
                                tr_bulk,
                                tr_pre
                            );
                            sqliters_types::dtrace!(
                                "TR|ins|cell|root={} pg={} idx={} o=new sz={}",
                                root_pgno,
                                pgno,
                                n_cell,
                                sz
                            );
                            sqliters_types::dtrace!(
                                "TR|ins|slot|ctx=ins pg={} i={} sz={} o=gap off={}",
                                pgno,
                                n_cell,
                                sz,
                                off
                            );
                        }
                        n_cell += 1;
                        top = off;
                        pending = match next() {
                            Ok(r) => r,
                            Err(e) => {
                                failed = Some(e);
                                None
                            }
                        };
                    }
                    put2(d, hdr + 3, n_cell);
                    put2(d, hdr + 5, top);
                    if let Some(e) = failed {
                        return Err(e);
                    }
                }
            }
            // The record that did not fit (page full, spilling payload, freeblocks on the page)
            // takes the general path; the next one starts a new run.
            match pending {
                Some(rec) => self.append_index_c(ac, rec)?,
                None => return Ok(()),
            }
        }
    }
    /// `OP_IdxInsert` without a usable seek result through a cursor that stays on the tree
    /// (the index copy of the transfer optimisation, whose source delivers the keys in index
    /// order). `sqlite3BtreeIndexMoveto` first looks whether the cursor is on the last entry
    /// of the tree and the key is greater: then the key goes behind it without a search.
    /// Any other key is placed by the ordinary search ([`Self::insert_index_c`]); the pages
    /// are the same either way.
    pub(crate) fn insert_index_sorted_c(
        &mut self,
        ac: &mut AppendCursor,
        record: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let n_cell = self.append_seek_end(ac, 0x0a)?;
        let pgno = ac.cur.page();
        let hdr = hdr_of(pgno);
        let greater = if n_cell == 0 {
            pgno == ac.root
        } else {
            let enc = self.encoding();
            let k = PgKind::decode(0x0a, usable as u32)?;
            let key = RawRecordKey::for_index_orders(record, orders);
            let d = self.pager.read_page(pgno)?;
            let off = get2(d, hdr + 8 + 2 * (n_cell - 1));
            let mut gt = false;
            if off >= hdr + 8 + 2 * n_cell && off < usable {
                let b0 = d[off];
                let parsed = if b0 < 0x80 {
                    Some((u32::from(b0), 1usize))
                } else {
                    get_varint(&d[off..usable])
                        .ok()
                        .and_then(|(v, n)| u32::try_from(v).ok().map(|v| (v, n)))
                };
                if let Some((n_payload, n1)) = parsed
                    && n_payload <= k.max_local
                    && off + n1 + n_payload as usize <= usable
                {
                    let start = off + n1;
                    // (`compare_stored` orders the stored record against the key)
                    gt = matches!(
                        key.compare_stored(&d[start..start + n_payload as usize], orders, enc),
                        Ok(Ordering::Less)
                    );
                }
            }
            gt
        };
        if greater {
            return self.append_index_inner(ac, record, false);
        }
        ac.valid = false;
        self.insert_index_c(ac.root, record, orders, ac.cur.bulk)
    }

    /// `OP_RowCell` + `OP_Insert` with `OPFLAG_APPEND|OPFLAG_PREFORMAT` through a cursor that
    /// stays on the table (the table copy of the transfer optimisation: the rowids arrive in
    /// ascending order). `sqlite3BtreeTableMoveto` answers at once when the cursor is on the
    /// last row and the new rowid is larger; any other rowid is placed by the ordinary search
    /// ([`Self::insert_table_preformat_c`]).
    pub(crate) fn insert_table_sorted_c(
        &mut self,
        ac: &mut AppendCursor,
        rowid: i64,
        payload: &[u8],
    ) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let n_cell = self.append_seek_end(ac, 0x0d)?;
        let pgno = ac.cur.page();
        let hdr = hdr_of(pgno);
        let greater = if n_cell == 0 {
            pgno == ac.root
        } else {
            let d = self.pager.read_page(pgno)?;
            let off = get2(d, hdr + 8 + 2 * (n_cell - 1));
            if off >= hdr + 8 + 2 * n_cell && off < usable {
                let c = &d[off..usable];
                let mut n = 0;
                while n < 8 && n < c.len() && c[n] & 0x80 != 0 {
                    n += 1;
                }
                n += 1;
                matches!(c.get(n..).map(get_varint_i64), Some(Ok((last, _))) if rowid > last)
            } else {
                false
            }
        };
        if greater {
            return self.append_table_inner(ac, rowid, payload, false);
        }
        ac.valid = false;
        self.insert_table_preformat_c(ac.root, rowid, payload)
    }
}
