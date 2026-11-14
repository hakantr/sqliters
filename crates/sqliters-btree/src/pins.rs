//! C's page reference timeline (`sqlite3PagerGet` / `sqlite3PagerUnref` as `btree.c` calls them).
//!
//! The page cache decides what to write out early and what to throw away from which pages are
//! referenced and from the order in which references were given back (`pcache.c`,
//! `pcache1.c`). This b-tree layer addresses pages by number and holds no page objects, so
//! the references C holds are kept here explicitly:
//!
//! * a [`VCursor`] is the page stack of one C cursor (`BtCursor.apPage[]`, `iPage`): the path
//!   from the root to the page the cursor is on. `moveToRoot` keeps the root and gives the
//!   other pages back (the deepest first), `moveToChild` takes a reference on the child,
//!   `moveToParent` gives the page back, closing the cursor gives every page back from the
//!   root down (`btreeReleaseAllCursorPages`);
//! * the cursors of a statement are closed when the statement ends, in the order they were
//!   opened (`closeAllCursors`: cursor numbers ascending);
//! * references C takes for the duration of one operation (the siblings and new pages of
//!   `balance_nonroot`, free-list trunks, overflow pages, pointer-map pages) are taken and
//!   given back where C does it, through [`Btree::pin`] / [`Btree::unpin`].

use sqliters_types::{Pgno, Result};

use crate::Btree;

/// `BTCURSOR_MAX_DEPTH`.
const VC_MAX_DEPTH: usize = 20;

/// The page stack of one C cursor.
#[derive(Clone, Debug)]
pub(crate) struct VCursor {
    /// Root page of the b-tree the cursor is open on (0 = slot not in use).
    pub(crate) root: Pgno,
    /// Distinguishes several C cursors of one statement on the same b-tree.
    pub(crate) role: u8,
    /// `apPage[0..=iPage]`.
    pages: [Pgno; VC_MAX_DEPTH],
    /// `aiIdx[]`: the child slot taken on each interior page of the stack (kept by the
    /// seeks whose position a following write reuses).
    ai: [u16; VC_MAX_DEPTH],
    /// `iPage + 1` (0: the cursor holds no page, `iPage == -1`).
    n: u8,
    /// [`sqliters_pager::Pager::pcache_epoch`] when the references were taken.
    epoch: u32,
    /// `eState == CURSOR_VALID` with the cursor on cell `ix` of the page it is on — known
    /// only after an insert that did not have to balance (the cases in which
    /// `sqlite3BtreeIndexMoveto` may start from where the cursor is).
    pub(crate) valid: bool,
    pub(crate) ix: u16,
    /// `BTCF_AtLast`: the cursor is on the last entry of the table (`sqlite3BtreeLast`), which
    /// stays true while rows are appended through it without a `balance()`.
    pub(crate) at_last: bool,
    /// `eState == CURSOR_SKIPNEXT` with this `skipNext` (0: not in that state). The cursor
    /// holds its pages and `ix`, but is not `valid`.
    pub(crate) skip: i8,
    /// `eState == CURSOR_REQUIRESSEEK` of a table cursor: the rowid `saveCursorKey` kept. The
    /// cursor holds no page; the next movement seeks this rowid first.
    pub(crate) seek_rowid: Option<i64>,
    /// `BTCF_ValidNKey` with `info.nKey`: the rowid of the entry a table cursor is on is known
    /// (`getCellInfo` ran: `OP_Rowid`, `OP_Column`, an exact `sqlite3BtreeTableMoveto`).
    pub(crate) nkey: Option<i64>,
    /// `eState == CURSOR_REQUIRESSEEK` of an index cursor: the key `saveCursorKey` kept.
    pub(crate) seek_key: Option<(Vec<u8>, Vec<sqliters_types::KeySortOrder>)>,
    /// The cursor is on an index b-tree (C numbers the cursors of the tables of a statement
    /// before the cursors of their indexes, and closes cursors by number).
    pub(crate) is_index: bool,
}

/// Role bit of a cursor whose movements are not C's: the engine is reading rows ahead of the
/// point at which C's program visits them (the scan of a DELETE / UPDATE is replayed later,
/// see `cscan.rs` of the core). Such a cursor moves over the pages without referencing them.
pub(crate) const VC_SILENT: u8 = 0x80;

/// Role of the cursor a statement writes a b-tree through (`OP_OpenWrite`).
pub const VC_WRITE: u8 = 0;
/// Role of a cursor that only reads (`OP_OpenRead` of a scan or a lookup).
pub const VC_READ: u8 = 1;
/// A second read cursor on the same b-tree (constraint checks, foreign keys, sub-queries).
pub const VC_READ2: u8 = 2;

impl Btree {
    /// Takes a reference on page `pgno` (`sqlite3PagerGet` / `getAndInitPage`).
    #[inline]
    pub(crate) fn pin(&mut self, pgno: Pgno) -> Result<()> {
        self.pager.page_ref(pgno)
    }

    /// Gives a reference on page `pgno` back (`releasePage`).
    #[inline]
    pub(crate) fn unpin(&mut self, pgno: Pgno) {
        if pgno != 0 {
            self.pager.page_unref(pgno);
        }
    }

    /// The cursor of the running statement on b-tree `root` with `role`, opened if the
    /// statement has none yet (C: `OP_OpenRead` / `OP_OpenWrite`; no page is fetched).
    pub fn vc_open(&mut self, root: Pgno, role: u8) -> usize {
        let base = self.vc_frames.last().copied().unwrap_or(0);
        for (i, c) in self.vcursors.iter().enumerate().skip(base) {
            if c.root == root && c.role == role {
                return i;
            }
        }
        self.vcursors.push(VCursor {
            root,
            role,
            pages: [0; VC_MAX_DEPTH],
            ai: [0; VC_MAX_DEPTH],
            n: 0,
            epoch: self.pager.pcache_epoch(),
            valid: false,
            ix: 0,
            at_last: false,
            skip: 0,
            seek_rowid: None,
            nkey: None,
            seek_key: None,
            is_index: false,
        });
        self.vcursors.len() - 1
    }

    /// The cursor of a read function of this layer on `root`: a scan that is still delivering
    /// rows of the same b-tree keeps its own cursor (a second cursor number in C). While the
    /// engine reads ahead of C's program the cursor is a silent one.
    pub(crate) fn vc_open_read(&mut self, root: Pgno) -> usize {
        let mut role = VC_READ + self.read_scans.iter().filter(|&&r| r == root).count() as u8;
        if self.c_silent > 0 {
            role |= VC_SILENT;
        }
        self.vc_open(root, role)
    }

    /// A nested statement starts (a trigger program, a statement a virtual table runs on its
    /// shadow tables): its cursors are its own and are closed when it ends.
    pub fn vc_frame_push(&mut self) {
        self.vc_frames.push(self.vcursors.len());
        // (the nested program starts in step with C: what the enclosing statement set up for
        // its own loop does not apply to it)
        self.frame_saved.push((self.c_silent, self.c_save_position, self.read_scans.len()));
        self.c_silent = 0;
        self.c_save_position = None;
    }

    /// The nested statement ends: `closeAllCursors` for its frame.
    pub fn vc_frame_pop(&mut self) {
        let base = self.vc_frames.pop().unwrap_or(0);
        self.vc_close_from(base);
        if let Some((silent, save_pos, n_scans)) = self.frame_saved.pop() {
            self.c_silent = silent;
            self.c_save_position = save_pos;
            self.read_scans.truncate(n_scans);
        }
    }

    /// The statement ends (`sqlite3VdbeHalt` → `closeAllCursors`): every cursor gives its
    /// pages back, cursors in the order they were opened, each from the root down.
    pub fn vc_close_all(&mut self) {
        self.c_silent = 0;
        self.c_save_position = None;
        self.c_defer_idx = false;
        self.c_idx_search_quiet = false;
        // (what a statement knew about a rowid it had just sought or produced ends with it:
        // a statement that fails after its `OP_NotExists` must not spare the next one its own)
        self.new_rowid_hint = None;
        if !self.idx_scans.is_empty() {
            self.idx_scans.clear();
        }
        if !self.idx_seek_hint.is_empty() {
            self.idx_seek_hint.clear();
        }
        if !self.read_scans.is_empty() {
            self.read_scans.clear();
        }
        if !self.vcursors.is_empty() {
            self.vc_frames.clear();
            self.frame_saved.clear();
            self.vc_close_from(0);
        }
        // (the index order is used by the close above)
        if !self.idx_order.is_empty() {
            self.idx_order.clear();
        }
    }

    fn vc_close_from(&mut self, base: usize) {
        // closeAllCursors: by cursor number — the table cursors of the statement, then the
        // cursors of their indexes in the order of the table's index list
        // (`sqlite3OpenTableAndIndices`: `iIdxCur + i` for the i-th index of `pTab->pIndex`;
        // the PRIMARY KEY b-tree of a WITHOUT ROWID table is the last index of its table).
        for i in base..self.vcursors.len() {
            if !self.vcursors[i].is_index {
                self.vc_release_all(i);
            }
        }
        let mut idx: Vec<(usize, usize)> = (base..self.vcursors.len())
            .filter(|&i| self.vcursors[i].is_index)
            .map(|i| {
                let root = self.vcursors[i].root;
                let rank = match self.idx_order.iter().position(|&r| r == root) {
                    Some(p) => p,
                    None if self.wr_roots.contains(&root) => usize::MAX,
                    None => usize::MAX / 2,
                };
                (rank, i)
            })
            .collect();
        idx.sort();
        for (_, i) in idx {
            self.vc_release_all(i);
        }
        self.vcursors.truncate(base);
    }

    /// Closes one cursor before the statement ends (`OP_Close`).
    pub fn vc_close(&mut self, root: Pgno, role: u8) {
        let base = self.vc_frames.last().copied().unwrap_or(0);
        if let Some(i) = (base..self.vcursors.len())
            .find(|&i| self.vcursors[i].root == root && self.vcursors[i].role == role)
        {
            self.vc_release_all(i);
            self.vcursors[i].root = 0;
            if i + 1 == self.vcursors.len() {
                self.vcursors.pop();
            }
        }
    }

    /// Closes every cursor of the running statement on `root` that is not its write cursor.
    pub(crate) fn vc_close_reads(&mut self, root: Pgno) {
        let base = self.vc_frames.last().copied().unwrap_or(0);
        for i in base..self.vcursors.len() {
            if self.vcursors[i].root == root && self.vcursors[i].role != VC_WRITE {
                self.vc_release_all(i);
                self.vcursors[i].root = 0;
            }
        }
        while self.vcursors.len() > base && self.vcursors.last().is_some_and(|c| c.root == 0) {
            self.vcursors.pop();
        }
    }

    /// The references of cursor `vc` are still the ones the cache knows about (it has not
    /// been emptied as a whole since they were taken).
    #[inline]
    fn vc_live(&mut self, vc: usize) -> bool {
        let e = self.pager.pcache_epoch();
        let c = &mut self.vcursors[vc];
        if c.epoch != e {
            c.epoch = e;
            c.n = 0;
            c.valid = false;
            c.at_last = false;
            c.skip = 0;
            c.nkey = None;
            return false;
        }
        true
    }

    /// `btreeReleaseAllCursorPages`: "for(i=0; i<pCur->iPage; i++) releasePageNotNull(apPage[i]);
    /// releasePageNotNull(pCur->pPage); pCur->iPage = -1".
    pub(crate) fn vc_release_all(&mut self, vc: usize) {
        self.vcursors[vc].valid = false;
        self.vcursors[vc].skip = 0;
        self.vcursors[vc].nkey = None;
        if !self.vc_live(vc) {
            return;
        }
        let n = self.vcursors[vc].n as usize;
        if self.vcursors[vc].role & VC_SILENT == 0 {
            for i in 0..n {
                let pg = self.vcursors[vc].pages[i];
                self.pager.page_unref(pg);
            }
        }
        self.vcursors[vc].n = 0;
    }

    /// `saveAllCursors(pBt, iRoot, pExcept)`: before a b-tree is written through one cursor,
    /// every other cursor on it loses its position — its pages are given back
    /// (`btreeReleaseAllCursorPages`), most recently opened cursor first. `root == None`: all
    /// b-trees. `except`: the cursor doing the write (`usize::MAX`: none of the cursors
    /// modelled here).
    pub(crate) fn vc_save_others(&mut self, root: Option<Pgno>, except: usize) {
        for i in (0..self.vcursors.len()).rev() {
            if i == except {
                continue;
            }
            let c = &self.vcursors[i];
            if c.root != 0 && root.is_none_or(|r| r == c.root) && (c.n > 0 || c.valid) {
                self.vc_release_all(i);
                self.vcursors[i].at_last = false;
            }
        }
    }

    /// `balance_nonroot` gave page objects other numbers (`sqlite3PagerRekey`): a cursor that
    /// holds a reference on such an object holds it under the new number.
    pub(crate) fn vc_renumber(&mut self, swaps: &[(Pgno, Pgno)]) {
        if swaps.is_empty() {
            return;
        }
        for c in &mut self.vcursors {
            let n = c.n as usize;
            for &(a, b) in swaps {
                for p in &mut c.pages[..n] {
                    if *p == a {
                        *p = b;
                    } else if *p == b {
                        *p = a;
                    }
                }
            }
        }
    }

    /// `moveToRoot`: a cursor that holds pages keeps the root and gives the others back, the
    /// page it is on first; a cursor that holds none takes a reference on the root.
    pub(crate) fn vc_move_to_root(&mut self, vc: usize) -> Result<()> {
        self.vcursors[vc].valid = false;
        self.vcursors[vc].at_last = false;
        self.vcursors[vc].skip = 0;
        self.vcursors[vc].nkey = None;
        // (`sqlite3BtreeClearCursor` of a cursor in CURSOR_REQUIRESSEEK)
        self.vcursors[vc].seek_rowid = None;
        self.vcursors[vc].seek_key = None;
        if self.vc_live(vc) && self.vcursors[vc].n > 0 {
            let silent = self.vcursors[vc].role & VC_SILENT != 0;
            while self.vcursors[vc].n > 1 {
                let i = self.vcursors[vc].n as usize - 1;
                let pg = self.vcursors[vc].pages[i];
                if !silent {
                    self.pager.page_unref(pg);
                }
                self.vcursors[vc].n -= 1;
            }
            return Ok(());
        }
        let root = self.vcursors[vc].root;
        if self.vcursors[vc].role & VC_SILENT == 0 {
            self.pager.page_ref_btree(root)?;
        }
        let c = &mut self.vcursors[vc];
        c.pages[0] = root;
        c.n = 1;
        Ok(())
    }

    /// `moveToChild`: one more page on the stack, with a reference.
    pub(crate) fn vc_move_to_child(&mut self, vc: usize, child: Pgno) -> Result<()> {
        if self.vcursors[vc].n as usize >= VC_MAX_DEPTH {
            return Ok(());
        }
        if self.vcursors[vc].role & VC_SILENT == 0 {
            self.pager.page_ref_btree(child)?;
        }
        let c = &mut self.vcursors[vc];
        c.pages[c.n as usize] = child;
        c.n += 1;
        Ok(())
    }

    /// `moveToChild` through child slot `idx` of the page the cursor is on.
    pub(crate) fn vc_move_to_child_at(&mut self, vc: usize, child: Pgno, idx: usize) -> Result<()> {
        let n = self.vcursors[vc].n as usize;
        if n > 0 && n <= VC_MAX_DEPTH {
            self.vcursors[vc].ai[n - 1] = idx as u16;
        }
        self.vc_move_to_child(vc, child)
    }

    /// The write that follows uses the cursor where the seek before it left it
    /// (`OPFLAG_USESEEKRESULT`): `cur` takes the cursor's path. `false` when the cursor holds
    /// no page.
    pub(crate) fn vc_resume(
        &mut self,
        vc: usize,
        cur: &mut crate::balance::BalCursor,
    ) -> Result<bool> {
        let n = self.vc_depth(vc);
        if n == 0 {
            return Ok(false);
        }
        // The stack must still describe the tree: every interior page leads to the next page
        // through the recorded slot and the last page is a table leaf.
        let usable = self.pager.usable_size() as usize;
        let path: [Pgno; VC_MAX_DEPTH] = self.vcursors[vc].pages;
        let ai: [u16; VC_MAX_DEPTH] = self.vcursors[vc].ai;
        for i in 0..n {
            let pg = path[i];
            if pg == 0 || pg > self.pager.page_count() {
                return Ok(false);
            }
            let hdr = if pg == 1 { 100 } else { 0 };
            let d = self.pager.read_page(pg)?;
            if d.len() < usable || hdr + 12 > d.len() {
                return Ok(false);
            }
            if i + 1 < n {
                if d[hdr] != 0x05 {
                    return Ok(false);
                }
                let n_cell = crate::balance::get2(d, hdr + 3);
                let idx = usize::from(ai[i]);
                let child = if idx == n_cell {
                    crate::balance::get4(d, hdr + 8)
                } else if idx < n_cell {
                    let off = crate::balance::get2(d, hdr + 12 + 2 * idx);
                    if off + 4 > usable {
                        return Ok(false);
                    }
                    crate::balance::get4(d, off)
                } else {
                    return Ok(false);
                };
                if child != path[i + 1] {
                    return Ok(false);
                }
            } else if d[hdr] != 0x0d {
                return Ok(false);
            }
        }
        cur.pg[..n].copy_from_slice(&path[..n]);
        cur.ai[..n].copy_from_slice(&ai[..n]);
        cur.i_page = n - 1;
        Ok(true)
    }

    /// `balance_deeper`: the child page the root's content moved to is already referenced
    /// (it was just allocated); the cursor takes that reference over as `apPage[1]`.
    pub(crate) fn vc_adopt_child(&mut self, vc: usize, child: Pgno) {
        let c = &mut self.vcursors[vc];
        if (c.n as usize) < VC_MAX_DEPTH {
            c.pages[c.n as usize] = child;
            c.n += 1;
        }
    }

    /// `moveToParent`: the page the cursor is on is given back.
    pub(crate) fn vc_move_to_parent(&mut self, vc: usize) {
        if !self.vc_live(vc) {
            return;
        }
        let c = &mut self.vcursors[vc];
        if c.n > 1 {
            c.n -= 1;
            let pg = c.pages[c.n as usize];
            if c.role & VC_SILENT == 0 {
                self.pager.page_unref(pg);
            }
        }
    }

    /// `cursorOnLastPage`: every page of the cursor's stack below the root is the right-most
    /// child of the page above it. Fills `ai` with `aiIdx[]` of the interior levels.
    pub(crate) fn vc_on_last_page(&mut self, vc: usize, ai: &mut [u16]) -> Result<bool> {
        let n = self.vc_depth(vc);
        if n == 0 {
            return Ok(false);
        }
        let usable = self.pager.usable_size() as usize;
        let path: [Pgno; VC_MAX_DEPTH] = self.vcursors[vc].pages;
        for i in 0..n - 1 {
            let pg = path[i];
            if pg == 0 || pg > self.pager.page_count() {
                return Ok(false);
            }
            let hdr = if pg == 1 { 100 } else { 0 };
            let d = self.pager.read_page(pg)?;
            if d.len() < usable || hdr + 12 > d.len() || d[hdr] & 0x08 != 0 {
                return Ok(false);
            }
            if crate::balance::get4(d, hdr + 8) != path[i + 1] {
                return Ok(false);
            }
            ai[i] = crate::balance::get2(d, hdr + 3) as u16;
        }
        Ok(true)
    }

    /// The cursor is valid on cell `ix` of the page it is on (after an insert that left the
    /// page stack alone).
    #[inline]
    pub(crate) fn vc_set_valid(&mut self, vc: usize, ix: usize) {
        let c = &mut self.vcursors[vc];
        c.valid = true;
        c.ix = ix as u16;
    }

    /// Whether the cursor is on page `pgno` (the top of its stack).
    #[inline]
    pub(crate) fn vc_is_on(&self, vc: usize, pgno: Pgno) -> bool {
        let c = &self.vcursors[vc];
        c.n > 0 && c.epoch == self.pager.pcache_epoch() && c.pages[c.n as usize - 1] == pgno
    }

    /// Number of pages the cursor holds (`iPage + 1`).
    #[inline]
    pub(crate) fn vc_depth(&self, vc: usize) -> usize {
        let c = &self.vcursors[vc];
        if c.epoch == self.pager.pcache_epoch() {
            c.n as usize
        } else {
            0
        }
    }

    /// The pages the cursor holds, root first.
    #[inline]
    pub(crate) fn vc_path(&self, vc: usize) -> &[Pgno] {
        let c = &self.vcursors[vc];
        &c.pages[..c.n as usize]
    }
    // ------------------------------------------------------------------------------------
    // Cursor movement as `btree.c` does it, on the real pages. The engine evaluates
    // statements with its own machinery (see `sqliters_pager`'s side store for reads without
    // a reference); these operations are what moves C's cursors, and so what takes and
    // gives back page references, at the points at which C's VDBE program runs `OP_Rewind`,
    // `OP_Next`, `OP_SeekRowid`, ... — the statement executors call them there.
    // ------------------------------------------------------------------------------------

    /// `(flags byte, nCell)` of a page the cursor holds.
    fn vcr_meta(&mut self, pg: Pgno) -> Result<(u8, usize)> {
        let hdr = if pg == 1 { 100 } else { 0 };
        let d = self.pager.read_page(pg)?;
        if hdr + 12 > d.len() {
            return Err(sqliters_types::SqliteError::corrupt("b-tree page buffer too small"));
        }
        Ok((d[hdr], crate::balance::get2(d, hdr + 3)))
    }

    /// Child pointer `idx` of interior page `pg` (`idx == nCell`: the right-most pointer).
    fn vcr_child(&mut self, pg: Pgno, idx: usize) -> Result<Pgno> {
        let hdr = if pg == 1 { 100 } else { 0 };
        let usable = self.pager.usable_size() as usize;
        let d = self.pager.read_page(pg)?;
        let n_cell = crate::balance::get2(d, hdr + 3);
        let child = if idx >= n_cell {
            crate::balance::get4(d, hdr + 8)
        } else {
            let off = crate::balance::get2(d, hdr + 12 + 2 * idx);
            if off + 4 > usable.min(d.len()) {
                return Err(sqliters_types::SqliteError::corrupt("cell offset out of range"));
            }
            crate::balance::get4(d, off)
        };
        if child == 0 || child > self.pager.page_count() {
            return Err(sqliters_types::SqliteError::corrupt("child page number out of range"));
        }
        Ok(child)
    }

    /// The integer key of cell `idx` of table page `pg` (leaf: after the payload size).
    fn vcr_int_key(&mut self, pg: Pgno, idx: usize) -> Result<i64> {
        let hdr = if pg == 1 { 100 } else { 0 };
        let d = self.pager.read_page(pg)?;
        let leaf = d[hdr] & 0x08 != 0;
        let ptr = hdr + if leaf { 8 } else { 12 } + 2 * idx;
        if ptr + 2 > d.len() {
            return Err(sqliters_types::SqliteError::corrupt("cell pointer out of range"));
        }
        let mut off = crate::balance::get2(d, ptr);
        if leaf {
            // payload size varint
            let mut k = 0;
            while off < d.len() && d[off] & 0x80 != 0 && k < 8 {
                off += 1;
                k += 1;
            }
            off += 1;
        } else {
            off += 4;
        }
        let mut v: u64 = 0;
        for i in 0..9 {
            let Some(&b) = d.get(off + i) else {
                return Err(sqliters_types::SqliteError::corrupt("cell extends past end of page"));
            };
            if i == 8 {
                v = (v << 8) | u64::from(b);
                break;
            }
            v = (v << 7) | u64::from(b & 0x7f);
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(v as i64)
    }

    /// `moveToChild` of the real cursor: `aiIdx[iPage] = ix; ix = 0`.
    fn vcr_to_child(&mut self, vc: usize, child: Pgno) -> Result<()> {
        let n = self.vcursors[vc].n as usize;
        if n == 0 || n >= VC_MAX_DEPTH {
            return Err(sqliters_types::SqliteError::corrupt("b-tree deeper than BTCURSOR_MAX_DEPTH"));
        }
        self.vcursors[vc].ai[n - 1] = self.vcursors[vc].ix;
        if self.vcursors[vc].role & VC_SILENT == 0 {
            self.pager.page_ref_btree(child)?;
        }
        let c = &mut self.vcursors[vc];
        c.pages[n] = child;
        c.n += 1;
        c.ix = 0;
        Ok(())
    }

    /// `moveToParent` of the real cursor: `ix = aiIdx[iPage-1]`, the page is given back.
    fn vcr_to_parent(&mut self, vc: usize) {
        let c = &mut self.vcursors[vc];
        let n = c.n as usize;
        c.ix = c.ai[n - 2];
        c.n -= 1;
        let pg = c.pages[n - 1];
        if c.role & VC_SILENT == 0 {
            self.pager.page_unref(pg);
        }
    }

    /// `moveToLeftmost`.
    fn vcr_leftmost(&mut self, vc: usize) -> Result<()> {
        loop {
            let pg = self.vc_top(vc);
            let (fl, _) = self.vcr_meta(pg)?;
            if fl & 0x08 != 0 {
                return Ok(());
            }
            let ix = usize::from(self.vcursors[vc].ix);
            let child = self.vcr_child(pg, ix)?;
            self.vcr_to_child(vc, child)?;
        }
    }

    /// `moveToRightmost`.
    fn vcr_rightmost(&mut self, vc: usize) -> Result<()> {
        loop {
            let pg = self.vc_top(vc);
            let (fl, n_cell) = self.vcr_meta(pg)?;
            if fl & 0x08 != 0 {
                self.vcursors[vc].ix = n_cell.saturating_sub(1) as u16;
                return Ok(());
            }
            self.vcursors[vc].ix = n_cell as u16;
            let child = self.vcr_child(pg, n_cell)?;
            self.vcr_to_child(vc, child)?;
        }
    }

    #[inline]
    fn vc_top(&self, vc: usize) -> Pgno {
        let c = &self.vcursors[vc];
        c.pages[c.n as usize - 1]
    }

    /// `moveToRoot` with what follows it in C: the cursor is on cell 0 of the root; a root
    /// that is an interior page without cells leads to its only child; `false`: the tree is
    /// empty (`SQLITE_EMPTY`, the cursor is `CURSOR_INVALID` and holds the root).
    fn vcr_root(&mut self, vc: usize) -> Result<bool> {
        self.vc_move_to_root(vc)?;
        self.vcursors[vc].ix = 0;
        let root = self.vcursors[vc].pages[0];
        let (fl, n_cell) = self.vcr_meta(root)?;
        if n_cell > 0 {
            return Ok(true);
        }
        if fl & 0x08 == 0 {
            let child = self.vcr_child(root, 0)?;
            self.vcr_to_child(vc, child)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// `sqlite3BtreeFirst`: `false` when the tree is empty.
    pub fn vc_first(&mut self, vc: usize) -> Result<bool> {
        if !self.vcr_root(vc)? {
            return Ok(false);
        }
        self.vcr_leftmost(vc)?;
        self.vcursors[vc].valid = true;
        Ok(true)
    }

    /// `sqlite3BtreeLast`: `false` when the tree is empty.
    pub fn vc_last(&mut self, vc: usize) -> Result<bool> {
        if self.vcursors[vc].valid && self.vcursors[vc].at_last && self.vc_depth(vc) > 0 {
            return Ok(true);
        }
        if !self.vcr_root(vc)? {
            return Ok(false);
        }
        self.vcr_rightmost(vc)?;
        let c = &mut self.vcursors[vc];
        c.valid = true;
        c.at_last = true;
        Ok(true)
    }

    /// `restoreCursorPosition` of a table cursor in CURSOR_REQUIRESSEEK: seeks the saved
    /// rowid (`btreeMoveto`); a cursor that then is on an entry other than the saved one is
    /// in CURSOR_SKIPNEXT.
    fn vcr_restore(&mut self, vc: usize) -> Result<()> {
        if self.vc_depth(vc) == 0
            && self.vcursors[vc].seek_rowid.is_none()
            && self.vcursors[vc].seek_key.is_none()
        {
            // (the cache was emptied under the cursor: nothing to restore from)
            return Ok(());
        }
        if let Some(k) = self.vcursors[vc].seek_rowid.take() {
            let res = self.vc_table_moveto(vc, k, false)?;
            if res != 0 && self.vcursors[vc].valid {
                let c = &mut self.vcursors[vc];
                c.valid = false;
                c.skip = if res < 0 { -1 } else { 1 };
            }
        } else if let Some((key, orders)) = self.vcursors[vc].seek_key.take() {
            let res = self.vc_index_moveto(vc, &key, &orders)?;
            if res != 0 && self.vcursors[vc].valid {
                let c = &mut self.vcursors[vc];
                c.valid = false;
                c.skip = if res < 0 { -1 } else { 1 };
            }
        }
        Ok(())
    }

    /// `sqlite3BtreeNext`: `false` at the end (`SQLITE_DONE`; the cursor is CURSOR_INVALID and
    /// holds the root only).
    pub fn vc_next(&mut self, vc: usize) -> Result<bool> {
        self.vcursors[vc].nkey = None;
        if !self.vcursors[vc].valid {
            self.vcr_restore(vc)?;
            let c = &mut self.vcursors[vc];
            if !c.valid && c.skip == 0 {
                return Ok(false);
            }
            if c.skip != 0 {
                let s = c.skip;
                c.skip = 0;
                c.valid = true;
                if s > 0 {
                    return Ok(true);
                }
            }
        }
        if self.vc_depth(vc) == 0 {
            self.vcursors[vc].valid = false;
            return Ok(false);
        }
        let pg = self.vc_top(vc);
        let (fl, n_cell) = self.vcr_meta(pg)?;
        let leaf = fl & 0x08 != 0;
        self.vcursors[vc].ix += 1;
        let ix = usize::from(self.vcursors[vc].ix);
        if ix >= n_cell {
            if !leaf {
                let child = self.vcr_child(pg, n_cell)?;
                self.vcr_to_child(vc, child)?;
                return self.vcr_leftmost(vc).map(|()| true);
            }
            loop {
                if self.vcursors[vc].n <= 1 {
                    let c = &mut self.vcursors[vc];
                    c.valid = false;
                    c.at_last = false;
                    return Ok(false);
                }
                self.vcr_to_parent(vc);
                let ppg = self.vc_top(vc);
                let (pfl, pn) = self.vcr_meta(ppg)?;
                if usize::from(self.vcursors[vc].ix) < pn {
                    if pfl & 0x01 != 0 {
                        // table b-tree: the divider is not an entry
                        return self.vc_next(vc);
                    }
                    return Ok(true);
                }
            }
        }
        if leaf {
            Ok(true)
        } else {
            self.vcr_leftmost(vc).map(|()| true)
        }
    }

    /// `sqlite3BtreePrevious`: `false` at the start.
    pub fn vc_prev(&mut self, vc: usize) -> Result<bool> {
        self.vcursors[vc].nkey = None;
        self.vcursors[vc].at_last = false;
        if !self.vcursors[vc].valid {
            self.vcr_restore(vc)?;
            let c = &mut self.vcursors[vc];
            if !c.valid && c.skip == 0 {
                return Ok(false);
            }
            if c.skip != 0 {
                let s = c.skip;
                c.skip = 0;
                c.valid = true;
                if s < 0 {
                    return Ok(true);
                }
            }
        }
        if self.vc_depth(vc) == 0 {
            self.vcursors[vc].valid = false;
            return Ok(false);
        }
        let pg = self.vc_top(vc);
        let (fl, _) = self.vcr_meta(pg)?;
        if fl & 0x08 == 0 {
            let ix = usize::from(self.vcursors[vc].ix);
            let child = self.vcr_child(pg, ix)?;
            self.vcr_to_child(vc, child)?;
            self.vcr_rightmost(vc)?;
            return Ok(true);
        }
        while self.vcursors[vc].ix == 0 {
            if self.vcursors[vc].n <= 1 {
                self.vcursors[vc].valid = false;
                return Ok(false);
            }
            self.vcr_to_parent(vc);
        }
        self.vcursors[vc].ix -= 1;
        let pg = self.vc_top(vc);
        let (fl, _) = self.vcr_meta(pg)?;
        if fl & 0x01 != 0 && fl & 0x08 == 0 {
            return self.vc_prev(vc);
        }
        Ok(true)
    }

    /// The rowid of the entry the (valid, table) cursor is on.
    pub(crate) fn vc_rowid(&mut self, vc: usize) -> Result<i64> {
        let pg = self.vc_top(vc);
        let ix = usize::from(self.vcursors[vc].ix);
        self.vcr_int_key(pg, ix)
    }

    /// Whether the cell the (table) cursor says it is on holds `rowid`.
    pub(crate) fn vc_cell_rowid_is(&mut self, vc: usize, rowid: i64) -> Result<bool> {
        let pg = self.vc_top(vc);
        let (fl, n_cell) = self.vcr_meta(pg)?;
        if fl != 0x0d || usize::from(self.vcursors[vc].ix) >= n_cell {
            return Ok(false);
        }
        Ok(self.vc_rowid(vc)? == rowid)
    }

    /// The (valid) cursor steps to the last cell of the leaf it is on — what a run of
    /// `sqlite3BtreeNext` calls inside one leaf comes to.
    fn vc_to_last_cell(&mut self, vc: usize) -> Result<()> {
        if !self.vcursors[vc].valid || self.vcursors[vc].skip != 0 {
            return Ok(());
        }
        let pg = self.vc_top(vc);
        let (fl, n_cell) = self.vcr_meta(pg)?;
        if fl & 0x08 != 0 && n_cell > 0 && usize::from(self.vcursors[vc].ix) < n_cell - 1 {
            self.vcursors[vc].ix = (n_cell - 1) as u16;
            self.vcursors[vc].nkey = None;
        }
        Ok(())
    }

    /// `n` calls of `sqlite3BtreeNext` (fewer if the end is reached): returns how many moved
    /// the cursor to an entry.
    pub fn vc_next_n(&mut self, vc: usize, n: u64) -> Result<u64> {
        let mut done = 0u64;
        while done < n {
            if self.vcursors[vc].valid && self.vcursors[vc].skip == 0 && self.vc_depth(vc) > 0 {
                // inside a leaf: no reference moves
                let pg = self.vc_top(vc);
                let (fl, n_cell) = self.vcr_meta(pg)?;
                let ix = usize::from(self.vcursors[vc].ix);
                if fl & 0x08 != 0 && ix + 1 < n_cell {
                    let room = (n_cell - 1 - ix) as u64;
                    let step = room.min(n - done);
                    self.vcursors[vc].ix = (ix as u64 + step) as u16;
                    self.vcursors[vc].nkey = None;
                    done += step;
                    continue;
                }
            }
            if !self.vc_next(vc)? {
                break;
            }
            done += 1;
        }
        Ok(done)
    }

    /// Whether the cursor is on the last cell of the page it is on.
    fn vc_is_last_cell(&mut self, vc: usize) -> Result<bool> {
        let pg = self.vc_top(vc);
        let (_, n_cell) = self.vcr_meta(pg)?;
        Ok(n_cell > 0 && usize::from(self.vcursors[vc].ix) == n_cell - 1)
    }

    /// `getCellInfo` ran on the row the cursor is on (`OP_Rowid`, `OP_Column`): the rowid is
    /// known to the cursor (`BTCF_ValidNKey`).
    pub fn vc_note_rowid(&mut self, vc: usize) -> Result<()> {
        if self.vcursors[vc].valid && self.vc_depth(vc) > 0 {
            let pg = self.vc_top(vc);
            let (fl, n_cell) = self.vcr_meta(pg)?;
            if fl == 0x0d && usize::from(self.vcursors[vc].ix) < n_cell {
                let k = self.vc_rowid(vc)?;
                self.vcursors[vc].nkey = Some(k);
            } else {
                self.vcursors[vc].nkey = None;
            }
        }
        Ok(())
    }

    /// `sqlite3BtreeTableMoveto(pCur, intKey, biasRight, &res)`: the cursor moves to the
    /// entry with rowid `key` (`0`), or to an entry next to where it would be (`-1`: a
    /// smaller one or the tree is empty, `1`: a larger one).
    pub fn vc_table_moveto(&mut self, vc: usize, key: i64, bias_right: bool) -> Result<i32> {
        if self.vcursors[vc].valid
            && self.vc_depth(vc) > 0
            && let Some(nk) = self.vcursors[vc].nkey
            && !self.vc_cell_rowid_is(vc, nk)?
        {
            // (what the cursor remembers does not describe the page any more: forget it)
            let c = &mut self.vcursors[vc];
            c.valid = false;
            c.nkey = None;
            c.at_last = false;
        }
        if self.vcursors[vc].valid
            && self.vc_depth(vc) > 0
            && let Some(nk) = self.vcursors[vc].nkey
        {
            if nk == key {
                return Ok(0);
            }
            if nk < key {
                if self.vcursors[vc].at_last && self.vc_is_last_cell(vc)? {
                    return Ok(-1);
                }
                // "If the requested key is one more than the previous key, then try to get
                // there using sqlite3BtreeNext() rather than a full binary search."
                if nk.checked_add(1) == Some(key) && self.vc_next(vc)? {
                    let k = self.vc_rowid(vc)?;
                    self.vcursors[vc].nkey = Some(k);
                    if k == key {
                        return Ok(0);
                    }
                }
            }
        }
        if !self.vcr_root(vc)? {
            return Ok(-1);
        }
        self.vcursors[vc].valid = true;
        let usable = self.pager.usable_size() as usize;
        loop {
            let pg = self.vc_top(vc);
            // One look at the page per level: the binary search of `sqlite3BtreeTableMoveto`
            // ("idx = upr>>(1-biasRight)", then halving) on the cell pointer array.
            let (leaf, n_cell, idx, c, exact_leaf, child) = {
                let hdr = if pg == 1 { 100 } else { 0 };
                let d = self.pager.read_page(pg)?;
                if hdr + 12 > d.len() || d.len() < usable {
                    return Err(sqliters_types::SqliteError::corrupt("b-tree page buffer too small"));
                }
                let leaf = d[hdr] & 0x08 != 0;
                let n_cell = crate::balance::get2(d, hdr + 3);
                let ptr0 = hdr + if leaf { 8 } else { 12 };
                if ptr0 + 2 * n_cell > d.len() {
                    return Err(sqliters_types::SqliteError::corrupt("cell pointer out of range"));
                }
                let key_at = |i: usize| -> Result<i64> {
                    let mut off = crate::balance::get2(d, ptr0 + 2 * i);
                    if leaf {
                        let mut k = 0;
                        while off < d.len() && d[off] & 0x80 != 0 && k < 8 {
                            off += 1;
                            k += 1;
                        }
                        off += 1;
                    } else {
                        off += 4;
                    }
                    let mut v: u64 = 0;
                    for j in 0..9 {
                        let Some(&b) = d.get(off + j) else {
                            return Err(sqliters_types::SqliteError::corrupt(
                                "cell extends past end of page",
                            ));
                        };
                        if j == 8 {
                            v = (v << 8) | u64::from(b);
                            break;
                        }
                        v = (v << 7) | u64::from(b & 0x7f);
                        if b & 0x80 == 0 {
                            break;
                        }
                    }
                    Ok(v as i64)
                };
                let mut lwr: i64 = 0;
                let mut upr: i64 = n_cell as i64 - 1;
                let mut idx: i64 = if bias_right { upr } else { upr >> 1 };
                let mut c: i32 = -1;
                let mut exact_leaf = false;
                if n_cell > 0 {
                    loop {
                        let ck = key_at(idx as usize)?;
                        if ck < key {
                            lwr = idx + 1;
                            if lwr > upr {
                                c = -1;
                                break;
                            }
                        } else if ck > key {
                            upr = idx - 1;
                            if lwr > upr {
                                c = 1;
                                break;
                            }
                        } else {
                            if leaf {
                                exact_leaf = true;
                            } else {
                                lwr = idx;
                            }
                            break;
                        }
                        idx = (lwr + upr) >> 1;
                    }
                }
                let child = if leaf {
                    0
                } else if lwr as usize >= n_cell {
                    crate::balance::get4(d, hdr + 8)
                } else {
                    let off = crate::balance::get2(d, hdr + 12 + 2 * lwr as usize);
                    if off + 4 > usable {
                        return Err(sqliters_types::SqliteError::corrupt("cell offset out of range"));
                    }
                    crate::balance::get4(d, off)
                };
                (leaf, n_cell, if leaf { idx } else { lwr }, c, exact_leaf, child)
            };
            if leaf {
                let cur = &mut self.vcursors[vc];
                cur.ix = idx.max(0) as u16;
                if exact_leaf {
                    cur.nkey = Some(key);
                    return Ok(0);
                }
                if n_cell == 0 {
                    cur.valid = false;
                }
                return Ok(c);
            }
            if child == 0 || child > self.pager.page_count() {
                return Err(sqliters_types::SqliteError::corrupt("child page number out of range"));
            }
            self.vcursors[vc].ix = idx as u16;
            self.vcr_to_child(vc, child)?;
        }
    }

    /// [`Self::vc_to_bal`] after checking the stack against the tree: every page of it must
    /// be the child of the page above it; the child slots (`aiIdx[]`) are taken from the
    /// pages. `false` (nothing copied): the stack does not describe the tree any more.
    pub(crate) fn vc_to_bal_checked(
        &mut self,
        vc: usize,
        cur: &mut crate::balance::BalCursor,
    ) -> Result<bool> {
        let n = self.vc_depth(vc);
        if n == 0 {
            return Ok(false);
        }
        let usable = self.pager.usable_size() as usize;
        let path: [Pgno; VC_MAX_DEPTH] = self.vcursors[vc].pages;
        for i in 0..n - 1 {
            let pg = path[i];
            if pg == 0 || pg > self.pager.page_count() {
                return Ok(false);
            }
            let hdr = if pg == 1 { 100 } else { 0 };
            let d = self.pager.read_page(pg)?;
            if d.len() < usable || hdr + 12 > d.len() || d[hdr] & 0x08 != 0 {
                return Ok(false);
            }
            let n_cell = crate::balance::get2(d, hdr + 3);
            let want = path[i + 1];
            let mut slot = None;
            if crate::balance::get4(d, hdr + 8) == want {
                slot = Some(n_cell);
            } else {
                for k in 0..n_cell {
                    let off = crate::balance::get2(d, hdr + 12 + 2 * k);
                    if off + 4 > usable {
                        return Ok(false);
                    }
                    if crate::balance::get4(d, off) == want {
                        slot = Some(k);
                        break;
                    }
                }
            }
            let Some(k) = slot else {
                return Ok(false);
            };
            self.vcursors[vc].ai[i] = k as u16;
        }
        self.vc_to_bal(vc, cur);
        Ok(true)
    }

    /// The cursor's pages as a balance path (`cur` takes the stack and the child slots).
    pub(crate) fn vc_to_bal(&self, vc: usize, cur: &mut crate::balance::BalCursor) {
        let c = &self.vcursors[vc];
        let n = c.n as usize;
        cur.pg[..n].copy_from_slice(&c.pages[..n]);
        cur.ai[..n].copy_from_slice(&c.ai[..n]);
        cur.i_page = n - 1;
        cur.vc = vc;
    }
    // ------------------------------------------------------------------------------------
    // The read functions of this layer walk the tree themselves; the C cursor of the scan is
    // moved next to them (`OP_Rewind` / `OP_SeekGE` / `OP_Next` / `OP_SeekRowid`).
    // ------------------------------------------------------------------------------------

    /// Whether the statement already knows where index `root` stands in its table's index
    /// list (see [`Self::cc_index_order`]).
    pub fn cc_index_order_has(&self, root: Pgno) -> bool {
        self.idx_order.contains(&root)
    }

    /// The index roots of a table the statement writes, in the order of the table's index
    /// list (for the order in which the statement's cursors are closed).
    pub fn cc_index_order(&mut self, roots: &[Pgno]) {
        for &r in roots {
            if !self.idx_order.contains(&r) {
                self.idx_order.push(r);
            }
        }
    }

    /// `root` is the PRIMARY KEY b-tree of a WITHOUT ROWID table (for the order in which the
    /// statement's cursors are closed).
    pub fn cc_mark_wr_root(&mut self, root: Pgno) {
        if !self.wr_roots.contains(&root) {
            self.wr_roots.push(root);
        }
    }

    /// The engine reads rows ahead of C's program (`true`: until the matching `false`; the
    /// statement end resets it): the read functions of this layer do not move C's cursors.
    pub fn cc_silent(&mut self, on: bool) {
        if on {
            self.c_silent += 1;
            return;
        }
        self.c_silent = self.c_silent.saturating_sub(1);
        if self.c_silent == 0 {
            for c in &mut self.vcursors {
                if c.role & VC_SILENT != 0 {
                    c.root = 0;
                    c.n = 0;
                    c.valid = false;
                }
            }
            let base = self.vc_frames.last().copied().unwrap_or(0);
            while self.vcursors.len() > base && self.vcursors.last().is_some_and(|c| c.root == 0) {
                self.vcursors.pop();
            }
        }
    }

    /// `OP_SeekRowid` of a point lookup whose own descent takes the references
    /// (`emu_seek_child` at each step, `emu_seek_leaf` on the leaf): the cursor to move, or
    /// `usize::MAX` when there is nothing left to do — the read functions are silent, or the
    /// cursor knew a row already (the shortcuts of `sqlite3BtreeTableMoveto` then decide, and
    /// the whole seek ran here).
    pub(crate) fn emu_seek_start(&mut self, root: Pgno, rowid: i64) -> Result<usize> {
        if self.pager.page_count() == 0 || root == 0 || self.c_silent > 0 {
            return Ok(usize::MAX);
        }
        let vc = self.vc_open_read(root);
        if self.vcursors[vc].valid
            && self.vc_depth(vc) > 0
            && let Some(nk) = self.vcursors[vc].nkey
            && (nk == rowid
                || nk.checked_add(1) == Some(rowid)
                || (nk < rowid && self.vcursors[vc].at_last))
        {
            // (one of the cases in which `sqlite3BtreeTableMoveto` does not start at the root)
            self.vc_table_moveto(vc, rowid, false)?;
            return Ok(usize::MAX);
        }
        self.vc_move_to_root(vc)?;
        self.vcursors[vc].ix = 0;
        Ok(vc)
    }

    /// `moveToChild` of the seek started with [`Self::emu_seek_start`].
    #[inline]
    pub(crate) fn emu_seek_child(&mut self, vc: usize, child: Pgno, idx: usize) -> Result<()> {
        if vc != usize::MAX {
            self.vc_move_to_child_at(vc, child, idx)?;
        }
        Ok(())
    }

    /// The seek started with [`Self::emu_seek_start`] found `rowid` in cell `ix` of the leaf
    /// the cursor is on.
    #[inline]
    pub(crate) fn emu_seek_leaf(&mut self, vc: usize, ix: usize, rowid: i64) {
        if vc != usize::MAX {
            let c = &mut self.vcursors[vc];
            c.ix = ix as u16;
            c.valid = true;
            c.nkey = Some(rowid);
        }
    }

    /// `OP_SeekRowid` of a point lookup on `root`.
    pub(crate) fn emu_seek_rowid(&mut self, root: Pgno, rowid: i64) -> Result<()> {
        if self.pager.page_count() == 0 || root == 0 || self.c_silent > 0 {
            return Ok(());
        }
        let vc = self.vc_open_read(root);
        self.vc_table_moveto(vc, rowid, false)?;
        Ok(())
    }

    /// A forward scan of table b-tree `root` starts: `OP_Rewind`, or `OP_SeekGE` / `OP_SeekGT`
    /// with the lower bound (`(rowid, inclusive)`).
    pub(crate) fn ss_begin_table(&mut self, root: Pgno, min: Option<(i64, bool)>) -> Result<ScanSync> {
        let vc = self.vc_open_read(root);
        self.read_scans.push(root);
        let on = match min {
            None => self.vc_first(vc)?,
            Some((k, inclusive)) => {
                let res = self.vc_table_moveto(vc, k, false)?;
                if !self.vcursors[vc].valid {
                    false
                } else if res < 0 || (res == 0 && !inclusive) {
                    self.vc_next(vc)?
                } else {
                    true
                }
            }
        };
        Ok(ScanSync { vc, root, on, last: None })
    }

    /// The walker is about to deliver entries of page `pg`: the cursor steps there.
    pub(crate) fn ss_enter(&mut self, s: &mut ScanSync, pg: Pgno) -> Result<()> {
        let mut guard = 0u32;
        while s.on && self.vc_depth(s.vc) > 0 && self.vc_top_page(s.vc) != pg {
            // (the entries left on the page the cursor is on are stepped over at once: only
            // the step off the page moves a reference)
            self.vc_to_last_cell(s.vc)?;
            s.on = self.vc_next(s.vc)?;
            guard += 1;
            if guard > 1_000_000 {
                break;
            }
        }
        Ok(())
    }

    /// The walker is done with page `pg`; the last entry it delivered from it was cell
    /// `last_ix` (`None`: it delivered none).
    pub(crate) fn ss_leave(&mut self, s: &mut ScanSync, pg: Pgno, last_ix: Option<usize>) {
        if let Some(ix) = last_ix
            && s.on
            && self.vc_depth(s.vc) > 0
            && self.vc_top_page(s.vc) == pg
        {
            self.vcursors[s.vc].ix = ix as u16;
            self.vcursors[s.vc].nkey = None;
            s.last = Some((pg, ix as u16));
        }
    }

    /// The scan is over. `broke`: the consumer stopped it (the cursor stays on the last entry
    /// delivered); otherwise the loop ran one `OP_Next` further — to the end of the tree or
    /// to the first entry out of range.
    pub(crate) fn ss_end(&mut self, s: ScanSync, broke: bool) -> Result<()> {
        if let Some(i) = self.read_scans.iter().rposition(|&r| r == s.root) {
            self.read_scans.remove(i);
        }
        if !broke
            && s.on
            && self.vc_depth(s.vc) > 0
            && s.last == Some((self.vc_top_page(s.vc), self.vcursors[s.vc].ix))
        {
            self.vc_next(s.vc)?;
        }
        Ok(())
    }

    /// The cursor takes the position `path` (pages root first, with the cell / child slot on
    /// each): `seek` — as `moveToRoot` and the descent leave it; otherwise as a step along the
    /// tree does (the pages below the part of the stack that stays are given back, the page
    /// the cursor is on first; the new ones are referenced top down). An empty `path`: the
    /// cursor is off the end (it holds the root only).
    pub(crate) fn vc_follow(&mut self, vc: usize, path: &[(Pgno, u16)], seek: bool) -> Result<()> {
        if !seek
            && !path.is_empty()
            && self.vc_depth(vc) == path.len()
            && self.vc_top_page(vc) == path[path.len() - 1].0
        {
            // same page: only the cell changes
            let c = &mut self.vcursors[vc];
            c.ix = path[path.len() - 1].1;
            c.valid = true;
            c.nkey = None;
            c.skip = 0;
            return Ok(());
        }
        if seek || self.vc_depth(vc) == 0 {
            self.vc_move_to_root(vc)?;
        } else {
            let mut keep = 0usize;
            {
                let c = &self.vcursors[vc];
                while keep < path.len() && keep < c.n as usize && c.pages[keep] == path[keep].0 {
                    keep += 1;
                }
            }
            let keep = keep.max(1);
            while self.vc_depth(vc) > keep {
                self.vc_move_to_parent(vc);
            }
            let c = &mut self.vcursors[vc];
            c.valid = false;
            c.nkey = None;
            c.skip = 0;
            c.at_last = false;
        }
        if path.is_empty() {
            return Ok(());
        }
        if self.vcursors[vc].pages[0] != path[0].0 {
            // (not the tree this cursor is on)
            return Ok(());
        }
        let have = self.vc_depth(vc);
        for i in have..path.len() {
            self.vc_move_to_child_at(vc, path[i].0, usize::from(path[i - 1].1))?;
        }
        let c = &mut self.vcursors[vc];
        for (i, p) in path.iter().enumerate().take(path.len() - 1) {
            c.ai[i] = p.1;
        }
        c.ix = path[path.len() - 1].1;
        c.valid = true;
        Ok(())
    }

    /// The index scan the read functions of this layer ran last while they were not moving
    /// C's cursors (see [`IdxScan`]); `None` unless exactly one scan ran since the last call.
    pub fn cc_take_idx_scan(&mut self) -> Option<IdxScan> {
        let one = self.idx_scans.len() == 1;
        let s = self.idx_scans.pop();
        self.idx_scans.clear();
        if one { s } else { None }
    }

    /// The index scans that follow are replayed by the caller (`cc_take_idx_scan`,
    /// `cc_idx_step`, `cc_idx_finish`) instead of being run as a whole.
    pub fn cc_idx_defer(&mut self, on: bool) {
        self.c_defer_idx = on;
        if on {
            self.idx_scans.clear();
        }
    }

    /// Whether an index scan that ends now is kept for a later replay (instead of moving the
    /// read cursor at once).
    #[inline]
    pub(crate) fn idx_scan_is_deferred(&self) -> bool {
        self.c_silent > 0 || self.c_defer_idx
    }

    /// An index scan of this layer is over (`first`: the first entry it delivered, `n`: how
    /// many it delivered, `seek_rec`: its lower bound as a record).
    pub(crate) fn idx_scan_done(&mut self, scan: IdxScan) -> Result<()> {
        if self.idx_scan_is_deferred() {
            // (more than one scan is "not exactly one": nothing further is kept)
            if self.idx_scans.len() < 2 {
                self.idx_scans.push(scan);
            }
            return Ok(());
        }
        let mut scan = scan;
        let role = VC_READ + self.read_scans.iter().filter(|&&r| r == scan.root).count() as u8;
        if scan.n > 0 {
            let last = scan.n - 1;
            self.cc_idx_step(&mut scan, role, last)?;
        }
        self.cc_idx_finish(&mut scan, role)
    }

    /// The cursor `(scan.root, role)` moves to entry number `ord` of the scan (`OP_SeekGE`
    /// for the first one, `OP_Next` from there).
    pub fn cc_idx_step(&mut self, scan: &mut IdxScan, role: u8, ord: u64) -> Result<()> {
        if scan.n == 0 || scan.at_end {
            return Ok(());
        }
        let vc = self.vc_open(scan.root, role);
        self.vcursors[vc].is_index = true;
        if !scan.started {
            scan.started = true;
            scan.pos = 0;
            let Some(first) = scan.first.take() else {
                scan.at_end = true;
                return Ok(());
            };
            let res = self.vc_index_moveto(vc, &first, &scan.orders)?;
            if res != 0 || !self.vcursors[vc].valid {
                scan.at_end = true;
                return Ok(());
            }
            // An entry that is a divider on an interior page: the search went down to the
            // last leaf below its left child, found every entry there smaller and stepped
            // back up (`OP_SeekGE`: "if( res<0 ) sqlite3BtreeNext").
            let top = self.vc_top_page(vc);
            let hdr = if top == 1 { 100 } else { 0 };
            let interior = self.pager.read_page(top)?.get(hdr).is_some_and(|f| f & 0x08 == 0);
            if interior && !scan.reverse {
                if self.vc_prev(vc)? {
                    self.vc_next(vc)?;
                }
            }
        }
        if !scan.reverse && scan.pos < ord {
            let want = ord - scan.pos;
            let moved = self.vc_next_n(vc, want)?;
            scan.pos += moved;
            if moved < want {
                scan.at_end = true;
            }
        }
        while scan.pos < ord {
            if !self.vc_prev(vc)? {
                scan.at_end = true;
                break;
            }
            scan.pos += 1;
        }
        Ok(())
    }

    /// The loop over the scan is over: unless its consumer stopped it, it ran one step
    /// further (to the first entry out of range, or off the end of the index).
    pub fn cc_idx_finish(&mut self, scan: &mut IdxScan, role: u8) -> Result<()> {
        if scan.at_end {
            return Ok(());
        }
        let vc = self.vc_open(scan.root, role);
        self.vcursors[vc].is_index = true;
        if scan.n == 0 {
            if self.pager.page_count() == 0 {
                return Ok(());
            }
            // nothing in range: the seek itself, to where the range would start
            let res = self.vc_index_moveto(vc, &scan.seek_rec, &scan.orders)?;
            if scan.eq_only {
                // "if( eqOnly && r.eqSeen==0 ) goto seek_not_found"
            } else if self.vcursors[vc].valid && !scan.reverse && res < 0 {
                self.vc_next(vc)?;
            } else if self.vcursors[vc].valid && scan.reverse && res > 0 {
                self.vc_prev(vc)?;
            }
            scan.at_end = true;
            return Ok(());
        }
        if scan.broke {
            return Ok(());
        }
        let last = scan.n - 1;
        self.cc_idx_step(scan, role, last)?;
        if !scan.at_end {
            if scan.reverse {
                self.vc_prev(vc)?;
            } else {
                self.vc_next(vc)?;
            }
            scan.at_end = true;
        }
        Ok(())
    }

    #[inline]
    fn vc_top_page(&self, vc: usize) -> Pgno {
        let c = &self.vcursors[vc];
        c.pages[c.n as usize - 1]
    }
}

/// State of the C cursor that accompanies a scan of this layer (see `Btree::ss_begin_table`).
pub(crate) struct ScanSync {
    vc: usize,
    root: Pgno,
    /// The cursor is on an entry.
    on: bool,
    /// Page and cell of the last entry delivered.
    last: Option<(Pgno, u16)>,
}

/// An index scan of this layer, to be replayed on a C cursor: the scan visited `n` entries
/// starting with `first`, in index order (`reverse`: backwards).
pub struct IdxScan {
    pub(crate) root: Pgno,
    pub(crate) first: Option<Vec<u8>>,
    pub(crate) seek_rec: Vec<u8>,
    pub(crate) orders: Vec<sqliters_types::KeySortOrder>,
    pub(crate) n: u64,
    pub(crate) reverse: bool,
    /// Every constraint of the seek is an equality (`OPFLAG_SEEKEQ` / `BTREE_SEEK_EQ`): when
    /// no entry is equal, `OP_SeekGE` leaves the loop without a step.
    pub(crate) eq_only: bool,
    /// The consumer stopped the scan (no step past the last entry).
    pub(crate) broke: bool,
    pub(crate) pos: u64,
    pub(crate) started: bool,
    pub(crate) at_end: bool,
}

impl IdxScan {
    /// Root of the index and number of entries the scan visited.
    pub fn root_and_len(&self) -> (Pgno, u64) {
        (self.root, self.n)
    }
}
