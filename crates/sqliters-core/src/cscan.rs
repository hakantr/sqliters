//! The scan cursor of a DELETE / UPDATE, moved the way C's VDBE program moves it.
//!
//! This engine reads the rows the access plan selects before it processes any of them (those
//! reads do not touch the page cache, see `sqliters_pager`'s side store). C's program is one
//! loop: the cursor steps to the next row (`OP_Next`), the row is tested and — in a one-pass
//! statement — changed right there, through the same cursor. What the page cache does depends
//! on where that cursor is, so the statement executors replay its movements here: before a
//! row is processed the cursor is stepped to it, one `OP_Next` per row the scan visits, and
//! after the last row it runs to the end of the scan.
//!
//! A statement that needs two passes (`ONEPASS_OFF`) runs the whole scan first through a read
//! cursor, which is closed when the write cursors are opened under the same cursor numbers
//! (`sqlite3OpenTableAndIndices`); each row is then found again with `OP_NotExists`.

use crate::ExecContext;
use crate::dml_c::{OnePass, ScanInfo, ScanShape};
use sqliters_btree::{IdxScan, VC_READ, VC_WRITE};
use sqliters_types::{DatabaseId, Pgno, Result};

/// How the loop over a table b-tree starts.
#[derive(Clone, Copy)]
enum Start {
    /// `OP_Rewind`.
    Rewind,
    /// `OP_SeekGE` (`true`: `OP_SeekGT`) to the lower bound of a rowid range.
    Seek(i64, bool),
    /// The first row the loop visits stands in for a lower bound that is not known here.
    Row(i64),
}

impl Start {
    /// Moves the cursor `(root, role)`: whether it is on a row afterwards.
    fn run(self, bt: &mut sqliters_btree::Btree, root: Pgno, role: u8) -> Result<bool> {
        match self {
            Start::Rewind => bt.cc_rewind(root, role),
            Start::Seek(k, strict) => bt.cc_seek_rowid_ge(root, role, k, strict),
            Start::Row(k) => bt.cc_seek_rowid(root, role, k),
        }
    }
}

enum Kind {
    /// `OP_Rewind` (or a seek to the lower bound of a rowid range) .. `OP_Next` over the table.
    Table(Start),
    /// `OP_SeekGE` .. `OP_Next` over an index.
    Index(Box<IdxScan>),
    /// `OP_SeekRowid` for each value of an IN list (ascending); `.1`: values sought so far,
    /// `.2`: how many of them were found.
    RowidList(Vec<i64>, usize, u32),
}

pub(crate) struct CScan {
    db_id: DatabaseId,
    kind: Kind,
    /// Root of the b-tree the scan runs on, and of the table.
    root: Pgno,
    table_root: Pgno,
    one_pass: OnePass,
    /// `OPFLAG_SAVEPOSITION` goes to the delete on the scan cursor (DELETE) / to the insert
    /// on the table cursor (UPDATE).
    is_delete: bool,
    /// Number of entries the scan visits.
    n_scanned: u32,
    /// For each row the statement processes, in processing order: its position among the
    /// visited entries (one-pass statements).
    ords: Vec<u32>,
    pos: u32,
    started: bool,
    at_end: bool,
}

impl CScan {
    /// The scan of the loop of a DELETE / UPDATE; `None` for loops this module does not
    /// replay.
    /// (Boxed, and built out of line: the executors that hold it recurse through triggers.)
    #[inline(never)]
    pub(crate) fn new(
        ctx: &mut ExecContext<'_>,
        db_id: DatabaseId,
        table_root: Pgno,
        scan: ScanInfo,
        one_pass: OnePass,
        is_delete: bool,
        n_scanned: usize,
        ords: Vec<u32>,
        first_rowid: Option<i64>,
    ) -> Option<Box<Self>> {
        let idx_scan = ctx
            .db_set
            .slot_mut(db_id)
            .and_then(|s| s.btree.as_mut())
            .and_then(sqliters_btree::Btree::cc_take_idx_scan);
        let rowid_lo = ctx.stmt_tx.c_rowid_lo.take();
        let rowid_seek = ctx.stmt_tx.c_rowid_seek.take();
        if scan.rowid_seek && n_scanned == 0 {
            // `rowid = X` and no such row: the statement is its one `OP_NotExists` on the
            // table's write cursor (a row that exists is sought when it is processed).
            if let (Some(rid), Some(bt)) = (
                rowid_seek,
                ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()),
            ) {
                let _ = bt.cc_seek_rowid(table_root, VC_WRITE, rid);
            }
            return None;
        }
        let (kind, root) = if scan.shape == ScanShape::FullTable {
            (Kind::Table(Start::Rewind), table_root)
        } else if scan.shape == ScanShape::RowidIn {
            (Kind::RowidList(ctx.stmt_tx.c_rowid_in.take()?, 0, 0), table_root)
        } else if scan.shape == ScanShape::RowidRange {
            // (the loop starts with a seek to its lower bound, or with OP_Rewind when it has
            // none)
            let start = match (rowid_lo, first_rowid) {
                (Some(Some((k, strict))), _) => Start::Seek(k, strict),
                (Some(None), _) => Start::Rewind,
                (None, Some(k)) => Start::Row(k),
                (None, None) => return None,
            };
            (Kind::Table(start), table_root)
        } else if let (Some(idx_root), Some(is)) = (scan.index_root, idx_scan) {
            let (r, n) = is.root_and_len();
            if r != idx_root || n != n_scanned as u64 {
                return None;
            }
            (Kind::Index(Box::new(is)), idx_root)
        } else {
            return None;
        };
        Some(Box::new(Self {
            db_id,
            kind,
            root,
            table_root,
            one_pass,
            is_delete,
            n_scanned: n_scanned as u32,
            ords,
            pos: 0,
            started: false,
            at_end: false,
        }))
    }

    /// Before the first row is processed.
    #[inline(never)]
    pub(crate) fn begin(&mut self, ctx: &mut ExecContext<'_>) -> Result<()> {
        let Some(bt) = ctx.db_set.slot_mut(self.db_id).and_then(|s| s.btree.as_mut()) else {
            return Ok(());
        };
        match self.one_pass {
            OnePass::Off => {
                // pass 1: the whole loop, the keys go to the RowSet
                match &mut self.kind {
                    Kind::Table(first) => {
                        let mut on = first.run(bt, self.root, VC_READ)?;
                        // one OP_Next per row visited; the last one finds the end of the loop
                        if on && self.n_scanned > 0 {
                            on = bt.cc_next_n(self.root, VC_READ, u64::from(self.n_scanned))?
                                == u64::from(self.n_scanned);
                        }
                        let _ = on;
                    }
                    Kind::Index(is) => {
                        if self.n_scanned > 0 {
                            bt.cc_idx_step(is, VC_READ, u64::from(self.n_scanned) - 1)?;
                        }
                        bt.cc_idx_finish(is, VC_READ)?;
                    }
                    Kind::RowidList(vals, done, _) => {
                        while *done < vals.len() {
                            bt.cc_seek_rowid(self.root, VC_READ, vals[*done])?;
                            *done += 1;
                        }
                    }
                }
                // sqlite3OpenTableAndIndices(OP_OpenWrite): the cursor numbers are reused
                bt.cc_close(self.table_root, VC_READ);
                bt.cc_close(self.root, VC_READ);
                self.at_end = true;
            }
            OnePass::Multi => bt.cc_save_position(Some(if self.is_delete {
                self.root
            } else {
                self.table_root
            })),
            OnePass::Single => {}
        }
        Ok(())
    }

    /// Row `i` (in processing order) is about to be processed.
    #[inline(never)]
    pub(crate) fn at_row(&mut self, ctx: &mut ExecContext<'_>, i: usize) -> Result<()> {
        if self.one_pass == OnePass::Off || self.at_end {
            return Ok(());
        }
        let Some(&target) = self.ords.get(i) else {
            return Ok(());
        };
        let Some(bt) = ctx.db_set.slot_mut(self.db_id).and_then(|s| s.btree.as_mut()) else {
            return Ok(());
        };
        match &mut self.kind {
            Kind::Table(first) => {
                if !self.started {
                    self.started = true;
                    if !first.run(bt, self.root, VC_WRITE)? {
                        self.at_end = true;
                        return Ok(());
                    }
                }
                if self.pos < target {
                    let want = u64::from(target - self.pos);
                    let moved = bt.cc_next_n(self.root, VC_WRITE, want)?;
                    self.pos += moved as u32;
                    if moved < want {
                        self.at_end = true;
                    }
                }
            }
            Kind::Index(is) => bt.cc_idx_step(is, VC_WRITE, u64::from(target))?,
            Kind::RowidList(vals, done, found) => {
                while *found <= target && *done < vals.len() {
                    if bt.cc_seek_rowid(self.root, VC_WRITE, vals[*done])? {
                        *found += 1;
                    }
                    *done += 1;
                }
            }
        }
        Ok(())
    }

    /// After the last row: the loop runs on to the end of the scan.
    #[inline(never)]
    pub(crate) fn end(&mut self, ctx: &mut ExecContext<'_>) -> Result<()> {
        let Some(bt) = ctx.db_set.slot_mut(self.db_id).and_then(|s| s.btree.as_mut()) else {
            return Ok(());
        };
        if self.one_pass != OnePass::Off && !self.at_end {
            match &mut self.kind {
                Kind::Table(first) => {
                    // (a loop that visits no row still starts: OP_Rewind / the seek fetches
                    // the root, and the pages down to where the range would begin)
                    if !self.started {
                        self.started = true;
                        if !first.run(bt, self.root, VC_WRITE)? {
                            self.at_end = true;
                        }
                    }
                    if self.n_scanned > 0 {
                        // to the last row the loop visits, then the OP_Next that ends it
                        if !self.at_end && self.pos + 1 < self.n_scanned {
                            let want = u64::from(self.n_scanned - 1 - self.pos);
                            let moved = bt.cc_next_n(self.root, VC_WRITE, want)?;
                            self.pos += moved as u32;
                            if moved < want {
                                self.at_end = true;
                            }
                        }
                        if !self.at_end {
                            bt.cc_next(self.root, VC_WRITE)?;
                            self.at_end = true;
                        }
                    }
                }
                Kind::Index(is) => bt.cc_idx_finish(is, VC_WRITE)?,
                Kind::RowidList(vals, done, _) => {
                    while *done < vals.len() {
                        bt.cc_seek_rowid(self.root, VC_WRITE, vals[*done])?;
                        *done += 1;
                    }
                }
            }
        }
        if self.one_pass == OnePass::Multi {
            bt.cc_save_position(None);
        }
        Ok(())
    }
}
