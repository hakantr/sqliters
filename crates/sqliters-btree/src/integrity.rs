//! B-tree level `PRAGMA integrity_check` (`sqlite3BtreeIntegrityCheck`, `btree.c`).
//!
//! A port of `checkTreePage` / `checkList` / `checkRef` / `checkPtrmap`: the same checks in
//! the same order, reporting with the message texts of C SQLite so that both engines flag the
//! same files with the same words.

use sqliters_types::{Pgno, Result, get_varint};

use crate::{BTCURSOR_MAX_DEPTH, Btree, PtrmapType, ptrmap_pageno};

/// Zero bytes appended to a page image so that parsing a cell that starts near the end of the
/// page never indexes out of bounds (a cell header is at most 4 + 9 + 9 bytes).
const PAGE_PAD: usize = 32;

/// Outcome of [`Btree::integrity_check_report`].
#[derive(Debug, Clone, Default)]
pub struct IntegrityReport {
    /// One entry per reported problem, in the order and with the texts of C's
    /// `sqlite3BtreeIntegrityCheck`. Empty when the file is healthy.
    pub messages: Vec<String>,
    /// For every entry of `roots`, the number of entries found in that tree: rows of a table
    /// tree, keys of an index tree (`aCnt[]` in C).
    pub row_counts: Vec<i64>,
    /// For every entry of `roots`, whether the tree holds a page that a cursor refuses to
    /// enter although it is well formed: a page other than the root without any cell, or an
    /// interior root without cells that is not page 1 (`moveToChild` / `moveToRoot` return
    /// `SQLITE_CORRUPT` there). The b-tree level check has no message for these; C fails when
    /// the row-level pass walks into such a page.
    pub unwalkable: Vec<bool>,
}

/// Message prefix in effect (`IntegrityCk.zPfx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prefix {
    None,
    /// `"Freelist: "`
    Freelist,
    /// `"Tree %u page %u: "`
    Page,
    /// `"Tree %u page %u cell %u: "`
    Cell,
    /// `"Tree %u page %u right child: "`
    RightChild,
}

/// The four page kinds as `decodeFlags` classifies them.
#[derive(Debug, Clone, Copy)]
struct PageKind {
    leaf: bool,
    int_key: bool,
    max_local: u32,
    min_local: u32,
}

#[derive(Debug, Clone, Copy)]
struct CellInfo {
    n_key: i64,
    n_payload: u32,
    n_local: u32,
    n_size: u32,
    /// Offset of the first payload byte in the page image.
    payload_off: usize,
}

struct IntegrityCk<'a> {
    bt: &'a mut Btree,
    /// Number of pages in the database (`nCkPage`).
    n_ck_page: Pgno,
    /// One bit per page: set once the page has been referenced (`aPgRef`).
    pg_ref: Vec<u8>,
    /// Errors that may still be reported (`mxErr`); the analysis stops at zero.
    mx_err: usize,
    messages: Vec<String>,
    pfx: Prefix,
    v0: u32,
    v1: u32,
    v2: u32,
    /// Number of pages above the one being checked (`nAbove`).
    n_above: usize,
    n_row: i64,
    /// Set when the tree being checked holds a page a cursor cannot enter.
    unwalkable: bool,
    usable: u32,
    page_size: u32,
    auto_vacuum: bool,
    /// The lock-byte page when references to it are to be named as such, else 0.
    named_lock_byte_page: Pgno,
    /// One page image per tree level, reused across pages.
    page_bufs: Vec<Vec<u8>>,
    /// Image of the freelist trunk page being walked.
    list_buf: Vec<u8>,
    /// Cell and freeblock extents `(start << 16) | end` for the coverage check (`heap`).
    spans: Vec<u32>,
}

#[inline]
fn get2(data: &[u8], off: usize) -> u32 {
    u32::from(u16::from_be_bytes([data[off], data[off + 1]]))
}

#[inline]
fn get4(data: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

/// The payload-size varint as the cell parsers of `btree.c` read it: at most nine bytes,
/// truncated to 32 bits. Returns `(value, offset_after)`.
#[inline]
fn payload_varint(data: &[u8], mut off: usize) -> (u32, usize) {
    let mut n = u32::from(data[off]);
    if n >= 0x80 {
        let end = off + 8;
        n &= 0x7f;
        loop {
            off += 1;
            n = (n << 7) | u32::from(data[off] & 0x7f);
            if data[off] < 0x80 || off >= end {
                break;
            }
        }
    }
    (n, off + 1)
}

impl PageKind {
    /// `decodeFlags`: `None` for a flag byte that is not one of the four page types.
    fn decode(flag: u8, usable: u32) -> Option<Self> {
        let max_leaf = usable - 35;
        let min_local = (usable - 12) * 32 / 255 - 23;
        let max_local = (usable - 12) * 64 / 255 - 23;
        match flag {
            0x0d => Some(Self {
                leaf: true,
                int_key: true,
                max_local: max_leaf,
                min_local,
            }),
            0x0a => Some(Self {
                leaf: true,
                int_key: false,
                max_local,
                min_local,
            }),
            0x02 => Some(Self {
                leaf: false,
                int_key: false,
                max_local,
                min_local,
            }),
            0x05 => Some(Self {
                leaf: false,
                int_key: true,
                max_local: max_leaf,
                min_local,
            }),
            _ => None,
        }
    }

    /// `xParseCell` for the cell at `pc` of the padded page image `data`.
    fn parse_cell(self, data: &[u8], pc: usize, usable: u32) -> CellInfo {
        if self.int_key && !self.leaf {
            // btreeParseCellPtrNoPayload
            let (key, n) = get_varint(&data[pc + 4..]).unwrap_or((0, 9));
            return CellInfo {
                n_key: key as i64,
                n_payload: 0,
                n_local: 0,
                n_size: 4 + n as u32,
                payload_off: pc,
            };
        }
        let mut off = pc + if self.leaf { 0 } else { 4 };
        let (n_payload, after) = payload_varint(data, off);
        off = after;
        let n_key = if self.int_key {
            if data[off] < 0x80 {
                off += 1;
                i64::from(data[off - 1])
            } else {
                let (key, n) = get_varint(&data[off..]).unwrap_or((0, 9));
                off += n;
                key as i64
            }
        } else {
            i64::from(n_payload)
        };
        let hdr_len = (off - pc) as u32;
        if n_payload <= self.max_local {
            CellInfo {
                n_key,
                n_payload,
                n_local: n_payload,
                n_size: (n_payload + hdr_len).max(4),
                payload_off: off,
            }
        } else {
            // btreeParseCellAdjustSizeForOverflow
            let surplus = self.min_local + (n_payload - self.min_local) % (usable - 4);
            let n_local = if surplus <= self.max_local {
                surplus
            } else {
                self.min_local
            };
            CellInfo {
                n_key,
                n_payload,
                n_local,
                n_size: hdr_len + n_local + 4,
                payload_off: off,
            }
        }
    }
}

/// `btreeComputeFreeSpace`: `false` when the freeblock chain or the free-space totals of the
/// page are not well formed.
fn free_space_is_sane(data: &[u8], hdr: usize, kind: PageKind, n_cell: u32, usable: u32) -> bool {
    let top = match get2(data, hdr + 5) {
        0 => 65_536,
        t => t,
    };
    let child_ptr = if kind.leaf { 0 } else { 4 };
    let cell_first = hdr as u32 + 8 + child_ptr + 2 * n_cell;
    let cell_last = usable - 4;
    let mut pc = get2(data, hdr + 1);
    let mut n_free = u32::from(data[hdr + 7]) + top;
    if pc > 0 {
        if pc < top {
            // There is always at least one cell before the first freeblock.
            return false;
        }
        let (next, size) = loop {
            if pc > cell_last {
                // Freeblock off the end of the page.
                return false;
            }
            let next = get2(data, pc as usize);
            let size = get2(data, pc as usize + 2);
            if size < 4 {
                return false;
            }
            n_free += size;
            if next < pc + size + 4 {
                break (next, size);
            }
            pc = next;
        };
        if next > 0 {
            // Freeblocks not in ascending order.
            return false;
        }
        if pc + size > usable {
            // Last freeblock extends past the end of the page.
            return false;
        }
    }
    n_free <= usable && n_free >= cell_first
}

impl IntegrityCk<'_> {
    /// `checkAppendMsg`.
    fn append_msg(&mut self, text: std::fmt::Arguments<'_>) {
        if self.mx_err == 0 {
            return;
        }
        self.mx_err -= 1;
        let msg = match self.pfx {
            Prefix::None => format!("{text}"),
            Prefix::Freelist => format!("Freelist: {text}"),
            Prefix::Page => format!("Tree {} page {}: {text}", self.v0, self.v1),
            Prefix::Cell => format!("Tree {} page {} cell {}: {text}", self.v0, self.v1, self.v2),
            Prefix::RightChild => {
                format!("Tree {} page {} right child: {text}", self.v0, self.v1)
            }
        };
        self.messages.push(msg);
    }

    fn is_referenced(&self, pg: Pgno) -> bool {
        self.pg_ref[(pg / 8) as usize] & (1 << (pg & 7)) != 0
    }

    fn set_referenced(&mut self, pg: Pgno) {
        self.pg_ref[(pg / 8) as usize] |= 1 << (pg & 7);
    }

    /// `checkRef`: records a reference to `pg`. Returns `true` (after reporting) when the page
    /// number is out of range or the page was referenced before.
    fn check_ref(&mut self, pg: Pgno) -> bool {
        if pg > self.n_ck_page || pg == 0 {
            self.append_msg(format_args!("invalid page number {pg}"));
            return true;
        }
        if self.is_referenced(pg) {
            if pg == self.named_lock_byte_page {
                self.append_msg(format_args!(
                    "2nd reference to page {pg} (lock-byte page {pg})"
                ));
            } else {
                self.append_msg(format_args!("2nd reference to page {pg}"));
            }
            return true;
        }
        self.set_referenced(pg);
        false
    }

    /// `checkPtrmap`: the pointer-map entry of `child` must be `(e_type, parent)`.
    fn check_ptrmap(&mut self, child: Pgno, e_type: PtrmapType, parent: Pgno) {
        match self.bt.ptrmap_get(child) {
            Ok((got_type, got_parent)) => {
                if got_type != e_type || got_parent != parent {
                    self.append_msg(format_args!(
                        "Bad ptr map entry key={child} expected=({},{parent}) got=({},{got_parent})",
                        e_type as u8, got_type as u8
                    ));
                }
            }
            Err(_) => self.append_msg(format_args!("Failed to read ptrmap key={child}")),
        }
    }

    /// `checkList`: walks the freelist (`is_free_list`) or an overflow chain starting at
    /// `i_page` and verifies that it holds `n` pages.
    fn check_list(&mut self, is_free_list: bool, mut i_page: Pgno, mut n: u32) -> Result<()> {
        let expected = n;
        let n_msg_at_start = self.messages.len();
        let usable = self.usable as usize;
        while i_page != 0 && self.mx_err > 0 {
            self.bt.check_interrupted()?;
            if self.check_ref(i_page) {
                break;
            }
            n = n.wrapping_sub(1);
            let next = if is_free_list {
                let mut buf = std::mem::take(&mut self.list_buf);
                buf.clear();
                match self.bt.pager.read_page(i_page) {
                    Ok(raw) => buf.extend_from_slice(raw),
                    Err(_) => {
                        self.list_buf = buf;
                        self.append_msg(format_args!("failed to get page {i_page}"));
                        break;
                    }
                }
                let n_leaf = get4(&buf, 4);
                if self.auto_vacuum {
                    self.check_ptrmap(i_page, PtrmapType::FreePage, 0);
                }
                if n_leaf > self.usable / 4 - 2 {
                    self.append_msg(format_args!("freelist leaf count too big on page {i_page}"));
                    n = n.wrapping_sub(1);
                } else {
                    for i in 0..n_leaf as usize {
                        debug_assert!(8 + 4 * i + 4 <= usable);
                        let free_page = get4(&buf, 8 + 4 * i);
                        if self.auto_vacuum {
                            self.check_ptrmap(free_page, PtrmapType::FreePage, 0);
                        }
                        self.check_ref(free_page);
                    }
                    n = n.wrapping_sub(n_leaf);
                }
                let next = get4(&buf, 0);
                self.list_buf = buf;
                next
            } else {
                let next = match self.bt.pager.read_page(i_page) {
                    Ok(raw) => get4(raw, 0),
                    Err(_) => {
                        self.append_msg(format_args!("failed to get page {i_page}"));
                        break;
                    }
                };
                // The pointer-map entry of the following overflow page must name this one.
                if self.auto_vacuum && n > 0 {
                    self.check_ptrmap(next, PtrmapType::Overflow2, i_page);
                }
                next
            };
            i_page = next;
        }
        if n != 0 && n_msg_at_start == self.messages.len() {
            self.append_msg(format_args!(
                "{} is {} but should be {expected}",
                if is_free_list {
                    "size"
                } else {
                    "overflow list length"
                },
                expected.wrapping_sub(n)
            ));
        }
        Ok(())
    }

    /// `checkTreePage`: checks page `i_page` and everything below it. Returns the height of
    /// the subtree (a leaf is 1; 0 when the page was not analysed) and, when the page was
    /// walked, the smallest integer key found in it (`*piMinKey`).
    ///
    /// The checks:
    ///  1. cells and freeblocks do not overlap and together cover the page,
    ///  2. integer keys are in order,
    ///  3. overflow chains are intact,
    ///  4. every child passes the same checks,
    ///  5. all children have the same depth.
    fn check_tree_page(&mut self, i_page: Pgno, max_key: i64) -> Result<(i32, Option<i64>)> {
        self.bt.check_interrupted()?;
        if self.mx_err == 0 || i_page == 0 {
            return Ok((0, None));
        }
        if self.check_ref(i_page) {
            return Ok((0, None));
        }
        let saved = (self.pfx, self.v1, self.v2);
        self.pfx = Prefix::Page;
        self.v1 = i_page;
        self.n_above += 1;

        let level = self.n_above - 1;
        if self.page_bufs.len() <= level {
            self.page_bufs.resize_with(level + 1, Vec::new);
        }
        let mut data = std::mem::take(&mut self.page_bufs[level]);
        let outcome = self.check_tree_page_body(i_page, max_key, &mut data);
        self.page_bufs[level] = data;

        self.pfx = saved.0;
        self.v1 = saved.1;
        self.v2 = saved.2;
        self.n_above -= 1;
        let (depth, min_key) = outcome?;
        Ok((depth + 1, min_key))
    }

    /// Body of [`Self::check_tree_page`] between the prefix save and restore; returns `depth`
    /// (-1 for a leaf or a page that could not be analysed) and the minimum key.
    fn check_tree_page_body(
        &mut self,
        i_page: Pgno,
        mut max_key: i64,
        data: &mut Vec<u8>,
    ) -> Result<(i32, Option<i64>)> {
        let usable = self.usable;
        let mut depth: i32 = -1;

        data.clear();
        match self.bt.pager.read_page(i_page) {
            Ok(raw) => {
                data.extend_from_slice(raw);
                data.resize(raw.len().max(usable as usize) + PAGE_PAD, 0);
            }
            Err(e) => {
                self.append_msg(format_args!(
                    "unable to get the page. error code={}",
                    e.extended_code
                ));
                return Ok((depth, None));
            }
        }
        if self.n_above > BTCURSOR_MAX_DEPTH {
            self.append_msg(format_args!("btree depth exceeds {BTCURSOR_MAX_DEPTH}"));
            return Ok((depth, None));
        }

        // btreeInitPage
        let hdr: usize = if i_page == 1 { 100 } else { 0 };
        let n_cell = get2(data, hdr + 3);
        let kind = match PageKind::decode(data[hdr], usable) {
            Some(kind) if n_cell <= (self.page_size - 8) / 6 => kind,
            _ => {
                self.append_msg(format_args!("btreeInitPage() returns error code 11"));
                return Ok((depth, None));
            }
        };
        if !free_space_is_sane(data, hdr, kind, n_cell, usable) {
            self.append_msg(format_args!("free space corruption"));
            return Ok((depth, None));
        }

        if n_cell == 0 && (self.n_above > 1 || (!kind.leaf && i_page != 1)) {
            self.unwalkable = true;
        }

        // Set up for cell analysis.
        self.pfx = Prefix::Cell;
        let content_offset = match get2(data, hdr + 5) {
            0 => 65_536,
            t => t,
        };
        if kind.leaf || !kind.int_key {
            self.n_row += i64::from(n_cell);
        }
        let cell_start = hdr + if kind.leaf { 8 } else { 12 };
        let mut do_coverage_check = true;
        // Only the first key checked on the page may equal `max_key`.
        let mut key_can_be_equal = true;

        if kind.leaf {
            self.spans.clear();
        } else {
            // Analyse the right-child page of an interior page.
            let pgno = get4(data, hdr + 8);
            if self.auto_vacuum {
                self.pfx = Prefix::RightChild;
                self.check_ptrmap(pgno, PtrmapType::BtreeNode, i_page);
            }
            let (d, min_key) = self.check_tree_page(pgno, max_key)?;
            depth = d;
            if let Some(k) = min_key {
                max_key = k;
            }
            key_can_be_equal = false;
        }

        for i in (0..n_cell as usize).rev() {
            if self.mx_err == 0 {
                break;
            }
            // Check the cell size.
            self.v2 = i as u32;
            let pc = get2(data, cell_start + 2 * i);
            if pc < content_offset || pc > usable - 4 {
                self.append_msg(format_args!(
                    "Offset {pc} out of range {content_offset}..{}",
                    usable - 4
                ));
                do_coverage_check = false;
                continue;
            }
            let info = kind.parse_cell(data, pc as usize, usable);
            if pc + info.n_size > usable {
                self.append_msg(format_args!("Extends off end of page"));
                do_coverage_check = false;
                continue;
            }
            if info.n_payload != 0 && data[info.payload_off] < 2 {
                self.append_msg(format_args!("Bad cell header size"));
                do_coverage_check = false;
                continue;
            }

            // Check for an integer primary key out of range.
            if kind.int_key {
                let out_of_order = if key_can_be_equal {
                    info.n_key > max_key
                } else {
                    info.n_key >= max_key
                };
                if out_of_order {
                    self.append_msg(format_args!("Rowid {} out of order", info.n_key));
                }
                max_key = info.n_key;
                key_can_be_equal = false;
            }

            // Check the content overflow list.
            if info.n_payload > info.n_local {
                let n_page = (info.n_payload - info.n_local + usable - 5) / (usable - 4);
                let pgno_ovfl = get4(data, (pc + info.n_size - 4) as usize);
                if self.auto_vacuum {
                    self.check_ptrmap(pgno_ovfl, PtrmapType::Overflow1, i_page);
                }
                self.check_list(false, pgno_ovfl, n_page)?;
            }

            if kind.leaf {
                // Populate the coverage-checking list for leaf pages.
                self.spans.push((pc << 16) | (pc + info.n_size - 1));
            } else {
                // Check the sanity of the left child page of an interior cell.
                let pgno = get4(data, pc as usize);
                if self.auto_vacuum {
                    self.check_ptrmap(pgno, PtrmapType::BtreeNode, i_page);
                }
                let (d2, min_key) = self.check_tree_page(pgno, max_key)?;
                if let Some(k) = min_key {
                    max_key = k;
                }
                key_can_be_equal = false;
                if d2 != depth {
                    self.append_msg(format_args!("Child page depth differs"));
                    depth = d2;
                }
            }
        }
        let min_key = Some(max_key);

        // Check for complete coverage of the page.
        self.pfx = Prefix::None;
        if do_coverage_check && self.mx_err > 0 {
            // The children of an interior page reused the list: collect its cells now.
            if !kind.leaf {
                self.spans.clear();
                for i in (0..n_cell as usize).rev() {
                    let pc = get2(data, cell_start + 2 * i);
                    let size = kind.parse_cell(data, pc as usize, usable).n_size;
                    self.spans.push((pc << 16) | (pc + size - 1));
                }
            }
            // Add the freeblocks. `free_space_is_sane` proved the chain ascending and in range.
            let mut i = get2(data, hdr + 1);
            while i > 0 {
                let size = get2(data, i as usize + 2);
                self.spans.push((i << 16) | (i + size - 1));
                i = get2(data, i as usize);
            }
            // Walk the extents in address order: an overlap means bytes are used twice, a gap
            // is fragmentation. The implied first extent is the header, the cell pointer array
            // and the unallocated space up to the cell content area.
            self.spans.sort_unstable();
            let mut n_frag: u32 = 0;
            let mut prev: u32 = content_offset - 1;
            let mut overlap_at = None;
            // C pulls an extent off its heap before testing it, so an overlap found on the very
            // last extent still leaves the heap empty and the fragmentation test runs too.
            let mut heap_empty = true;
            for (j, &x) in self.spans.iter().enumerate() {
                if (prev & 0xffff) >= (x >> 16) {
                    overlap_at = Some(x >> 16);
                    heap_empty = j + 1 == self.spans.len();
                    break;
                }
                n_frag += (x >> 16) - (prev & 0xffff) - 1;
                prev = x;
            }
            if let Some(byte) = overlap_at {
                self.append_msg(format_args!(
                    "Multiple uses for byte {byte} of page {i_page}"
                ));
            }
            n_frag += usable - (prev & 0xffff) - 1;
            // The total of all fragments is stored in the header byte at offset 7.
            let reported = u32::from(data[hdr + 7]);
            if heap_empty && n_frag != reported {
                self.append_msg(format_args!(
                    "Fragmentation of {n_frag} bytes reported as {reported} on page {i_page}"
                ));
            }
        }
        Ok((depth, min_key))
    }
}

impl Btree {
    /// Verifies the structure of the database file (`sqlite3BtreeIntegrityCheck`): the
    /// freelist, every B-tree in `roots` with its overflow chains, and that each page is used
    /// exactly once. Returns one message per problem, in C SQLite's wording; an empty `Vec`
    /// means the file is healthy.
    ///
    /// One addition to C's texts: the lock-byte page counts as used from the start, so C
    /// reports a reference to it as a plain `2nd reference to page N`; this entry point appends
    /// ` (lock-byte page N)` to say why. [`Self::integrity_check_report`], which serves
    /// `PRAGMA integrity_check`, prints exactly what C prints.
    pub fn integrity_check(&mut self, roots: &[Pgno]) -> Result<Vec<String>> {
        Ok(self
            .integrity_check_impl(roots, false, usize::MAX, true)?
            .messages)
    }

    /// [`Self::integrity_check`] with the controls of the C routine.
    ///
    /// `partial` says that `roots` does not list every B-tree of the file (a check of a single
    /// table): the freelist is then only walked when `roots` starts with page 1, and the
    /// "every page is used" pass and the auto-vacuum root checks are skipped. At most `mx_err`
    /// problems are reported; the analysis stops there. A root of `0` is skipped.
    pub fn integrity_check_report(
        &mut self,
        roots: &[Pgno],
        partial: bool,
        mx_err: usize,
    ) -> Result<IntegrityReport> {
        self.integrity_check_impl(roots, partial, mx_err, false)
    }

    fn integrity_check_impl(
        &mut self,
        roots: &[Pgno],
        partial: bool,
        mx_err: usize,
        name_lock_byte_page: bool,
    ) -> Result<IntegrityReport> {
        let n_ck_page = self.pager.page_count();
        let mut row_counts = vec![0i64; roots.len()];
        if n_ck_page == 0 {
            return Ok(IntegrityReport {
                messages: Vec::new(),
                row_counts,
                unwalkable: vec![false; roots.len()],
            });
        }

        let ck_freelist = !partial || roots.first() == Some(&1);
        let usable = self.pager.usable_size();
        let page_size = self.pager.page_size();
        let lock_byte_pgno = self.lock_byte_page();
        let (first_trunk, n_free, hdr_max_root, hdr_incr) = {
            let h = self.pager.header();
            (
                h.first_freelist_trunk_page,
                h.total_freelist_pages,
                h.largest_root_btree_page,
                h.incremental_vacuum_mode,
            )
        };
        let auto_vacuum = hdr_max_root > 0;

        let mut ck = IntegrityCk {
            bt: self,
            n_ck_page,
            pg_ref: vec![0u8; (n_ck_page / 8) as usize + 1],
            mx_err,
            messages: Vec::new(),
            pfx: Prefix::None,
            v0: 0,
            v1: 0,
            v2: 0,
            n_above: 0,
            n_row: 0,
            unwalkable: false,
            usable,
            page_size,
            auto_vacuum,
            named_lock_byte_page: if name_lock_byte_page {
                lock_byte_pgno
            } else {
                0
            },
            page_bufs: Vec::new(),
            list_buf: Vec::new(),
            spans: Vec::new(),
        };

        if lock_byte_pgno <= n_ck_page {
            ck.set_referenced(lock_byte_pgno);
        }

        // Check the integrity of the freelist.
        if ck_freelist {
            ck.pfx = Prefix::Freelist;
            ck.check_list(true, first_trunk, n_free)?;
            ck.pfx = Prefix::None;
        }

        // Check all the trees.
        if !partial {
            if auto_vacuum {
                let mx = roots.iter().copied().max().unwrap_or(0);
                if mx != hdr_max_root {
                    ck.append_msg(format_args!(
                        "max rootpage ({mx}) disagrees with header ({hdr_max_root})"
                    ));
                }
            } else if hdr_incr != 0 {
                ck.append_msg(format_args!(
                    "incremental_vacuum enabled with a max rootpage of zero"
                ));
            }
        }
        let mut unwalkable = vec![false; roots.len()];
        for (i, &root) in roots.iter().enumerate() {
            if ck.mx_err == 0 {
                break;
            }
            ck.n_row = 0;
            ck.unwalkable = false;
            if root != 0 {
                if auto_vacuum && root > 1 && !partial {
                    ck.check_ptrmap(root, PtrmapType::RootPage, 0);
                }
                ck.v0 = root;
                ck.check_tree_page(root, i64::MAX)?;
            }
            row_counts[i] = ck.n_row;
            unwalkable[i] = ck.unwalkable;
        }

        // Make sure every page in the file is referenced.
        if !partial {
            for pg in 1..=n_ck_page {
                if ck.mx_err == 0 {
                    break;
                }
                let is_ptrmap = auto_vacuum && ptrmap_pageno(pg, usable, lock_byte_pgno) == pg;
                let referenced = ck.is_referenced(pg);
                if !referenced && !is_ptrmap {
                    ck.append_msg(format_args!("Page {pg}: never used"));
                }
                // A database with auto-vacuum must not reference pointer-map pages.
                if referenced && is_ptrmap {
                    ck.append_msg(format_args!("Page {pg}: pointer map referenced"));
                }
            }
        }

        Ok(IntegrityReport {
            messages: ck.messages,
            row_counts,
            unwalkable,
        })
    }
}
