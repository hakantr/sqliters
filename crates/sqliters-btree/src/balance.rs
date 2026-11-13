//! Port of the rebalancing machinery of C SQLite 3.54.1 (`sqlite/src/btree.c`): `balance()`,
//! `balance_nonroot()`, `balance_deeper()`, `balance_quick()`, `copyNodeContent()`, the
//! `CellArray` helpers (`populateCellCache`, `cachedCellSize`, `rebuildPage`, `editPage`,
//! `pageInsertArray`, `pageFreeArray`) and the page primitives they need (`dropCell`,
//! `insertCell`, `allocateSpace`, `pageFindSlot`, `defragmentPage`, `btreeComputeFreeSpace`,
//! `zeroPage`, `setChildPtrmaps`, `ptrmapPutOvflPtr`, the four `xCellSize` methods).
//!
//! C works on `MemPage` objects that live in the page cache and on raw pointers into them. Here
//! the pages being redistributed (up to three old siblings, up to five new ones) are copied into
//! scratch buffers, edited there with the same byte operations C performs, and copied back to
//! the pager under their final page numbers; the parent page is edited in place in the pager.
//! A cell of the `CellArray` is `(buffer, offset)` instead of a pointer. Every decision — which
//! siblings take part, how the cells are distributed, which pages are allocated and freed and
//! in which order, how divider cells are built, whether a page is edited or rebuilt — is taken
//! from the same values, in the same order, as in C.
//!
//! Step H1 of the alignment work routes every delete through this module. The insert side
//! still uses its own split code; [`Btree::balance`] already handles overfull pages (overflow
//! cells, `balance_quick`, `balance_deeper`), so the insert side can adopt it by handing it a
//! [`BalCursor`] whose current page carries the cell that did not fit (see `BalCursor::ovfl`).

#![allow(unexpected_cfgs)]

use super::*;
use sqliters_pager::DB_HEADER_SIZE;

/// `NB` in btree.c: pages taking part in one `balance_nonroot`.
const NB: usize = 3;
/// `apNew[NB+2]`.
const NNEW: usize = NB + 2;
/// Buffer id of the divider-cell copies (`aSpace1`).
const BUF_SPACE1: u32 = NNEW as u32;
/// Buffer id of the overflow cells of the page being balanced (`MemPage.apOvfl[]`).
const BUF_OVFL: u32 = NNEW as u32 + 1;
const OFF_BITS: u32 = 24;
const OFF_MASK: u32 = (1 << OFF_BITS) - 1;

#[cold]
fn corrupt(msg: &'static str) -> SqliteError {
    SqliteError::corrupt(msg)
}

#[inline(always)]
pub(crate) fn at(d: &[u8], i: usize) -> u8 {
    d.get(i).copied().unwrap_or(0)
}

#[inline(always)]
pub(crate) fn get2(d: &[u8], i: usize) -> usize {
    (usize::from(at(d, i)) << 8) | usize::from(at(d, i + 1))
}

#[inline(always)]
pub(crate) fn put2(d: &mut [u8], i: usize, v: usize) {
    d[i] = (v >> 8) as u8;
    d[i + 1] = v as u8;
}

#[inline(always)]
pub(crate) fn get4(d: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([at(d, i), at(d, i + 1), at(d, i + 2), at(d, i + 3)])
}

#[inline(always)]
pub(crate) fn put4(d: &mut [u8], i: usize, v: u32) {
    d[i..i + 4].copy_from_slice(&v.to_be_bytes());
}

#[inline(always)]
pub(crate) const fn hdr_of(pgno: Pgno) -> usize {
    if pgno == 1 { DB_HEADER_SIZE } else { 0 }
}

/// What `decodeFlags` derives from the flag byte of a b-tree page.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PgKind {
    pub flags: u8,
    pub leaf: bool,
    pub int_key: bool,
    pub int_key_leaf: bool,
    /// `MemPage.childPtrSize`: 4 on interior pages, 0 on leaves.
    pub child_ptr: usize,
    pub max_local: u32,
    pub min_local: u32,
}

impl PgKind {
    pub(crate) fn decode(flags: u8, usable: u32) -> Result<Self> {
        let (leaf, int_key) = match flags {
            0x0d => (true, true),
            0x05 => (false, true),
            0x0a => (true, false),
            0x02 => (false, false),
            _ => return Err(corrupt("invalid b-tree page type")),
        };
        let min_local = (usable - 12) * 32 / 255 - 23;
        let max_local = if int_key {
            usable - 35
        } else {
            (usable - 12) * 64 / 255 - 23
        };
        Ok(Self {
            flags,
            leaf,
            int_key,
            int_key_leaf: leaf && int_key,
            child_ptr: if leaf { 0 } else { 4 },
            max_local,
            min_local,
        })
    }
}

/// `MemPage.xCellSize`: the bytes the cell at `pc` occupies in the cell content area
/// (`cellSizePtr`, `cellSizePtrIdxLeaf`, `cellSizePtrNoPayload`, `cellSizePtrTableLeaf`,
/// 16-bit truncation included). Bytes past the end of `d` read as zero.
pub(crate) fn cell_size(k: &PgKind, usable: u32, d: &[u8], pc: usize) -> usize {
    let mut it = pc;
    if !k.leaf {
        it += 4;
    }
    if k.int_key && !k.leaf {
        let end = it + 9;
        loop {
            let b = at(d, it);
            it += 1;
            if b & 0x80 == 0 || it >= end {
                break;
            }
        }
        return usize::from((it - pc) as u16);
    }
    let mut n_size = u32::from(at(d, it));
    if n_size >= 0x80 {
        let end = it + 8;
        n_size &= 0x7f;
        loop {
            it += 1;
            n_size = (n_size << 7) | u32::from(at(d, it) & 0x7f);
            if at(d, it) < 0x80 || it >= end {
                break;
            }
        }
    }
    it += 1;
    if k.int_key {
        let mut n = 0;
        loop {
            let b = at(d, it);
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
    if n_size <= k.max_local {
        let mut total = n_size.wrapping_add(head);
        if k.leaf && total < 4 {
            total = 4;
        }
        usize::from(total as u16)
    } else {
        let mut local = k.min_local + (n_size - k.min_local) % (usable - 4);
        if local > k.max_local {
            local = k.min_local;
        }
        usize::from(local.wrapping_add(4 + u32::from(head as u16)) as u16)
    }
}

/// `CellInfo` (the fields the balancer needs).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CellInfo {
    pub n_key: i64,
    pub n_payload: u32,
    pub n_local: u32,
    pub n_size: usize,
    /// Offset of the payload inside the cell (`info.pPayload - pCell`).
    pub n_hdr: usize,
}

/// `MemPage.xParseCell`.
pub(crate) fn parse_cell(k: &PgKind, usable: u32, d: &[u8], pc: usize) -> Result<CellInfo> {
    if k.int_key && !k.leaf {
        let s = d
            .get(pc + 4..)
            .ok_or_else(|| corrupt("table interior cell out of page"))?;
        let (key, n) = get_varint_i64(s)?;
        return Ok(CellInfo {
            n_key: key,
            n_payload: 0,
            n_local: 0,
            n_size: 4 + n,
            n_hdr: 4 + n,
        });
    }
    let mut it = pc + k.child_ptr;
    let mut n_payload = u32::from(at(d, it));
    if n_payload >= 0x80 {
        let end = it + 8;
        n_payload &= 0x7f;
        loop {
            it += 1;
            n_payload = (n_payload << 7) | u32::from(at(d, it) & 0x7f);
            if at(d, it) < 0x80 || it >= end {
                break;
            }
        }
    }
    it += 1;
    let mut n_key = i64::from(n_payload);
    if k.int_key {
        let s = d
            .get(it..)
            .ok_or_else(|| corrupt("table leaf cell out of page"))?;
        let (key, n) = get_varint_i64(s)?;
        n_key = key;
        it += n;
    }
    let head = it - pc;
    if n_payload <= k.max_local {
        let mut n_size = usize::from((n_payload as u16).wrapping_add(head as u16));
        if n_size < 4 {
            n_size = 4;
        }
        Ok(CellInfo {
            n_key,
            n_payload,
            n_local: n_payload,
            n_size,
            n_hdr: head,
        })
    } else {
        let surplus = k.min_local + (n_payload - k.min_local) % (usable - 4);
        let n_local = if surplus <= k.max_local {
            surplus
        } else {
            k.min_local
        };
        Ok(CellInfo {
            n_key,
            n_payload,
            n_local,
            n_size: usize::from((head as u32 + n_local) as u16) + 4,
            n_hdr: head,
        })
    }
}

/// `btreeComputeFreeSpace`: the free bytes of a page (gap, freeblocks and fragments).
///
/// C keeps this number in `MemPage.nFree` and updates it on every change; this engine has no
/// per-page object, so the delete paths recompute it from the page (one pass over the
/// freeblock chain) when the cheap bounds of [`leaf_needs_balance`] cannot decide.
pub(crate) fn compute_free_space(
    d: &[u8],
    hdr: usize,
    child_ptr: usize,
    n_cell: usize,
    usable: usize,
) -> Result<i32> {
    let Some(d) = d.get(..usable) else {
        return Err(corrupt("b-tree page buffer too small"));
    };
    if hdr + 8 > usable || usable < 4 {
        return Err(corrupt("b-tree page buffer too small"));
    }
    let be16 = |i: usize| usize::from(u16::from_be_bytes([d[i], d[i + 1]]));
    let top = match be16(hdr + 5) {
        0 => 65_536,
        t => t,
    };
    let i_cell_first = hdr + 8 + child_ptr + 2 * n_cell;
    let i_cell_last = usable - 4;
    let mut pc = be16(hdr + 1);
    let mut n_free = usize::from(d[hdr + 7]) + top;
    if pc > 0 {
        if pc < top {
            return Err(corrupt("freeblock before the cell content area"));
        }
        let (mut next, mut size);
        loop {
            if pc > i_cell_last {
                return Err(corrupt("freeblock off the end of the page"));
            }
            // One 4-byte freeblock header: next (2 bytes), size (2 bytes).
            let h = &d[pc..pc + 4];
            next = usize::from(u16::from_be_bytes([h[0], h[1]]));
            size = usize::from(u16::from_be_bytes([h[2], h[3]]));
            if size < 4 {
                return Err(corrupt("freeblock smaller than 4 bytes"));
            }
            n_free += size;
            if next < pc + size + 4 {
                break;
            }
            pc = next;
        }
        if next > 0 {
            return Err(corrupt("freeblocks not in ascending order"));
        }
        if pc + size > usable {
            return Err(corrupt("last freeblock extends past page end"));
        }
    }
    if n_free > usable || n_free < i_cell_first {
        return Err(corrupt("free space of page out of range"));
    }
    Ok((n_free - i_cell_first) as i32)
}

/// Header facts of a b-tree page: kind, cell count and free bytes (`btreeInitPage` +
/// `btreeComputeFreeSpace`).
pub(crate) fn page_state(d: &[u8], pgno: Pgno, usable: usize) -> Result<(PgKind, usize, i32)> {
    let hdr = hdr_of(pgno);
    if d.len() < usable || hdr + 12 > usable {
        return Err(corrupt("b-tree page buffer too small"));
    }
    let kind = PgKind::decode(d[hdr], usable as u32)?;
    let n_cell = get2(d, hdr + 3);
    // `MX_CELL(pBt)`: `(pageSize-8)/6`.
    if n_cell > (d.len() - 8) / 6 {
        return Err(corrupt("too many cells on b-tree page"));
    }
    let n_free = compute_free_space(d, hdr, kind.child_ptr, n_cell, usable)?;
    Ok((kind, n_cell, n_free))
}

/// `true` when the page — a leaf a cell was just removed from — is more than two thirds free,
/// the test `sqlite3BtreeDelete` makes before calling `balance()`
/// (`pPage->nFree*3 > usableSize*2`). `Err` for a page whose free-space bookkeeping is corrupt.
#[inline]
pub(crate) fn leaf_needs_balance(d: &[u8], hdr: usize, usable: usize) -> Result<bool> {
    let n_cell = get2(d, hdr + 3);
    let i_cell_first = hdr + 8 + 2 * n_cell;
    // Cheap bound first: every cell takes at least 4 bytes, so the page cannot be two thirds
    // free once pointer array and minimum cell bytes already fill a third of it.
    if (i_cell_first + 4 * n_cell) * 3 >= usable {
        return Ok(false);
    }
    let n_free = if at(d, hdr + 1) == 0 && at(d, hdr + 2) == 0 {
        // No freeblock: gap plus fragments.
        let top = {
            let t = get2(d, hdr + 5);
            if t == 0 { 65_536 } else { t }
        };
        let n = usize::from(d[hdr + 7]) + top;
        if n > usable || n < i_cell_first {
            return Err(corrupt("free space of page out of range"));
        }
        (n - i_cell_first) as i32
    } else {
        compute_free_space(d, hdr, 0, n_cell, usable)?
    };
    Ok(n_free as usize * 3 > usable * 2)
}

/// `pageFindSlot`: a freeblock of at least `n_byte` bytes, unlinked or shortened. The second
/// value is C's `*pRc = SQLITE_CORRUPT`.
fn page_find_slot(d: &mut [u8], hdr: usize, usable: usize, n_byte: usize) -> (Option<usize>, bool) {
    let mut i_addr = hdr + 1;
    let mut pc = get2(d, i_addr);
    let max_pc = usable as isize - n_byte as isize;
    while (pc as isize) <= max_pc {
        let size = get2(d, pc + 2);
        if size >= n_byte {
            let x = size - n_byte;
            if x < 4 {
                if d[hdr + 7] > 57 {
                    return (None, false);
                }
                let nx = [at(d, pc), at(d, pc + 1)];
                d[i_addr..i_addr + 2].copy_from_slice(&nx);
                d[hdr + 7] += x as u8;
                return (Some(pc), false);
            } else if (x + pc) as isize > max_pc {
                return (None, true);
            }
            put2(d, pc + 2, x);
            return (Some(pc + x), false);
        }
        i_addr = pc;
        pc = get2(d, pc);
        if pc <= i_addr {
            return (None, pc != 0);
        }
    }
    (None, pc as isize > max_pc + n_byte as isize - 4)
}

/// `defragmentPage`.
#[allow(clippy::too_many_arguments)]
fn defragment_page(
    d: &mut [u8],
    hdr: usize,
    k: &PgKind,
    usable: usize,
    n_cell: usize,
    n_free: i32,
    n_max_frag: i32,
    tmp: &mut Vec<u8>,
) -> Result<()> {
    let cell_offset = hdr + 8 + k.child_ptr;
    let i_cell_first = cell_offset + 2 * n_cell;
    let cbrk: usize;
    let mut fast_done = None;
    if i32::from(d[hdr + 7]) <= n_max_frag {
        let i_free = get2(d, hdr + 1);
        if i_free > usable - 4 {
            return Err(corrupt("freeblock off the page in defragment"));
        }
        if i_free != 0 {
            let i_free2 = get2(d, i_free);
            if i_free2 > usable - 4 {
                return Err(corrupt("freeblock off the page in defragment"));
            }
            if i_free2 == 0 || (d[i_free2] == 0 && d[i_free2 + 1] == 0) {
                let mut sz2 = 0usize;
                let mut sz = get2(d, i_free + 2);
                let top = get2(d, hdr + 5);
                if top >= i_free {
                    return Err(corrupt("freeblock before content area in defragment"));
                }
                if i_free2 != 0 {
                    if i_free + sz > i_free2 {
                        return Err(corrupt("overlapping freeblocks in defragment"));
                    }
                    sz2 = get2(d, i_free2 + 2);
                    if i_free2 + sz2 > usable {
                        return Err(corrupt("freeblock past page end in defragment"));
                    }
                    d.copy_within(i_free + sz..i_free2, i_free + sz + sz2);
                    sz += sz2;
                } else if i_free + sz > usable {
                    return Err(corrupt("freeblock past page end in defragment"));
                }
                let c = top + sz;
                d.copy_within(top..i_free, c);
                let mut p = cell_offset;
                while p < i_cell_first {
                    let pc = get2(d, p);
                    if pc < i_free {
                        put2(d, p, pc + sz);
                    } else if pc < i_free2 {
                        put2(d, p, pc + sz2);
                    }
                    p += 2;
                }
                fast_done = Some(c);
            }
        }
    }
    if let Some(c) = fast_done {
        cbrk = c;
    } else {
        let mut c = usable;
        let i_cell_last = usable - 4;
        let i_cell_start = get2(d, hdr + 5);
        if n_cell > 0 {
            tmp.clear();
            tmp.extend_from_slice(&d[..usable]);
            for i in 0..n_cell {
                let p = cell_offset + i * 2;
                let pc = get2(d, p);
                if pc > i_cell_last {
                    return Err(corrupt("cell offset past page end in defragment"));
                }
                let size = cell_size(k, usable as u32, tmp, pc);
                if c < i_cell_start + size || pc + size > usable {
                    return Err(corrupt("cell does not fit in defragment"));
                }
                c -= size;
                put2(d, p, c);
                d[c..c + size].copy_from_slice(&tmp[pc..pc + size]);
            }
        }
        d[hdr + 7] = 0;
        cbrk = c;
    }
    if i64::from(d[hdr + 7]) + cbrk as i64 - i_cell_first as i64 != i64::from(n_free)
        || cbrk < i_cell_first
    {
        return Err(corrupt("free space mismatch after defragment"));
    }
    put2(d, hdr + 5, cbrk);
    d[hdr + 1] = 0;
    d[hdr + 2] = 0;
    d[i_cell_first..cbrk].fill(0);
    Ok(())
}

/// How [`allocate_space`] found the room (decision trace): the gap between the cell pointer
/// array and the cell content area, a freeblock, or the gap after defragmenting the page.
pub(crate) const SLOT_GAP: u8 = 0;
pub(crate) const SLOT_FREEBLOCK: u8 = 1;
pub(crate) const SLOT_DEFRAG: u8 = 2;

#[cfg(sqliters_dtrace)]
pub(crate) const fn slot_name(how: u8) -> &'static str {
    match how {
        SLOT_FREEBLOCK => "freeblock",
        SLOT_DEFRAG => "defrag",
        _ => "gap",
    }
}

/// `allocateSpace`: room for `n_byte` bytes of cell content; the caller guarantees that
/// `n_byte + 2 <= n_free`. Returns the offset and how it was found (`SLOT_*`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn allocate_space(
    d: &mut [u8],
    hdr: usize,
    k: &PgKind,
    usable: usize,
    n_cell: usize,
    n_free: i32,
    n_byte: usize,
    tmp: &mut Vec<u8>,
) -> Result<(usize, u8)> {
    let mut how = SLOT_GAP;
    let gap = hdr + 8 + k.child_ptr + 2 * n_cell;
    let mut top = get2(d, hdr + 5);
    if gap > top {
        if top == 0 && usable == 65_536 {
            top = 65_536;
        } else {
            return Err(corrupt("cell content area overlaps cell pointers"));
        }
    } else if top > usable {
        return Err(corrupt("cell content area past page end"));
    }
    if (d[hdr + 2] != 0 || d[hdr + 1] != 0) && gap + 2 <= top {
        match page_find_slot(d, hdr, usable, n_byte) {
            (Some(g2), _) => {
                if g2 <= gap {
                    return Err(corrupt("freeblock inside the cell pointer array"));
                }
                return Ok((g2, SLOT_FREEBLOCK));
            }
            (None, true) => return Err(corrupt("corrupt freeblock chain")),
            (None, false) => {}
        }
    }
    if gap + 2 + n_byte > top {
        let n_max_frag = 4.min(n_free - (2 + n_byte as i32));
        defragment_page(d, hdr, k, usable, n_cell, n_free, n_max_frag, tmp)?;
        how = SLOT_DEFRAG;
        top = {
            let t = get2(d, hdr + 5);
            if t == 0 { 65_536 } else { t }
        };
        if gap + 2 + n_byte > top {
            return Err(corrupt("no room for cell after defragment"));
        }
    }
    top -= n_byte;
    put2(d, hdr + 5, top);
    Ok((top, how))
}

/// `dropCell`: removes cell `idx` (`sz` bytes) from a page, updating `n_cell` / `n_free` the
/// way C updates `MemPage.nCell` / `nFree`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drop_cell(
    d: &mut [u8],
    hdr: usize,
    child_ptr: usize,
    usable: usize,
    n_cell: &mut usize,
    n_free: &mut i32,
    idx: usize,
    sz: usize,
    secure: bool,
) -> Result<()> {
    if idx >= *n_cell {
        return Err(corrupt("dropCell index out of range"));
    }
    let base = hdr + 8 + child_ptr;
    let ptr = base + 2 * idx;
    let pc = get2(d, ptr);
    if pc + sz > usable {
        return Err(corrupt("cell extends past page end in dropCell"));
    }
    if !raw_page_free_space(d, hdr, usable, pc, sz, secure) {
        return Err(corrupt("freeSpace failed in dropCell"));
    }
    *n_free += sz as i32;
    *n_cell -= 1;
    if *n_cell == 0 {
        d[hdr + 1..hdr + 5].fill(0);
        d[hdr + 7] = 0;
        put2(d, hdr + 5, usable);
        *n_free = (usable - hdr - child_ptr - 8) as i32;
    } else {
        d.copy_within(ptr + 2..base + 2 * (*n_cell + 1), ptr);
        put2(d, hdr + 3, *n_cell);
        *n_free += 2;
    }
    Ok(())
}

/// `zeroPage`.
pub(crate) fn zero_page(d: &mut [u8], hdr: usize, flags: u8, usable: usize, secure: bool) {
    if secure {
        d[hdr..usable].fill(0);
    }
    d[hdr] = flags;
    d[hdr + 1..hdr + 5].fill(0);
    d[hdr + 7] = 0;
    put2(d, hdr + 5, usable);
}

/// Overflow cells of one page (`MemPage.apOvfl[]` / `aiOvfl[]` / `nOverflow`) together with the
/// bytes they point at (C: `pTmpSpace`, `aOvflSpace`).
#[derive(Default, Debug)]
pub(crate) struct OvflCells {
    pub n: usize,
    pub idx: [u16; 4],
    pub off: [u32; 4],
    pub len: [u32; 4],
    pub space: Vec<u8>,
}

impl OvflCells {
    /// Appends an overflow cell (`pPage->apOvfl[j] = pCell; pPage->aiOvfl[j] = i`).
    pub(crate) fn push(&mut self, idx: usize, cell: &[u8]) -> Result<()> {
        if self.n >= 4 {
            return Err(corrupt("too many overflow cells on one page"));
        }
        self.idx[self.n] = idx as u16;
        self.off[self.n] = self.space.len() as u32;
        self.len[self.n] = cell.len() as u32;
        self.space.extend_from_slice(cell);
        // (C cells may be read up to a few bytes past their end by the size functions.)
        self.n += 1;
        Ok(())
    }

    /// [`Self::push`] for a cell of `len` bytes that `fill` writes in place.
    pub(crate) fn push_with(
        &mut self,
        idx: usize,
        len: usize,
        fill: impl FnOnce(&mut [u8]),
    ) -> Result<()> {
        if self.n >= 4 {
            return Err(corrupt("too many overflow cells on one page"));
        }
        let start = self.space.len();
        self.space.resize(start + len, 0);
        fill(&mut self.space[start..]);
        self.idx[self.n] = idx as u16;
        self.off[self.n] = start as u32;
        self.len[self.n] = len as u32;
        self.n += 1;
        Ok(())
    }
}

/// The part of a `BtCursor` that `balance()` uses: the page path from the root
/// (`apPage[]` / `aiIdx[]` / `iPage`) and the overflow cells of the pages on it.
pub(crate) struct BalCursor {
    pub pg: [Pgno; BTCURSOR_MAX_DEPTH],
    pub ai: [u16; BTCURSOR_MAX_DEPTH],
    pub i_page: usize,
    /// `(level, cells)`: overflow cells of `pg[level]`. A page that received a cell which did
    /// not fit carries it here until `balance()` has dealt with it.
    pub ovfl: Vec<(usize, OvflCells)>,
    /// `pCur->hints & BTREE_BULKLOAD`.
    pub bulk: bool,
}

impl BalCursor {
    pub(crate) fn new(root: Pgno) -> Self {
        let mut pg = [0; BTCURSOR_MAX_DEPTH];
        pg[0] = root;
        Self {
            pg,
            ai: [0; BTCURSOR_MAX_DEPTH],
            i_page: 0,
            ovfl: Vec::new(),
            bulk: false,
        }
    }

    /// `moveToChild`: records that child slot `idx` of the current page leads to `child`.
    #[inline]
    pub(crate) fn descend(&mut self, idx: usize, child: Pgno) -> Result<()> {
        if self.i_page + 1 >= BTCURSOR_MAX_DEPTH {
            return Err(corrupt("b-tree deeper than BTCURSOR_MAX_DEPTH"));
        }
        self.ai[self.i_page] = idx as u16;
        self.i_page += 1;
        self.pg[self.i_page] = child;
        Ok(())
    }

    #[inline]
    pub(crate) const fn page(&self) -> Pgno {
        self.pg[self.i_page]
    }

    fn ovfl_n(&self, level: usize) -> usize {
        self.ovfl
            .iter()
            .find(|(l, _)| *l == level)
            .map_or(0, |(_, o)| o.n)
    }

    fn take_ovfl(&mut self, level: usize) -> OvflCells {
        match self.ovfl.iter().position(|(l, _)| *l == level) {
            Some(i) => self.ovfl.swap_remove(i).1,
            None => OvflCells::default(),
        }
    }

    pub(crate) fn set_ovfl(&mut self, level: usize, cells: OvflCells) {
        self.ovfl.retain(|(l, _)| *l != level);
        if cells.n > 0 {
            self.ovfl.push((level, cells));
        }
    }
}

/// Decision-trace helper: the divider key as printed by both engines (`r<rowid>` for table
/// trees, `h<fnv1a32 of the cell after its child pointer>` for index trees).
#[cfg(sqliters_dtrace)]
fn tr_key(cell: &[u8], int_key: bool) -> String {
    if int_key {
        match get_varint_i64(&cell[4..]) {
            Ok((k, _)) => format!("r{k}"),
            Err(_) => "r?".to_string(),
        }
    } else {
        let mut h: u32 = 0x811c_9dc5;
        for &b in &cell[4..] {
            h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
        }
        format!("h{h:08x}")
    }
}

/// Working state of one `balance_nonroot` call: the sibling buffers and the `CellArray`.
struct NonRoot {
    usable: usize,
    /// Kind of the sibling pages (`b.pRef`).
    kind: PgKind,
    secure: bool,
    /// `apOld[i]` / `apNew[i]` page images (index = sibling slot; the page number a slot ends
    /// up with is decided separately).
    bufs: Vec<Vec<u8>>,
    /// `aSpace1`: copies of the divider cells taken out of the parent.
    space1: Vec<u8>,
    /// Bytes of the overflow cells of the page being balanced.
    ovfl_space: Vec<u8>,
    /// `b.apCell[]` as `(buffer << OFF_BITS) | offset`.
    ap: Vec<u32>,
    /// `b.szCell[]` (0 = not computed yet).
    sz: Vec<u16>,
    /// `sqlite3PagerTempSpace`.
    tmp: Vec<u8>,
    /// Cells currently stored on each sibling buffer (`MemPage.nCell`).
    pg_ncell: [usize; NNEW],
    /// `MemPage.nOverflow` / `aiOvfl[]` of each sibling (only the balanced page can have any).
    pg_novfl: [usize; NNEW],
    ai_ovfl: [u16; 4],
}

impl NonRoot {
    #[inline]
    fn cell_loc(&self, i: usize) -> (u32, usize) {
        let v = self.ap[i];
        (v >> OFF_BITS, (v & OFF_MASK) as usize)
    }

    #[inline]
    fn buf(&self, b: u32) -> &[u8] {
        match b {
            BUF_SPACE1 => &self.space1,
            BUF_OVFL => &self.ovfl_space,
            _ => &self.bufs[b as usize],
        }
    }

    /// `cachedCellSize`.
    fn cached_cell_size(&mut self, n: usize) -> usize {
        if self.sz[n] != 0 {
            return usize::from(self.sz[n]);
        }
        let (b, off) = self.cell_loc(n);
        let s = cell_size(&self.kind, self.usable as u32, self.buf(b), off);
        self.sz[n] = s as u16;
        s
    }

    /// `populateCellCache`.
    fn populate_cell_cache(&mut self, idx: usize, n: usize) {
        for i in idx..idx + n {
            self.cached_cell_size(i);
        }
    }

    /// `memmove(dst, apCell[i], sz)` into sibling buffer `pg`. `false` when the source range
    /// does not lie inside its buffer (C: the `pEnd` straddle checks).
    fn copy_cell(&mut self, pg: usize, dst: usize, i: usize, sz: usize) -> bool {
        let (b, off) = self.cell_loc(i);
        if dst + sz > self.bufs[pg].len() {
            return false;
        }
        if b as usize == pg {
            if off + sz > self.bufs[pg].len() {
                return false;
            }
            self.bufs[pg].copy_within(off..off + sz, dst);
            return true;
        }
        match b {
            BUF_SPACE1 => {
                let Some(src) = self.space1.get(off..off + sz) else {
                    return false;
                };
                self.bufs[pg][dst..dst + sz].copy_from_slice(src);
            }
            BUF_OVFL => {
                let Some(src) = self.ovfl_space.get(off..off + sz) else {
                    return false;
                };
                self.bufs[pg][dst..dst + sz].copy_from_slice(src);
            }
            _ => {
                let b = b as usize;
                let (src, dstb) = if b < pg {
                    let (lo, hi) = self.bufs.split_at_mut(pg);
                    (&lo[b], &mut hi[0])
                } else {
                    let (lo, hi) = self.bufs.split_at_mut(b);
                    (&hi[0], &mut lo[pg])
                };
                let Some(s) = src.get(off..off + sz) else {
                    return false;
                };
                dstb[dst..dst + sz].copy_from_slice(s);
            }
        }
        true
    }

    /// `rebuildPage`.
    fn rebuild_page(&mut self, i_first: usize, n_cell: usize, pg: usize) -> Result<()> {
        let usable = self.usable;
        let base = 8 + self.kind.child_ptr;
        let mut j = get2(&self.bufs[pg], 5);
        if j > usable {
            j = 0;
        }
        self.tmp.clear();
        self.tmp.resize(usable, 0);
        self.tmp[j..usable].copy_from_slice(&self.bufs[pg][j..usable]);
        let mut p_data = usable;
        let mut cellptr = base;
        for i in i_first..i_first + n_cell {
            let sz = usize::from(self.sz[i]);
            let (b, off) = self.cell_loc(i);
            if p_data < sz {
                return Err(corrupt("cells do not fit in rebuildPage"));
            }
            p_data -= sz;
            if p_data < cellptr + 2 {
                return Err(corrupt("cells do not fit in rebuildPage"));
            }
            put2(&mut self.bufs[pg], cellptr, p_data);
            cellptr += 2;
            if b as usize == pg && off >= j && off < usable {
                if off + sz > usable {
                    return Err(corrupt("cell extends past page end in rebuildPage"));
                }
                self.bufs[pg][p_data..p_data + sz].copy_from_slice(&self.tmp[off..off + sz]);
            } else if !self.copy_cell(pg, p_data, i, sz) {
                return Err(corrupt("cell extends past its page in rebuildPage"));
            }
        }
        self.pg_ncell[pg] = n_cell;
        self.pg_novfl[pg] = 0;
        let d = &mut self.bufs[pg];
        put2(d, 1, 0);
        put2(d, 3, n_cell);
        put2(d, 5, p_data);
        d[7] = 0;
        Ok(())
    }

    /// `pageInsertArray`: `true` = the cells do not fit without defragmenting.
    fn page_insert_array(
        &mut self,
        pg: usize,
        p_begin: usize,
        p_data: &mut usize,
        mut cellptr: usize,
        i_first: usize,
        n_cell: usize,
    ) -> bool {
        if n_cell == 0 {
            return false;
        }
        let usable = self.usable;
        for i in i_first..i_first + n_cell {
            let sz = self.cached_cell_size(i);
            let d = &mut self.bufs[pg];
            let mut slot = None;
            if !(d[1] == 0 && d[2] == 0) {
                slot = page_find_slot(d, 0, usable, sz).0;
            }
            let slot = match slot {
                Some(s) => s,
                None => {
                    if *p_data < p_begin + sz {
                        return true;
                    }
                    *p_data -= sz;
                    *p_data
                }
            };
            if !self.copy_cell(pg, slot, i, sz) {
                return true;
            }
            put2(&mut self.bufs[pg], cellptr, slot);
            cellptr += 2;
        }
        false
    }

    /// `pageFreeArray`: returns the number of cells of the range that were stored on the page.
    fn page_free_array(&mut self, pg: usize, i_first: usize, n_cell: usize) -> usize {
        let usable = self.usable;
        let p_start = 8 + self.kind.child_ptr;
        let mut n_ret = 0;
        let mut n_free = 0usize;
        let mut a_ofst = [0usize; 10];
        let mut a_after = [0usize; 10];
        for i in i_first..i_first + n_cell {
            let (b, off) = self.cell_loc(i);
            if b as usize == pg && off >= p_start && off < usable {
                let sz = usize::from(self.sz[i]);
                let i_ofst = off;
                let i_after = i_ofst + sz;
                let mut j = 0;
                while j < n_free {
                    if a_ofst[j] == i_after {
                        a_ofst[j] = i_ofst;
                        break;
                    } else if a_after[j] == i_ofst {
                        a_after[j] = i_after;
                        break;
                    }
                    j += 1;
                }
                if j >= n_free {
                    if n_free >= a_ofst.len() {
                        for j in 0..n_free {
                            raw_page_free_space(
                                &mut self.bufs[pg],
                                0,
                                usable,
                                a_ofst[j],
                                a_after[j] - a_ofst[j],
                                self.secure,
                            );
                        }
                        n_free = 0;
                    }
                    a_ofst[n_free] = i_ofst;
                    a_after[n_free] = i_after;
                    if i_after > usable {
                        return 0;
                    }
                    n_free += 1;
                }
                n_ret += 1;
            }
        }
        for j in 0..n_free {
            raw_page_free_space(
                &mut self.bufs[pg],
                0,
                usable,
                a_ofst[j],
                a_after[j] - a_ofst[j],
                self.secure,
            );
        }
        n_ret
    }

    /// `editPage`. Returns `true` when the page had to be rebuilt (`editpage_fail`).
    fn edit_page(&mut self, pg: usize, i_old: usize, i_new: usize, n_new: usize) -> Result<bool> {
        let base = 8 + self.kind.child_ptr;
        let p_begin = base + n_new * 2;
        let mut n_cell = self.pg_ncell[pg];
        let n_ovfl = self.pg_novfl[pg];
        let i_old_end = i_old + n_cell + n_ovfl;
        let i_new_end = i_new + n_new;
        let ok = 'edit: {
            if i_old < i_new {
                let n_shift = self.page_free_array(pg, i_old, i_new - i_old);
                if n_shift > n_cell {
                    return Err(corrupt("editPage removed more cells than the page holds"));
                }
                let d = &mut self.bufs[pg];
                let src = base + n_shift * 2;
                let end = (src + n_cell * 2).min(d.len());
                d.copy_within(src..end, base);
                n_cell -= n_shift;
            }
            if i_new_end < i_old_end {
                let n_tail = self.page_free_array(pg, i_new_end, i_old_end - i_new_end);
                if n_tail > n_cell {
                    return Err(corrupt("editPage removed more cells than the page holds"));
                }
                n_cell -= n_tail;
            }
            let mut p_data = get2(&self.bufs[pg], 5);
            if p_data < p_begin || p_data > self.bufs[pg].len() {
                break 'edit false;
            }
            if i_new < i_old {
                let n_add = n_new.min(i_old - i_new);
                let d = &mut self.bufs[pg];
                if base + n_add * 2 + n_cell * 2 > d.len() {
                    break 'edit false;
                }
                d.copy_within(base..base + n_cell * 2, base + n_add * 2);
                if self.page_insert_array(pg, p_begin, &mut p_data, base, i_new, n_add) {
                    break 'edit false;
                }
                n_cell += n_add;
            }
            for i in 0..n_ovfl {
                let i_cell = (i_old + usize::from(self.ai_ovfl[i])) as isize - i_new as isize;
                if i_cell >= 0 && (i_cell as usize) < n_new {
                    let i_cell = i_cell as usize;
                    let cellptr = base + i_cell * 2;
                    if n_cell > i_cell {
                        let d = &mut self.bufs[pg];
                        if cellptr + 2 + (n_cell - i_cell) * 2 > d.len() {
                            break 'edit false;
                        }
                        d.copy_within(cellptr..cellptr + (n_cell - i_cell) * 2, cellptr + 2);
                    }
                    n_cell += 1;
                    self.cached_cell_size(i_cell + i_new);
                    if self.page_insert_array(pg, p_begin, &mut p_data, cellptr, i_cell + i_new, 1)
                    {
                        break 'edit false;
                    }
                }
            }
            if n_cell > n_new {
                break 'edit false;
            }
            let cellptr = base + n_cell * 2;
            if self.page_insert_array(
                pg,
                p_begin,
                &mut p_data,
                cellptr,
                i_new + n_cell,
                n_new - n_cell,
            ) {
                break 'edit false;
            }
            self.pg_ncell[pg] = n_new;
            self.pg_novfl[pg] = 0;
            let d = &mut self.bufs[pg];
            put2(d, 3, n_new);
            put2(d, 5, p_data);
            true
        };
        if ok {
            return Ok(false);
        }
        if n_new < 1 {
            return Err(corrupt("editPage: no cells left for a sibling page"));
        }
        self.populate_cell_cache(i_new, n_new);
        self.rebuild_page(i_new, n_new, pg)?;
        Ok(true)
    }
}

impl Btree {
    fn bal_take_buf(&mut self, page_size: usize) -> Vec<u8> {
        match self.bal_pool.pop() {
            Some(mut b) => {
                b.clear();
                b.resize(page_size, 0);
                b
            }
            None => vec![0u8; page_size],
        }
    }

    fn bal_give_buf(&mut self, buf: Vec<u8>) {
        if self.bal_pool.len() < 8 {
            self.bal_pool.push(buf);
        }
    }

    /// `ptrmapPutOvflPtr(pPage, pSrc, pCell)`: when the cell at `off` of `d` (parsed as a cell
    /// of a page of kind `k`) has overflow pages, record `pgno` as the owner of the first one.
    fn ptrmap_put_ovfl_ptr(&mut self, pgno: Pgno, k: &PgKind, d: &[u8], off: usize) -> Result<()> {
        let usable = self.pager.usable_size();
        let info = parse_cell(k, usable, d, off)?;
        if info.n_local < info.n_payload {
            if off + info.n_size > d.len() {
                return Err(corrupt("cell with overflow extends past its page"));
            }
            let ovfl = get4(d, off + info.n_size - 4);
            self.ptrmap_put(ovfl, PtrmapType::Overflow1, pgno)?;
        }
        Ok(())
    }

    /// `setChildPtrmaps` for the page image `d` stored as page `pgno`.
    fn set_child_ptrmaps_raw(&mut self, pgno: Pgno, d: &[u8]) -> Result<()> {
        let usable = self.pager.usable_size();
        let hdr = hdr_of(pgno);
        let k = PgKind::decode(at(d, hdr), usable)?;
        let n_cell = get2(d, hdr + 3);
        let base = hdr + 8 + k.child_ptr;
        for i in 0..n_cell {
            let off = get2(d, base + 2 * i);
            self.ptrmap_put_ovfl_ptr(pgno, &k, d, off)?;
            if !k.leaf {
                self.ptrmap_put(get4(d, off), PtrmapType::BtreeNode, pgno)?;
            }
        }
        if !k.leaf {
            self.ptrmap_put(get4(d, hdr + 8), PtrmapType::BtreeNode, pgno)?;
        }
        Ok(())
    }

    /// `insertCell(pPage, i, pCell, sz, pTemp, iChild)` on page `pgno` (already writable).
    /// `cell` holds the `sz` bytes of the cell; with `child` its first four bytes are replaced
    /// by that page number. A cell that does not fit becomes an overflow cell in `ovfl`.
    /// Returns `true` when the cell was stored on the page.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_cell(
        &mut self,
        pgno: Pgno,
        k: &PgKind,
        n_cell: &mut usize,
        n_free: &mut i32,
        ovfl: &mut OvflCells,
        i: usize,
        cell: &[u8],
        child: Option<Pgno>,
    ) -> Result<bool> {
        let sz = cell.len();
        if ovfl.n > 0 || sz as i32 + 2 > *n_free {
            ovfl.push(i, cell)?;
            if let Some(c) = child {
                let o = ovfl.off[ovfl.n - 1] as usize;
                put4(&mut ovfl.space, o, c);
            }
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
        let usable = self.pager.usable_size() as usize;
        let hdr = hdr_of(pgno);
        let mut tmp = std::mem::take(&mut self.bal_tmp);
        let mut slot_how = SLOT_GAP;
        let res = (|| -> Result<usize> {
            let d = self.pager.write_page(pgno)?;
            let (idx, how) = allocate_space(d, hdr, k, usable, *n_cell, *n_free, sz, &mut tmp)?;
            slot_how = how;
            *n_free -= 2 + sz as i32;
            match child {
                Some(c) => {
                    d[idx + 4..idx + sz].copy_from_slice(&cell[4..]);
                    put4(d, idx, c);
                }
                None => d[idx..idx + sz].copy_from_slice(cell),
            }
            if i > *n_cell {
                return Err(corrupt("insertCell index out of range"));
            }
            let ins = hdr + 8 + k.child_ptr + i * 2;
            d.copy_within(ins..ins + 2 * (*n_cell - i), ins + 2);
            put2(d, ins, idx);
            *n_cell += 1;
            put2(d, hdr + 3, *n_cell);
            Ok(idx)
        })();
        self.bal_tmp = tmp;
        let idx = res?;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|ins|slot|ctx={} pg={} i={} sz={} o={} off={}",
                self.tr_ctx(),
                pgno,
                i,
                sz,
                slot_name(slot_how),
                idx
            );
        }
        let _ = slot_how;
        if self.is_auto_vacuum() {
            // The cell may point at an overflow page: that page now belongs to `pgno`.
            let info = {
                let d = self.pager.read_page(pgno)?;
                let info = parse_cell(k, usable as u32, d, idx)?;
                if info.n_local < info.n_payload {
                    if idx + info.n_size > d.len() {
                        return Err(corrupt("cell with overflow extends past its page"));
                    }
                    Some(get4(d, idx + info.n_size - 4))
                } else {
                    None
                }
            };
            if let Some(ov) = info {
                self.ptrmap_put(ov, PtrmapType::Overflow1, pgno)?;
            }
        }
        Ok(true)
    }

    /// `anotherValidCursor`: `SQLITE_CORRUPT` when a cursor in the valid state sits on `pgno`
    /// (the root about to be made deeper). Every cursor of the tree is saved before a write, so
    /// this only fires when two trees share a page.
    fn another_valid_cursor(&self, pgno: Pgno) -> Result<()> {
        for entry in &self.cursor_slots {
            if let Some(cur) = &entry.cursor
                && let BtreeCursorState::Valid { stack, .. } = &cur.state
                && stack.last().is_some_and(|top| top.pgno == pgno)
            {
                return Err(corrupt("another valid cursor on the page being balanced"));
            }
        }
        Ok(())
    }

    /// `balance_deeper`: the root `root` is overfull. Its content moves to a new child, the
    /// root becomes an interior page with that child as its only pointer. Returns the child
    /// (which now owns the root's overflow cells).
    fn balance_deeper(&mut self, root: Pgno) -> Result<Pgno> {
        let usable = self.pager.usable_size() as usize;
        let page_size = self.pager.page_size() as usize;
        let hdr = hdr_of(root);
        self.pager.write_page(root)?;
        let child = self.allocate_btree_page(AllocMode::Near(root))?;
        let mut cbuf = self.bal_take_buf(page_size);
        let res = (|| -> Result<()> {
            // copyNodeContent(pRoot, pChild)
            cbuf.copy_from_slice(self.pager.read_page(child)?);
            let (kind, n_cell, _n_free) = {
                let from = self.pager.read_page(root)?;
                let st = page_state(from, root, usable)?;
                let i_data = get2(from, hdr + 5);
                if i_data > usable {
                    return Err(corrupt("cell content offset past page end"));
                }
                cbuf[i_data..usable].copy_from_slice(&from[i_data..usable]);
                // `memcpy(&aTo[iToHdr], &aFrom[iFromHdr], pFrom->cellOffset + 2*pFrom->nCell)`
                // (`cellOffset` includes the header offset, as in C).
                let n = (hdr + 8 + st.0.child_ptr + 2 * st.1)
                    .min(from.len() - hdr)
                    .min(cbuf.len());
                cbuf[..n].copy_from_slice(&from[hdr..hdr + n]);
                st
            };
            // `btreeInitPage(pTo)` + `btreeComputeFreeSpace(pTo)`.
            page_state(&cbuf, child, usable)?;
            self.pager.write_page(child)?.copy_from_slice(&cbuf);
            if self.is_auto_vacuum() {
                self.set_child_ptrmaps_raw(child, &cbuf)?;
                self.ptrmap_put(child, PtrmapType::BtreeNode, root)?;
            }
            let _ = n_cell;
            let secure = self.zero_freed_cells();
            let d = self.pager.write_page(root)?;
            zero_page(d, hdr, kind.flags & !0x08, usable, secure);
            put4(d, hdr + 8, child);
            Ok(())
        })();
        self.bal_give_buf(cbuf);
        res?;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|bal|deeper|ctx={} root={} child={}",
                self.tr_ctx(),
                root,
                child
            );
        }
        Ok(child)
    }

    /// `balance_quick`: the right-most leaf `page` of a table tree got one overflow cell at its
    /// end. A new right sibling takes that cell; the divider for `page` goes into `parent`.
    /// Returns the overflow cells of `parent` (the divider, when it did not fit).
    fn balance_quick(
        &mut self,
        parent: Pgno,
        page: Pgno,
        page_ovfl: &OvflCells,
    ) -> Result<OvflCells> {
        let usable = self.pager.usable_size() as usize;
        let page_size = self.pager.page_size() as usize;
        let secure = self.zero_freed_cells();
        let (pkind, mut par_ncell, mut par_nfree) = {
            let d = self.pager.read_page(parent)?;
            page_state(d, parent, usable)?
        };
        let (kind, n_cell, _) = {
            let d = self.pager.read_page(page)?;
            page_state(d, page, usable)?
        };
        if n_cell == 0 {
            return Err(corrupt("balance_quick on an empty leaf"));
        }
        let pgno_new = self.allocate_btree_page(AllocMode::Any)?;
        let o = page_ovfl.off[0] as usize;
        let sz_cell = cell_size(&kind, usable as u32, &page_ovfl.space, o);
        if o + sz_cell > page_ovfl.space.len() || sz_cell + 10 > usable {
            return Err(corrupt("overflow cell out of range in balance_quick"));
        }
        let mut nbuf = self.bal_take_buf(page_size);
        nbuf.copy_from_slice(self.pager.read_page(pgno_new)?);
        zero_page(&mut nbuf, 0, 0x0d, usable, secure);
        // rebuildPage(&b, 0, 1, pNew)
        let p_data = usable - sz_cell;
        put2(&mut nbuf, 8, p_data);
        nbuf[p_data..p_data + sz_cell].copy_from_slice(&page_ovfl.space[o..o + sz_cell]);
        put2(&mut nbuf, 1, 0);
        put2(&mut nbuf, 3, 1);
        put2(&mut nbuf, 5, p_data);
        nbuf[7] = 0;
        self.pager.write_page(pgno_new)?.copy_from_slice(&nbuf);
        let res = (|| -> Result<OvflCells> {
            if self.is_auto_vacuum() {
                self.ptrmap_put(pgno_new, PtrmapType::BtreeNode, parent)?;
                if sz_cell as u32 > kind.min_local {
                    self.ptrmap_put_ovfl_ptr(pgno_new, &kind, &nbuf, p_data)?;
                }
            }
            // Divider: page number of `page` + the key of its right-most cell.
            let mut div = [0u8; 13];
            let mut n_out = 4;
            {
                let d = self.pager.read_page(page)?;
                let mut pc = get2(d, hdr_of(page) + 8 + 2 * (n_cell - 1));
                let stop = pc + 9;
                loop {
                    let b = at(d, pc);
                    pc += 1;
                    if b & 0x80 == 0 || pc >= stop {
                        break;
                    }
                }
                let stop = pc + 9;
                loop {
                    let b = at(d, pc);
                    div[n_out] = b;
                    n_out += 1;
                    pc += 1;
                    if b & 0x80 == 0 || pc >= stop {
                        break;
                    }
                }
            }
            let mut out = OvflCells::default();
            let at_end = par_ncell;
            self.insert_cell(
                parent,
                &pkind,
                &mut par_ncell,
                &mut par_nfree,
                &mut out,
                at_end,
                &div[..n_out],
                Some(page),
            )?;
            let d = self.pager.write_page(parent)?;
            put4(d, hdr_of(parent) + 8, pgno_new);
            Ok(out)
        })();
        self.bal_give_buf(nbuf);
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|bal|quick|ctx={} parent={} pg={} new={}",
                self.tr_ctx(),
                parent,
                page,
                pgno_new
            );
        }
        res
    }

    /// `balance()`: the page `cur` points at was modified; redistribute until every page on
    /// the path is neither overfull nor more than two thirds empty.
    #[cold]
    #[inline(never)]
    pub(crate) fn balance(&mut self, cur: &mut BalCursor) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        loop {
            let pgno = cur.page();
            let (kind, n_cell, n_free) = {
                let d = self.pager.read_page(pgno)?;
                page_state(d, pgno, usable)?
            };
            let n_ovfl = cur.ovfl_n(cur.i_page);
            if n_ovfl == 0 && n_free as usize * 3 <= usable * 2 {
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|bal|step|ctx={} pg={} ovfl=0 nfree={} o=stop",
                        self.tr_ctx(),
                        pgno,
                        n_free
                    );
                }
                break;
            }
            if cur.i_page == 0 {
                if n_ovfl > 0 {
                    self.another_valid_cursor(pgno)?;
                    if self.dtrace_persistent() {
                        sqliters_types::dtrace!(
                            "TR|bal|step|ctx={} pg={} ovfl={} nfree={} o=deeper",
                            self.tr_ctx(),
                            pgno,
                            n_ovfl,
                            n_free
                        );
                    }
                    let child = self.balance_deeper(pgno)?;
                    let o = cur.take_ovfl(0);
                    cur.i_page = 1;
                    cur.ai[0] = 0;
                    cur.pg[1] = child;
                    cur.set_ovfl(1, o);
                    continue;
                }
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|bal|step|ctx={} pg={} ovfl=0 nfree={} o=root-stop",
                        self.tr_ctx(),
                        pgno,
                        n_free
                    );
                }
                break;
            }
            let parent = cur.pg[cur.i_page - 1];
            let i_idx = usize::from(cur.ai[cur.i_page - 1]);
            self.pager.write_page(parent)?;
            let page_ovfl = cur.take_ovfl(cur.i_page);
            let par_ncell = {
                let d = self.pager.read_page(parent)?;
                get2(d, hdr_of(parent) + 3)
            };
            let par_out = if kind.int_key_leaf
                && page_ovfl.n == 1
                && usize::from(page_ovfl.idx[0]) == n_cell
                && parent != 1
                && par_ncell == i_idx
            {
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|bal|step|ctx={} pg={} ovfl=1 nfree={} o=quick",
                        self.tr_ctx(),
                        pgno,
                        n_free
                    );
                }
                self.balance_quick(parent, pgno, &page_ovfl)?
            } else {
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|bal|step|ctx={} pg={} ovfl={} nfree={} o=nonroot",
                        self.tr_ctx(),
                        pgno,
                        page_ovfl.n,
                        n_free
                    );
                }
                let par_in = cur.take_ovfl(cur.i_page - 1);
                self.balance_nonroot(parent, i_idx, par_in, page_ovfl, cur.i_page == 1, cur.bulk)?
            };
            cur.set_ovfl(cur.i_page - 1, par_out);
            cur.i_page -= 1;
        }
        Ok(())
    }

    /// `balance_nonroot(pParent, iParentIdx, aOvflSpace, isRoot, bBulk)`.
    ///
    /// `par_ovfl` are the overflow cells `parent` carries on entry (none, or the one at
    /// `i_parent_idx` left by a delete of an interior cell); `page_ovfl` those of the child
    /// being balanced. Returns the overflow cells of `parent` on exit (new dividers that did
    /// not fit).
    #[allow(clippy::too_many_lines)]
    fn balance_nonroot(
        &mut self,
        parent: Pgno,
        i_parent_idx: usize,
        par_ovfl: OvflCells,
        page_ovfl: OvflCells,
        is_root: bool,
        bulk: bool,
    ) -> Result<OvflCells> {
        let page_size = self.pager.page_size() as usize;
        let mut st = NonRoot {
            usable: self.pager.usable_size() as usize,
            kind: PgKind::decode(0x0d, self.pager.usable_size())?,
            secure: self.zero_freed_cells(),
            bufs: Vec::with_capacity(NNEW),
            space1: std::mem::take(&mut self.bal_space1),
            ovfl_space: page_ovfl.space,
            ap: std::mem::take(&mut self.bal_ap),
            sz: std::mem::take(&mut self.bal_sz),
            tmp: std::mem::take(&mut self.bal_tmp),
            pg_ncell: [0; NNEW],
            pg_novfl: [0; NNEW],
            ai_ovfl: page_ovfl.idx,
        };
        st.space1.clear();
        st.ap.clear();
        st.sz.clear();
        let res = self.balance_nonroot_inner(
            &mut st,
            parent,
            i_parent_idx,
            par_ovfl,
            page_ovfl.n,
            &page_ovfl.off,
            is_root,
            bulk,
            page_size,
        );
        self.bal_space1 = std::mem::take(&mut st.space1);
        self.bal_ap = std::mem::take(&mut st.ap);
        self.bal_sz = std::mem::take(&mut st.sz);
        self.bal_tmp = std::mem::take(&mut st.tmp);
        for b in st.bufs.drain(..) {
            self.bal_give_buf(b);
        }
        res
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::needless_range_loop
    )]
    fn balance_nonroot_inner(
        &mut self,
        st: &mut NonRoot,
        parent: Pgno,
        i_parent_idx: usize,
        par_ovfl: OvflCells,
        page_novfl: usize,
        page_ovfl_off: &[u32; 4],
        is_root: bool,
        bulk: bool,
        page_size: usize,
    ) -> Result<OvflCells> {
        let usable = st.usable;
        let auto_vac = self.is_auto_vacuum();
        let secure = st.secure;
        let par_hdr = hdr_of(parent);
        let b_bulk = usize::from(bulk);

        let (pkind, mut par_ncell, mut par_nfree) = {
            let d = self.pager.read_page(parent)?;
            page_state(d, parent, usable)?
        };
        if pkind.leaf {
            return Err(corrupt("parent of a balanced page is a leaf"));
        }
        let par_base = par_hdr + 12;
        let mut par_novfl = par_ovfl.n;
        if par_novfl > 1 || (par_novfl == 1 && usize::from(par_ovfl.idx[0]) != i_parent_idx) {
            return Err(corrupt("unexpected overflow cells on the parent page"));
        }

        // ---- Find the sibling pages to balance; drop their divider cells from the parent.
        let mut i = par_novfl + par_ncell;
        let nx_div;
        if i < 2 {
            nx_div = 0;
        } else {
            nx_div = if i_parent_idx == 0 {
                0
            } else if i_parent_idx == i {
                i - 2 + b_bulk
            } else {
                i_parent_idx - 1
            };
            i = 2 - b_bulk;
        }
        let n_old = i + 1;
        if i + nx_div < par_novfl || i + nx_div - par_novfl > par_ncell {
            return Err(corrupt("sibling index out of range in balance_nonroot"));
        }
        let p_right = {
            let d = self.pager.read_page(parent)?;
            if i + nx_div - par_novfl == par_ncell {
                par_hdr + 8
            } else {
                get2(d, par_base + 2 * (i + nx_div - par_novfl)) & (page_size - 1)
            }
        };
        let mut pgno = get4(self.pager.read_page(parent)?, p_right);

        let mut old_pgno = [0 as Pgno; NB];
        let mut old_nfree = [0i32; NB];
        let mut old_ncell = [0usize; NB];
        // apDiv[i] as (offset, len) into `div_stash` (copies taken before the cell is dropped).
        let mut div = [(0usize, 0usize); NB];
        let mut div_stash: Vec<u8> = Vec::new();
        let mut sz_new = [0i32; NNEW + 2];
        let mut cnt_new = [0i32; NNEW + 2];
        let mut cnt_old = [0i32; NNEW + 2];
        let mut n_max_cells = 0usize;
        for _ in 0..n_old {
            let b = self.bal_take_buf(page_size);
            st.bufs.push(b);
        }
        loop {
            // getAndInitPage(pBt, pgno, &apOld[i], 0)
            if pgno < 2 || pgno > self.pager.page_count() || pgno == parent {
                return Err(corrupt("invalid sibling page number in balance_nonroot"));
            }
            if old_pgno[i + 1..n_old].contains(&pgno) {
                return Err(corrupt("page is its own sibling in balance_nonroot"));
            }
            {
                let d = self.pager.read_page(pgno)?;
                st.bufs[i].copy_from_slice(d);
            }
            let (_, nc, nf) = page_state(&st.bufs[i], pgno, usable)?;
            old_pgno[i] = pgno;
            old_ncell[i] = nc;
            old_nfree[i] = nf;
            n_max_cells += nc + 4;
            if i == 0 {
                break;
            }
            i -= 1;
            if par_novfl > 0 && i + nx_div == usize::from(par_ovfl.idx[0]) {
                let o = par_ovfl.off[0] as usize;
                pgno = get4(&par_ovfl.space, o);
                let sz = cell_size(&pkind, usable as u32, &par_ovfl.space, o);
                if o + sz > par_ovfl.space.len() {
                    return Err(corrupt("parent overflow cell out of range"));
                }
                div[i] = (div_stash.len(), sz);
                div_stash.extend_from_slice(&par_ovfl.space[o..o + sz]);
                sz_new[i] = sz as i32;
                par_novfl = 0;
            } else {
                let ci = i + nx_div - par_novfl;
                if ci >= par_ncell {
                    return Err(corrupt("divider index out of range in balance_nonroot"));
                }
                let d = self.pager.write_page(parent)?;
                let off = get2(d, par_base + 2 * ci) & (page_size - 1);
                pgno = get4(d, off);
                let sz = cell_size(&pkind, usable as u32, d, off);
                if off + sz > usable {
                    return Err(corrupt("divider cell extends past page end"));
                }
                div[i] = (div_stash.len(), sz);
                div_stash.extend_from_slice(&d[off..off + sz]);
                sz_new[i] = sz as i32;
                drop_cell(
                    d,
                    par_hdr,
                    4,
                    usable,
                    &mut par_ncell,
                    &mut par_nfree,
                    ci,
                    sz,
                    secure,
                )?;
            }
        }
        if par_novfl != 0 {
            return Err(corrupt(
                "parent overflow cell is not a divider of the siblings",
            ));
        }
        let _ = n_max_cells;

        // ---- Load pointers to all cells of the siblings and the dividers into b.apCell[].
        st.kind = PgKind::decode(st.bufs[0][0], usable as u32)?;
        let kind = st.kind;
        let leaf_correction: usize = if kind.leaf { 4 } else { 0 };
        let leaf_data = kind.int_key_leaf;
        let nld = usize::from(!leaf_data);
        let cell_base = 8 + kind.child_ptr;
        let mask = page_size - 1;
        // Sibling slot of the page that carries overflow cells.
        let the_page = i_parent_idx.wrapping_sub(nx_div);
        for i in 0..n_old {
            if st.bufs[i][0] != st.bufs[0][0] {
                return Err(corrupt("sibling pages of different types"));
            }
            let n_cell_pg = old_ncell[i];
            st.pg_ncell[i] = n_cell_pg;
            let mut j = 0usize;
            if i == the_page && page_novfl > 0 {
                st.pg_novfl[i] = page_novfl;
                let limit = usize::from(st.ai_ovfl[0]);
                if n_cell_pg < limit {
                    return Err(corrupt("overflow cell index past the end of its page"));
                }
                while j < limit {
                    let off = get2(&st.bufs[i], cell_base + 2 * j) & mask;
                    st.ap.push(((i as u32) << OFF_BITS) | off as u32);
                    st.sz.push(0);
                    j += 1;
                }
                for k in 0..page_novfl {
                    st.ap.push((BUF_OVFL << OFF_BITS) | page_ovfl_off[k]);
                    st.sz.push(0);
                }
            }
            while j < n_cell_pg {
                let off = get2(&st.bufs[i], cell_base + 2 * j) & mask;
                st.ap.push(((i as u32) << OFF_BITS) | off as u32);
                st.sz.push(0);
                j += 1;
            }
            cnt_old[i] = st.ap.len() as i32;
            if i < n_old - 1 && !leaf_data {
                let (doff, dsz) = div[i];
                let start = st.space1.len();
                st.space1.extend_from_slice(&div_stash[doff..doff + dsz]);
                let mut szc = dsz - leaf_correction;
                if kind.leaf {
                    while szc < 4 {
                        // "Do not allow any cells smaller than 4 bytes."
                        st.space1.push(0);
                        szc += 1;
                    }
                } else {
                    // The right pointer of the child becomes the left pointer of the divider.
                    let rp = [st.bufs[i][8], st.bufs[i][9], st.bufs[i][10], st.bufs[i][11]];
                    st.space1[start..start + 4].copy_from_slice(&rp);
                }
                // Slack so that the size functions never read past the buffer.
                st.ap
                    .push((BUF_SPACE1 << OFF_BITS) | (start + leaf_correction) as u32);
                st.sz.push(szc as u16);
            }
        }
        let n_cell = st.ap.len() as i32;

        // ---- Figure out how many pages are needed and which cells go where.
        let usable_space = (usable - 12 + leaf_correction) as i32;
        for i in 0..n_old {
            sz_new[i] = usable_space - old_nfree[i];
            if i == the_page {
                for j in 0..page_novfl {
                    let s = cell_size(
                        &kind,
                        usable as u32,
                        &st.ovfl_space,
                        page_ovfl_off[j] as usize,
                    );
                    sz_new[i] += 2 + s as i32;
                }
            }
            cnt_new[i] = cnt_old[i];
        }
        let mut k = n_old;
        let mut i = 0usize;
        while i < k {
            while sz_new[i] > usable_space {
                if i + 1 >= k {
                    k = i + 2;
                    if k > NB + 2 {
                        return Err(corrupt("balance_nonroot needs more than five siblings"));
                    }
                    sz_new[k - 1] = 0;
                    cnt_new[k - 1] = n_cell;
                }
                if cnt_new[i] < 1 {
                    return Err(corrupt("sibling page without cells in balance_nonroot"));
                }
                let mut sz = 2 + st.cached_cell_size((cnt_new[i] - 1) as usize) as i32;
                sz_new[i] -= sz;
                if !leaf_data {
                    sz = if cnt_new[i] < n_cell {
                        2 + st.cached_cell_size(cnt_new[i] as usize) as i32
                    } else {
                        0
                    };
                }
                sz_new[i + 1] += sz;
                cnt_new[i] -= 1;
            }
            while cnt_new[i] < n_cell {
                let mut sz = 2 + st.cached_cell_size(cnt_new[i] as usize) as i32;
                if sz_new[i] + sz > usable_space {
                    break;
                }
                sz_new[i] += sz;
                cnt_new[i] += 1;
                if !leaf_data {
                    sz = if cnt_new[i] < n_cell {
                        2 + st.cached_cell_size(cnt_new[i] as usize) as i32
                    } else {
                        0
                    };
                }
                sz_new[i + 1] -= sz;
            }
            if cnt_new[i] >= n_cell {
                k = i + 1;
            } else if cnt_new[i] <= (if i > 0 { cnt_new[i - 1] } else { 0 }) {
                return Err(corrupt("empty sibling in balance_nonroot"));
            }
            i += 1;
        }

        // ---- The packing above is biased to the left; even it out from the right.
        let mut i = k - 1;
        while i > 0 {
            let mut sz_right = sz_new[i];
            let mut sz_left = sz_new[i - 1];
            let mut r = cnt_new[i - 1] - 1;
            let mut d = r + 1 - i32::from(leaf_data);
            if r < 0 || d < 0 || d >= n_cell {
                return Err(corrupt("cell index out of range in balance_nonroot"));
            }
            st.cached_cell_size(d as usize);
            loop {
                let sz_r = st.cached_cell_size(r as usize) as i32;
                let sz_d = i32::from(st.sz[d as usize]);
                if sz_right != 0
                    && (bulk
                        || sz_right + sz_d + 2 > sz_left - (sz_r + if i == k - 1 { 0 } else { 2 }))
                {
                    break;
                }
                sz_right += sz_d + 2;
                sz_left -= sz_r + 2;
                cnt_new[i - 1] = r;
                r -= 1;
                d -= 1;
                if r < 0 {
                    break;
                }
                if d < 0 {
                    return Err(corrupt("cell index out of range in balance_nonroot"));
                }
            }
            sz_new[i] = sz_right;
            sz_new[i - 1] = sz_left;
            if cnt_new[i - 1] <= (if i > 1 { cnt_new[i - 2] } else { 0 }) {
                return Err(corrupt("empty sibling in balance_nonroot"));
            }
            i -= 1;
        }

        if self.dtrace_persistent() {
            #[cfg(sqliters_dtrace)]
            {
                let old: Vec<String> = (0..n_old)
                    .map(|i| {
                        format!(
                            "{}:{}:{}",
                            old_pgno[i],
                            old_ncell[i] + if i == the_page { page_novfl } else { 0 },
                            old_nfree[i]
                        )
                    })
                    .collect();
                let cnt: Vec<String> = (0..k).map(|i| cnt_new[i].to_string()).collect();
                let szs: Vec<String> = (0..k).map(|i| sz_new[i].to_string()).collect();
                sqliters_types::dtrace!(
                    "TR|bal|sib|ctx={} parent={} idx={} nxdiv={} nold={} bulk={} old={}",
                    self.tr_ctx(),
                    parent,
                    i_parent_idx,
                    nx_div,
                    n_old,
                    b_bulk,
                    old.join(",")
                );
                sqliters_types::dtrace!(
                    "TR|bal|dist|ctx={} k={} leafdata={} ncell={} cnt={} sz={}",
                    self.tr_ctx(),
                    k,
                    u8::from(leaf_data),
                    n_cell,
                    cnt.join(","),
                    szs.join(",")
                );
            }
        }

        // ---- Allocate k new pages. Reuse old pages where possible.
        let page_flags = st.bufs[0][0];
        let mut new_pgno = [0 as Pgno; NNEW];
        let n_new = k;
        for i in 0..k {
            if i < n_old {
                new_pgno[i] = old_pgno[i];
                self.pager.write_page(old_pgno[i])?;
            } else {
                let near = if bulk { 1 } else { pgno };
                pgno = self.allocate_btree_page(AllocMode::Near(near))?;
                let mut b = self.bal_take_buf(page_size);
                b.copy_from_slice(self.pager.read_page(pgno)?);
                zero_page(&mut b, 0, page_flags, usable, secure);
                st.bufs.push(b);
                st.pg_ncell[i] = 0;
                new_pgno[i] = pgno;
                cnt_old[i] = n_cell;
                if auto_vac {
                    self.ptrmap_put(pgno, PtrmapType::BtreeNode, parent)?;
                }
            }
        }

        // ---- Reassign page numbers so that the new pages are in ascending order.
        let a_pgno = new_pgno;
        for i in 0..n_new.saturating_sub(1) {
            let mut i_b = i;
            for j in i + 1..n_new {
                if new_pgno[j] < new_pgno[i_b] {
                    i_b = j;
                }
            }
            if i_b != i {
                new_pgno.swap(i, i_b);
            }
        }
        if self.dtrace_persistent() {
            #[cfg(sqliters_dtrace)]
            {
                let np: Vec<String> = (0..n_new).map(|i| new_pgno[i].to_string()).collect();
                let fp: Vec<String> = (n_new..n_old).map(|i| old_pgno[i].to_string()).collect();
                sqliters_types::dtrace!(
                    "TR|bal|pages|ctx={} new={} freed={}",
                    self.tr_ctx(),
                    np.join(","),
                    fp.join(",")
                );
            }
        }

        {
            let d = self.pager.write_page(parent)?;
            put4(d, p_right, new_pgno[n_new - 1]);
        }

        // The right-child pointer of the right-most new sibling is the one the right-most old
        // sibling had.
        if (page_flags & 0x08) == 0 && n_old != n_new {
            let rp = [
                st.bufs[n_old - 1][8],
                st.bufs[n_old - 1][9],
                st.bufs[n_old - 1][10],
                st.bufs[n_old - 1][11],
            ];
            st.bufs[n_new - 1][8..12].copy_from_slice(&rp);
        }

        // ---- Pointer-map entries of the cells that change page.
        if auto_vac {
            let mut i_new = 0usize;
            let mut i_old = 0usize;
            let mut cnt_old_next = (st.pg_ncell[0] + st.pg_novfl[0]) as i32;
            let mut i = 0i32;
            while i < n_cell {
                while i == cnt_old_next {
                    i_old += 1;
                    if i_old >= n_new.max(n_old) {
                        return Err(corrupt("cell count mismatch in balance_nonroot"));
                    }
                    cnt_old_next += (st.pg_ncell[i_old] + st.pg_novfl[i_old] + nld) as i32;
                }
                if i == cnt_new[i_new] {
                    i_new += 1;
                    if !leaf_data {
                        i += 1;
                        continue;
                    }
                }
                let (cb, co) = st.cell_loc(i as usize);
                if i_old >= n_new || new_pgno[i_new] != a_pgno[i_old] || cb as usize != i_old {
                    if leaf_correction == 0 {
                        let child = get4(st.buf(cb), co);
                        self.ptrmap_put(child, PtrmapType::BtreeNode, new_pgno[i_new])?;
                    }
                    if st.cached_cell_size(i as usize) as u32 > kind.min_local {
                        self.ptrmap_put_ovfl_ptr(new_pgno[i_new], &kind, st.buf(cb), co)?;
                    }
                }
                i += 1;
            }
        }

        // ---- Insert new divider cells into the parent.
        let mut out = OvflCells::default();
        let mut cellbuf: Vec<u8> = Vec::new();
        for i in 0..n_new - 1 {
            let mut j = cnt_new[i] as usize;
            if j >= st.ap.len() {
                return Err(corrupt("divider index out of range in balance_nonroot"));
            }
            let (cb, co) = st.cell_loc(j);
            let szj = st.cached_cell_size(j);
            let mut sz = szj + leaf_correction;
            cellbuf.clear();
            if !kind.leaf {
                let src = st.buf(cb);
                let Some(cell) = src.get(co..co + sz) else {
                    return Err(corrupt("divider cell extends past its page"));
                };
                cellbuf.extend_from_slice(cell);
                let lp = [cellbuf[0], cellbuf[1], cellbuf[2], cellbuf[3]];
                st.bufs[i][8..12].copy_from_slice(&lp);
            } else if leaf_data {
                // The divider is the integer key of the right-most cell of the left sibling.
                j -= 1;
                let (cb, co) = st.cell_loc(j);
                let info = parse_cell(&kind, usable as u32, st.buf(cb), co)?;
                cellbuf.extend_from_slice(&[0, 0, 0, 0]);
                push_varint(&mut cellbuf, info.n_key as u64);
                sz = cellbuf.len();
            } else {
                // Index leaf cell promoted to a divider: it gains a child pointer. A cell whose
                // reported size is the 4-byte minimum may really be shorter: reparse it as a
                // cell of the parent.
                let src = st.buf(cb);
                if szj == 4 {
                    let mut it = co;
                    let mut n_size = u32::from(at(src, it));
                    if n_size >= 0x80 {
                        let end = it + 8;
                        n_size &= 0x7f;
                        loop {
                            it += 1;
                            n_size = (n_size << 7) | u32::from(at(src, it) & 0x7f);
                            if at(src, it) < 0x80 || it >= end {
                                break;
                            }
                        }
                    }
                    it += 1;
                    sz = if n_size <= pkind.max_local {
                        (n_size as usize + (it - co)) + 4
                    } else {
                        // (cannot happen for a 4-byte cell; keep C's arithmetic)
                        cell_size(&kind, usable as u32, src, co) + 4
                    };
                }
                let Some(cell) = src.get(co..co + sz - 4) else {
                    return Err(corrupt("divider cell extends past its page"));
                };
                cellbuf.extend_from_slice(&[0, 0, 0, 0]);
                cellbuf.extend_from_slice(cell);
            }
            let _fits = self.insert_cell(
                parent,
                &pkind,
                &mut par_ncell,
                &mut par_nfree,
                &mut out,
                nx_div + i,
                &cellbuf[..sz],
                Some(new_pgno[i]),
            )?;
            if self.dtrace_persistent() {
                #[cfg(sqliters_dtrace)]
                sqliters_types::dtrace!(
                    "TR|bal|div|ctx={} i={} left={} sz={} key={} o={}",
                    self.tr_ctx(),
                    i,
                    new_pgno[i],
                    sz,
                    tr_key(&cellbuf[..sz], kind.int_key),
                    if _fits { "fits" } else { "overflow" }
                );
            }
        }

        // ---- Now update the actual sibling pages.
        let mut ab_done = [false; NNEW];
        let mut new_nfree = [0i32; NNEW];
        let n_new_i = n_new as i32;
        for i in (1 - n_new_i)..n_new_i {
            let ipg = i.unsigned_abs() as usize;
            if ab_done[ipg] {
                continue;
            }
            if i >= 0 || cnt_old[ipg - 1] >= cnt_new[ipg - 1] {
                let (i_new, i_old, n_new_cell);
                if ipg == 0 {
                    i_new = 0;
                    i_old = 0;
                    n_new_cell = cnt_new[0];
                } else {
                    i_old = if ipg < n_old {
                        cnt_old[ipg - 1] + nld as i32
                    } else {
                        n_cell
                    };
                    i_new = cnt_new[ipg - 1] + nld as i32;
                    n_new_cell = cnt_new[ipg] - i_new;
                }
                if n_new_cell < 0 || i_new < 0 || i_old < 0 || i_new + n_new_cell > n_cell {
                    return Err(corrupt("cell range out of bounds in balance_nonroot"));
                }
                let _rebuilt =
                    st.edit_page(ipg, i_old as usize, i_new as usize, n_new_cell as usize)?;
                ab_done[ipg] = true;
                new_nfree[ipg] = usable_space - sz_new[ipg];
                if self.dtrace_persistent() {
                    sqliters_types::dtrace!(
                        "TR|bal|edit|ctx={} pg={} ncell={} nfree={} o={}",
                        self.tr_ctx(),
                        new_pgno[ipg],
                        n_new_cell,
                        new_nfree[ipg],
                        if _rebuilt { "rebuild" } else { "edit" }
                    );
                }
            }
        }

        if is_root && par_ncell == 0 && par_hdr as i32 <= new_nfree[0] {
            // ---- balance-shallower: the root has no cell left; its only child moves into it.
            if self.dtrace_persistent() {
                sqliters_types::dtrace!(
                    "TR|bal|shallow|ctx={} root={} child={} o=shallower",
                    self.tr_ctx(),
                    parent,
                    new_pgno[0]
                );
            }
            {
                let mut tmp = std::mem::take(&mut st.tmp);
                let r = defragment_page(
                    &mut st.bufs[0],
                    0,
                    &kind,
                    usable,
                    st.pg_ncell[0],
                    new_nfree[0],
                    -1,
                    &mut tmp,
                );
                st.tmp = tmp;
                r?;
            }
            // copyNodeContent(apNew[0], pParent)
            {
                let from = &st.bufs[0];
                let i_data = get2(from, 5);
                if i_data > usable {
                    return Err(corrupt("cell content offset past page end"));
                }
                let n_hdr = 8 + kind.child_ptr + 2 * st.pg_ncell[0];
                let d = self.pager.write_page(parent)?;
                d[i_data..usable].copy_from_slice(&from[i_data..usable]);
                if par_hdr + n_hdr > d.len() {
                    return Err(corrupt("cell pointer array past page end"));
                }
                d[par_hdr..par_hdr + n_hdr].copy_from_slice(&from[..n_hdr]);
            }
            {
                // `btreeInitPage(pTo)` + `btreeComputeFreeSpace(pTo)`.
                let d = self.pager.read_page(parent)?;
                page_state(d, parent, usable)?;
            }
            if auto_vac {
                let img = self.pager.read_page(parent)?.to_vec();
                self.set_child_ptrmaps_raw(parent, &img)?;
            }
            // freePage(apNew[0]) — the page keeps the content it had in C's cache.
            for i in 0..n_new {
                self.pager
                    .write_page(new_pgno[i])?
                    .copy_from_slice(&st.bufs[i]);
            }
            self.free_page(new_pgno[0])?;
        } else {
            if self.dtrace_persistent() {
                sqliters_types::dtrace!(
                    "TR|bal|shallow|ctx={} root={} child={} o={}",
                    self.tr_ctx(),
                    parent,
                    new_pgno[0],
                    if is_root && par_ncell == 0 {
                        "root-empty-no-room"
                    } else {
                        "no"
                    }
                );
            }
            for i in 0..n_new {
                self.pager
                    .write_page(new_pgno[i])?
                    .copy_from_slice(&st.bufs[i]);
            }
            if auto_vac && leaf_correction == 0 {
                // Fix the pointer map entries of the right-child of each sibling page.
                for i in 0..n_new {
                    let key = get4(&st.bufs[i], 8);
                    self.ptrmap_put(key, PtrmapType::BtreeNode, new_pgno[i])?;
                }
            }
        }

        // ---- Free any old pages that were not reused as new pages.
        for i in n_new..n_old {
            self.free_page(old_pgno[i])?;
        }
        Ok(out)
    }
}
