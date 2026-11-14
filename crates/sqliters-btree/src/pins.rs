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
}

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
        });
        self.vcursors.len() - 1
    }

    /// A nested statement starts (a trigger program, a statement a virtual table runs on its
    /// shadow tables): its cursors are its own and are closed when it ends.
    pub fn vc_frame_push(&mut self) {
        self.vc_frames.push(self.vcursors.len());
    }

    /// The nested statement ends: `closeAllCursors` for its frame.
    pub fn vc_frame_pop(&mut self) {
        let base = self.vc_frames.pop().unwrap_or(0);
        self.vc_close_from(base);
    }

    /// The statement ends (`sqlite3VdbeHalt` → `closeAllCursors`): every cursor gives its
    /// pages back, cursors in the order they were opened, each from the root down.
    pub fn vc_close_all(&mut self) {
        if self.vcursors.is_empty() {
            return;
        }
        self.vc_frames.clear();
        self.vc_close_from(0);
    }

    fn vc_close_from(&mut self, base: usize) {
        for i in base..self.vcursors.len() {
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
            return false;
        }
        true
    }

    /// `btreeReleaseAllCursorPages`: "for(i=0; i<pCur->iPage; i++) releasePageNotNull(apPage[i]);
    /// releasePageNotNull(pCur->pPage); pCur->iPage = -1".
    pub(crate) fn vc_release_all(&mut self, vc: usize) {
        self.vcursors[vc].valid = false;
        if !self.vc_live(vc) {
            return;
        }
        let n = self.vcursors[vc].n as usize;
        for i in 0..n {
            let pg = self.vcursors[vc].pages[i];
            self.pager.page_unref(pg);
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
        if self.vc_live(vc) && self.vcursors[vc].n > 0 {
            while self.vcursors[vc].n > 1 {
                let i = self.vcursors[vc].n as usize - 1;
                let pg = self.vcursors[vc].pages[i];
                self.pager.page_unref(pg);
                self.vcursors[vc].n -= 1;
            }
            return Ok(());
        }
        let root = self.vcursors[vc].root;
        self.pager.page_ref(root)?;
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
        self.pager.page_ref(child)?;
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
            self.pager.page_unref(pg);
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
}
