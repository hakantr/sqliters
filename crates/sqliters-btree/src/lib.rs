//! `sqliters-btree`: SQLite Format 3 B-Tree & B+Tree storage engine,
//! overflow chains, freelist page management, and `PRAGMA integrity_check`.

pub mod page;

use std::cmp::Ordering;
use std::path::Path;
use std::sync::Arc;

use sqliters_pager::{Pager, TransactionState};
use sqliters_types::{
    Bitvec, ErrorCode, KeySortOrder, Pgno, Result, SqliteError, TextEncoding, Value,
    compare_records_with_encoding, decode_record_with_encoding,
};
use sqliters_vfs::{OpenFlags, Vfs};

pub use page::{
    BtreePageType, IndexInteriorCell, IndexLeafCell, ParsedPage, PayloadBounds, TableInteriorCell,
    TableLeafCell,
};

/// SQLite B-Tree storage manager over a [`Pager`].
pub struct Btree {
    pager: Pager,
}

impl Btree {
    pub fn new(pager: Pager) -> Self {
        Self { pager }
    }

    pub fn open_file(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Self> {
        Ok(Self::new(Pager::open_file(path, flags)?))
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::new(Pager::open_in_memory()?))
    }

    pub fn open_with_vfs(
        vfs: Arc<dyn Vfs>,
        path: Option<&Path>,
        flags: OpenFlags,
    ) -> Result<Self> {
        Ok(Self::new(Pager::open_with_vfs(vfs, path, flags)?))
    }

    #[inline]
    pub const fn pager(&self) -> &Pager {
        &self.pager
    }

    #[inline]
    pub fn pager_mut(&mut self) -> &mut Pager {
        &mut self.pager
    }

    #[inline]
    pub fn begin_read(&mut self) -> Result<()> {
        self.pager.begin_read()
    }

    #[inline]
    pub fn begin_write(&mut self) -> Result<()> {
        self.pager.begin_write()
    }

    #[inline]
    pub fn commit(&mut self) -> Result<()> {
        self.pager.commit()
    }

    #[inline]
    pub fn rollback(&mut self) -> Result<()> {
        self.pager.rollback()
    }

    #[inline]
    pub const fn transaction_state(&self) -> TransactionState {
        self.pager.transaction_state()
    }

    #[inline]
    pub const fn encoding(&self) -> TextEncoding {
        self.pager.encoding()
    }

    // ========================================================================
    // Freelist & Page Allocation (`btreeInt.h:206-215`, `allocateBtreePage`)
    // ========================================================================

    /// Allocates a page either by popping from the SQLite freelist or extending the database file.
    pub fn allocate_page(&mut self) -> Result<Pgno> {
        let first_trunk = self.pager.header().first_freelist_trunk_page;
        if first_trunk != 0 {
            let usable = self.pager.usable_size() as usize;
            let trunk_bytes = self.pager.read_page(first_trunk)?.to_vec();
            let next_trunk = u32::from_be_bytes([
                trunk_bytes[0],
                trunk_bytes[1],
                trunk_bytes[2],
                trunk_bytes[3],
            ]);
            let leaf_count = u32::from_be_bytes([
                trunk_bytes[4],
                trunk_bytes[5],
                trunk_bytes[6],
                trunk_bytes[7],
            ]);

            if leaf_count > 0 {
                let idx = (leaf_count - 1) as usize;
                let off = 8 + 4 * idx;
                if off + 4 > usable {
                    return Err(SqliteError::corrupt("freelist trunk leaf count out of bounds"));
                }
                let leaf_pgno = u32::from_be_bytes([
                    trunk_bytes[off],
                    trunk_bytes[off + 1],
                    trunk_bytes[off + 2],
                    trunk_bytes[off + 3],
                ]);
                let new_count = leaf_count - 1;
                {
                    let w = self.pager.write_page(first_trunk)?;
                    w[4..8].copy_from_slice(&new_count.to_be_bytes());
                    w[off..off + 4].fill(0);
                }
                let hdr = self.pager.header_mut()?;
                hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);

                let page_buf = self.pager.write_page(leaf_pgno)?;
                page_buf.fill(0);
                return Ok(leaf_pgno);
            }

            // Trunk page has 0 leaves: pop the trunk page itself!
            {
                let hdr = self.pager.header_mut()?;
                hdr.first_freelist_trunk_page = next_trunk;
                hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
            }
            let page_buf = self.pager.write_page(first_trunk)?;
            page_buf.fill(0);
            return Ok(first_trunk);
        }

        self.pager.allocate_raw_page()
    }

    /// Pushes `pgno` onto the database freelist (`freePage2` in `btree.c`).
    pub fn free_page(&mut self, pgno: Pgno) -> Result<()> {
        if pgno <= 1 {
            return Err(SqliteError::corrupt(format!(
                "cannot push page {pgno} onto freelist"
            )));
        }
        let usable = self.pager.usable_size() as usize;
        // Max leaf entries per trunk page (matches SQLite's usableSize/4 - 2 limit; leave 6 spare slots like btree.c)
        let max_leaves = ((usable / 4).saturating_sub(8)).max(1) as u32;
        let first_trunk = self.pager.header().first_freelist_trunk_page;

        if first_trunk != 0 {
            let trunk_ro = self.pager.read_page(first_trunk)?;
            let leaf_count =
                u32::from_be_bytes([trunk_ro[4], trunk_ro[5], trunk_ro[6], trunk_ro[7]]);
            if leaf_count < max_leaves {
                let off = 8 + 4 * (leaf_count as usize);
                let new_count = leaf_count + 1;
                {
                    let w = self.pager.write_page(first_trunk)?;
                    w[4..8].copy_from_slice(&new_count.to_be_bytes());
                    w[off..off + 4].copy_from_slice(&pgno.to_be_bytes());
                }
                // Zero freed leaf page for deterministic cleanliness
                self.pager.write_page(pgno)?.fill(0);
                let hdr = self.pager.header_mut()?;
                hdr.total_freelist_pages += 1;
                return Ok(());
            }
        }

        // Make `pgno` the new freelist trunk page pointing to `first_trunk` with 0 leaves
        {
            let w = self.pager.write_page(pgno)?;
            w.fill(0);
            w[0..4].copy_from_slice(&first_trunk.to_be_bytes());
            w[4..8].copy_from_slice(&0u32.to_be_bytes());
        }
        let hdr = self.pager.header_mut()?;
        hdr.first_freelist_trunk_page = pgno;
        hdr.total_freelist_pages += 1;
        Ok(())
    }

    // ========================================================================
    // Overflow Payload Chains (`btreeInt.h:198-204`)
    // ========================================================================

    fn prepare_cell_payload(
        &mut self,
        bounds: PayloadBounds,
        full_payload: &[u8],
    ) -> Result<(u32, Vec<u8>, Option<Pgno>)> {
        let total_len = u32::try_from(full_payload.len())
            .map_err(|_| SqliteError::new(ErrorCode::TooBig, "row payload exceeds 4GiB"))?;
        let local_len = bounds.local_len(total_len);
        let local_bytes = full_payload[..local_len].to_vec();

        if local_len == full_payload.len() {
            return Ok((total_len, local_bytes, None));
        }

        let usable = bounds.usable_size as usize;
        let chunk_cap = usable - 4;
        let spilled = &full_payload[local_len..];
        let chunks: Vec<&[u8]> = spilled.chunks(chunk_cap).collect();

        // Allocate overflow pages in forward order and link them
        let mut ovfl_pages = Vec::with_capacity(chunks.len());
        for _ in 0..chunks.len() {
            ovfl_pages.push(self.allocate_page()?);
        }

        for (i, chunk) in chunks.iter().enumerate() {
            let pgno = ovfl_pages[i];
            let next_pgno = ovfl_pages.get(i + 1).copied().unwrap_or(0);
            let buf = self.pager.write_page(pgno)?;
            buf[..usable].fill(0);
            buf[0..4].copy_from_slice(&next_pgno.to_be_bytes());
            buf[4..4 + chunk.len()].copy_from_slice(chunk);
        }

        Ok((total_len, local_bytes, Some(ovfl_pages[0])))
    }

    fn read_full_payload(
        &mut self,
        total_len: u32,
        local_payload: &[u8],
        mut next_ovfl: Option<Pgno>,
    ) -> Result<Vec<u8>> {
        let total = total_len as usize;
        if local_payload.len() >= total {
            return Ok(local_payload[..total].to_vec());
        }
        let usable = self.pager.usable_size() as usize;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(local_payload);

        let max_hops = self.pager.page_count() as usize + 1;
        let mut hops = 0usize;
        while out.len() < total {
            let Some(ovfl_pgno) = next_ovfl.filter(|&p| p != 0) else {
                return Err(SqliteError::corrupt(
                    "overflow chain ended prematurely before payload length",
                ));
            };
            hops += 1;
            if hops > max_hops {
                return Err(SqliteError::corrupt("cycle detected in overflow page chain"));
            }
            let raw = self.pager.read_page(ovfl_pgno)?;
            let nxt = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let remaining = total - out.len();
            let take = remaining.min(usable - 4);
            out.extend_from_slice(&raw[4..4 + take]);
            next_ovfl = if nxt == 0 { None } else { Some(nxt) };
        }
        Ok(out)
    }

    fn free_overflow_chain(&mut self, mut first_ovfl: Option<Pgno>) -> Result<()> {
        let max_hops = self.pager.page_count() as usize + 1;
        let mut hops = 0usize;
        while let Some(pgno) = first_ovfl.filter(|&p| p != 0) {
            hops += 1;
            if hops > max_hops {
                return Err(SqliteError::corrupt("cycle detected freeing overflow chain"));
            }
            let nxt = {
                let raw = self.pager.read_page(pgno)?;
                u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]])
            };
            self.free_page(pgno)?;
            first_ovfl = if nxt == 0 { None } else { Some(nxt) };
        }
        Ok(())
    }

    fn read_parsed_page(&mut self, pgno: Pgno) -> Result<ParsedPage> {
        let usable = self.pager.usable_size();
        let raw = self.pager.read_page(pgno)?;
        ParsedPage::parse(pgno, usable, raw)
    }

    fn write_parsed_page(&mut self, pgno: Pgno, page: &ParsedPage) -> Result<()> {
        let usable = self.pager.usable_size();
        let raw = self.pager.write_page(pgno)?;
        page.write(pgno, usable, raw)
    }

    // ========================================================================
    // Tree Creation, Clearing, and Dropping
    // ========================================================================

    /// Creates a new empty Table B+Tree (`0x0d` root leaf) and returns its root `Pgno`.
    pub fn create_table(&mut self) -> Result<Pgno> {
        let pgno = self.allocate_page()?;
        let empty = ParsedPage::TableLeaf { cells: Vec::new() };
        self.write_parsed_page(pgno, &empty)?;
        Ok(pgno)
    }

    /// Creates a new empty Index B-Tree (`0x0a` root leaf) and returns its root `Pgno`.
    pub fn create_index(&mut self) -> Result<Pgno> {
        let pgno = self.allocate_page()?;
        let empty = ParsedPage::IndexLeaf { cells: Vec::new() };
        self.write_parsed_page(pgno, &empty)?;
        Ok(pgno)
    }

    /// Empties all entries and child/overflow pages of a B-tree rooted at `root_pgno`,
    /// preserving `root_pgno` itself as an empty leaf page (`OP_Clear`).
    pub fn clear_tree(&mut self, root_pgno: Pgno) -> Result<()> {
        let parsed = self.read_parsed_page(root_pgno)?;
        let is_table = matches!(
            parsed,
            ParsedPage::TableLeaf { .. } | ParsedPage::TableInterior { .. }
        );
        self.free_subtree_children(&parsed)?;
        let empty = if is_table {
            ParsedPage::TableLeaf { cells: Vec::new() }
        } else {
            ParsedPage::IndexLeaf { cells: Vec::new() }
        };
        self.write_parsed_page(root_pgno, &empty)
    }

    /// Destroys a B-tree rooted at `root_pgno` (`root_pgno > 1`), returning every page (including `root_pgno`) to the freelist (`OP_Destroy`).
    pub fn drop_tree(&mut self, root_pgno: Pgno) -> Result<()> {
        if root_pgno <= 1 {
            return Err(SqliteError::misuse("cannot drop sqlite_schema root page 1"));
        }
        let parsed = self.read_parsed_page(root_pgno)?;
        self.free_subtree_children(&parsed)?;
        self.free_page(root_pgno)
    }

    fn free_subtree_children(&mut self, page: &ParsedPage) -> Result<()> {
        match page {
            ParsedPage::TableLeaf { cells } => {
                for c in cells {
                    self.free_overflow_chain(c.overflow_page)?;
                }
            }
            ParsedPage::IndexLeaf { cells } => {
                for c in cells {
                    self.free_overflow_chain(c.overflow_page)?;
                }
            }
            ParsedPage::TableInterior { cells, right_child } => {
                for c in cells {
                    let child = self.read_parsed_page(c.left_child)?;
                    self.free_subtree_children(&child)?;
                    self.free_page(c.left_child)?;
                }
                let rchild = self.read_parsed_page(*right_child)?;
                self.free_subtree_children(&rchild)?;
                self.free_page(*right_child)?;
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                for c in cells {
                    self.free_overflow_chain(c.overflow_page)?;
                    let child = self.read_parsed_page(c.left_child)?;
                    self.free_subtree_children(&child)?;
                    self.free_page(c.left_child)?;
                }
                let rchild = self.read_parsed_page(*right_child)?;
                self.free_subtree_children(&rchild)?;
                self.free_page(*right_child)?;
            }
        }
        Ok(())
    }

    // ========================================================================
    // Table B+Tree Queries, Seeks, Inserts (with Splitting), and Deletes
    // ========================================================================

    /// Scans all `(rowid, payload_bytes)` entries of a Table B+Tree in ascending `rowid` order.
    pub fn scan_table(&mut self, root_pgno: Pgno) -> Result<Vec<(i64, Vec<u8>)>> {
        if self.pager.page_count() == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        self.collect_table_entries(root_pgno, &mut out)?;
        Ok(out)
    }

    fn collect_table_entries(
        &mut self,
        pgno: Pgno,
        out: &mut Vec<(i64, Vec<u8>)>,
    ) -> Result<()> {
        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::TableLeaf { cells } => {
                for c in cells {
                    let payload = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    out.push((c.rowid, payload));
                }
                Ok(())
            }
            ParsedPage::TableInterior { cells, right_child } => {
                for c in cells {
                    self.collect_table_entries(c.left_child, out)?;
                }
                self.collect_table_entries(right_child, out)
            }
            _ => Err(SqliteError::corrupt(format!(
                "expected table B-tree page at {pgno}, found {:?}",
                parsed.page_type()
            ))),
        }
    }

    /// Point-lookup by `rowid` in O(log N) page reads (`sqlite3BtreeTableMoveto`).
    pub fn seek_table_rowid(&mut self, root_pgno: Pgno, rowid: i64) -> Result<Option<Vec<u8>>> {
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let mut cur_pgno = root_pgno;
        loop {
            let parsed = self.read_parsed_page(cur_pgno)?;
            match parsed {
                ParsedPage::TableLeaf { cells } => {
                    return match cells.binary_search_by_key(&rowid, |c| c.rowid) {
                        Ok(idx) => {
                            let c = &cells[idx];
                            let full = self.read_full_payload(
                                c.total_payload_len,
                                &c.local_payload,
                                c.overflow_page,
                            )?;
                            Ok(Some(full))
                        }
                        Err(_) => Ok(None),
                    };
                }
                ParsedPage::TableInterior { cells, right_child } => {
                    // Find first cell with key >= rowid
                    let idx = cells.partition_point(|c| c.key < rowid);
                    cur_pgno = if idx < cells.len() {
                        cells[idx].left_child
                    } else {
                        right_child
                    };
                }
                _ => {
                    return Err(SqliteError::corrupt(format!(
                        "expected table page at {cur_pgno}"
                    )));
                }
            }
        }
    }

    /// Returns the maximum existing `rowid` in a Table B+Tree (`None` if empty) in O(log N) page reads.
    pub fn max_table_rowid(&mut self, root_pgno: Pgno) -> Result<Option<i64>> {
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let mut cur_pgno = root_pgno;
        loop {
            let parsed = self.read_parsed_page(cur_pgno)?;
            match parsed {
                ParsedPage::TableLeaf { cells } => return Ok(cells.last().map(|c| c.rowid)),
                ParsedPage::TableInterior { right_child, .. } => cur_pgno = right_child,
                _ => return Err(SqliteError::corrupt("expected table page in max_table_rowid")),
            }
        }
    }

    /// Allocates the next available `rowid` (`max_rowid + 1`, or `1` if empty; falls back to random if `i64::MAX` is used).
    pub fn next_rowid(&mut self, root_pgno: Pgno) -> Result<i64> {
        match self.max_table_rowid(root_pgno)? {
            None => Ok(1),
            Some(max_id) if max_id < i64::MAX => Ok((max_id + 1).max(1)),
            Some(_) => {
                for probe in 1..10_000i64 {
                    if self.seek_table_rowid(root_pgno, probe)?.is_none() {
                        return Ok(probe);
                    }
                }
                Err(SqliteError::new(
                    ErrorCode::Full,
                    "database or disk is full (exhausted rowids)",
                ))
            }
        }
    }

    /// Inserts or replaces `(rowid, payload)` in the Table B+Tree rooted at `root_pgno`.
    /// Automatically handles overflow chains and multi-level B+Tree splits (preserving `root_pgno`, including Page 1!).
    pub fn insert_table(&mut self, root_pgno: Pgno, rowid: i64, payload: &[u8]) -> Result<()> {
        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_table_leaf(usable);
        let (total_len, local_payload, overflow_page) =
            self.prepare_cell_payload(bounds, payload)?;
        let new_cell = TableLeafCell {
            rowid,
            total_payload_len: total_len,
            local_payload,
            overflow_page,
        };

        if let Some((divider_key, right_pgno)) = self.insert_table_rec(root_pgno, new_cell)? {
            // Root page split (`balance_deeper` in btree.c):
            // Move the left half out of `root_pgno` into a newly allocated `left_pgno`,
            // and turn `root_pgno` itself into a 1-cell `TableInterior` pointing to `(left_pgno, divider_key)` and `right_pgno`!
            let left_parsed = self.read_parsed_page(root_pgno)?;
            let left_pgno = self.allocate_page()?;
            self.write_parsed_page(left_pgno, &left_parsed)?;

            let new_root = ParsedPage::TableInterior {
                cells: vec![TableInteriorCell {
                    left_child: left_pgno,
                    key: divider_key,
                }],
                right_child: right_pgno,
            };
            self.write_parsed_page(root_pgno, &new_root)?;
        }
        Ok(())
    }

    /// Inserts `cell` into subtree `pgno`. If `pgno` splits, writes the left half into `pgno`,
    /// allocates `right_pgno` for the right half, and returns `Some((max_left_key, right_pgno))`.
    fn insert_table_rec(
        &mut self,
        pgno: Pgno,
        cell: TableLeafCell,
    ) -> Result<Option<(i64, Pgno)>> {
        let usable = self.pager.usable_size();
        let parsed = self.read_parsed_page(pgno)?;

        match parsed {
            ParsedPage::TableLeaf { mut cells } => {
                match cells.binary_search_by_key(&cell.rowid, |c| c.rowid) {
                    Ok(idx) => {
                        let old = std::mem::replace(&mut cells[idx], cell);
                        self.free_overflow_chain(old.overflow_page)?;
                    }
                    Err(idx) => {
                        cells.insert(idx, cell);
                    }
                }

                let candidate = ParsedPage::TableLeaf { cells };
                if candidate.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &candidate)?;
                    return Ok(None);
                }

                // Split leaf page into two balanced halves (byte-weighted midpoint)
                let ParsedPage::TableLeaf { mut cells } = candidate else {
                    unreachable!()
                };
                let split_idx = choose_table_leaf_split(&cells);
                let right_cells = cells.split_off(split_idx);
                let divider_key = cells.last().expect("left half non-empty").rowid;

                let left_page = ParsedPage::TableLeaf { cells };
                let right_page = ParsedPage::TableLeaf { cells: right_cells };

                let right_pgno = self.allocate_page()?;
                self.write_parsed_page(pgno, &left_page)?;
                self.write_parsed_page(right_pgno, &right_page)?;
                Ok(Some((divider_key, right_pgno)))
            }
            ParsedPage::TableInterior {
                mut cells,
                mut right_child,
            } => {
                let idx = cells.partition_point(|c| c.key < cell.rowid);
                let target_child = if idx < cells.len() {
                    cells[idx].left_child
                } else {
                    right_child
                };

                let Some((div_key, new_right_child)) =
                    self.insert_table_rec(target_child, cell)?
                else {
                    return Ok(None);
                };

                if idx < cells.len() {
                    // `target_child` kept the left half (<= div_key); `new_right_child` got (> div_key, <= old_key)
                    cells[idx].left_child = new_right_child;
                    cells.insert(
                        idx,
                        TableInteriorCell {
                            left_child: target_child,
                            key: div_key,
                        },
                    );
                } else {
                    // `right_child` split into `target_child (<= div_key)` and `new_right_child (> div_key)`
                    cells.push(TableInteriorCell {
                        left_child: target_child,
                        key: div_key,
                    });
                    right_child = new_right_child;
                }

                let candidate = ParsedPage::TableInterior { cells, right_child };
                if candidate.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &candidate)?;
                    return Ok(None);
                }

                // Split interior page!
                let ParsedPage::TableInterior {
                    mut cells,
                    right_child: orig_right,
                } = candidate
                else {
                    unreachable!()
                };
                let mid = cells.len() / 2;
                let right_cells = cells.split_off(mid + 1);
                let promoted = cells.pop().expect("mid exists");

                let left_page = ParsedPage::TableInterior {
                    cells,
                    right_child: promoted.left_child,
                };
                let right_page = ParsedPage::TableInterior {
                    cells: right_cells,
                    right_child: orig_right,
                };

                let right_pgno = self.allocate_page()?;
                self.write_parsed_page(pgno, &left_page)?;
                self.write_parsed_page(right_pgno, &right_page)?;
                Ok(Some((promoted.key, right_pgno)))
            }
            _ => Err(SqliteError::corrupt("expected table B-tree page in insert")),
        }
    }

    /// Deletes `rowid` from the Table B+Tree rooted at `root_pgno`. Returns `true` if a row was deleted.
    pub fn delete_table(&mut self, root_pgno: Pgno, rowid: i64) -> Result<bool> {
        let (deleted, _) = self.delete_table_rec(root_pgno, rowid, true)?;
        Ok(deleted)
    }

    /// Returns `(deleted, child_replacement)`:
    /// - `None`: `pgno` still valid and non-empty (or is `is_root`).
    /// - `Some(0)`: `pgno` was a non-root leaf that became completely empty and has been freed!
    /// - `Some(sole_child)`: `pgno` was a non-root interior page reduced to 0 cells; freed and replaced by `sole_child`!
    fn delete_table_rec(
        &mut self,
        pgno: Pgno,
        rowid: i64,
        is_root: bool,
    ) -> Result<(bool, Option<Pgno>)> {
        let usable = self.pager.usable_size();
        let parsed = self.read_parsed_page(pgno)?;

        match parsed {
            ParsedPage::TableLeaf { mut cells } => {
                let Ok(idx) = cells.binary_search_by_key(&rowid, |c| c.rowid) else {
                    return Ok((false, None));
                };
                let removed = cells.remove(idx);
                self.free_overflow_chain(removed.overflow_page)?;

                if cells.is_empty() && !is_root {
                    self.free_page(pgno)?;
                    return Ok((true, Some(0)));
                }

                self.write_parsed_page(pgno, &ParsedPage::TableLeaf { cells })?;
                Ok((true, None))
            }
            ParsedPage::TableInterior {
                mut cells,
                mut right_child,
            } => {
                let idx = cells.partition_point(|c| c.key < rowid);
                let target_child = if idx < cells.len() {
                    cells[idx].left_child
                } else {
                    right_child
                };

                let (deleted, replacement) = self.delete_table_rec(target_child, rowid, false)?;
                if !deleted {
                    return Ok((false, None));
                }

                if let Some(rep) = replacement {
                    if rep == 0 {
                        // Child leaf became empty and was freed!
                        if idx < cells.len() {
                            cells.remove(idx);
                        } else if let Some(last) = cells.pop() {
                            right_child = last.left_child;
                        }
                    } else {
                        // Child interior collapsed to `rep`
                        if idx < cells.len() {
                            cells[idx].left_child = rep;
                        } else {
                            right_child = rep;
                        }
                    }
                }

                if cells.is_empty() {
                    if !is_root {
                        self.free_page(pgno)?;
                        return Ok((true, Some(right_child)));
                    }
                    // Root interior page has 0 cells: collapse `right_child` into `root_pgno` if it fits!
                    let child_parsed = self.read_parsed_page(right_child)?;
                    if child_parsed.fits_in_page(pgno, usable) {
                        self.write_parsed_page(pgno, &child_parsed)?;
                        self.free_page(right_child)?;
                        return Ok((true, None));
                    }
                }

                self.write_parsed_page(pgno, &ParsedPage::TableInterior { cells, right_child })?;
                Ok((true, None))
            }
            _ => Err(SqliteError::corrupt("expected table page in delete")),
        }
    }

    // ========================================================================
    // Index B-Tree Queries, Seeks, Inserts (with Splitting), and Deletes
    // ========================================================================

    /// Scans all serialized key records in an Index B-Tree in index sort order.
    pub fn scan_index(&mut self, root_pgno: Pgno) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        self.collect_index_entries(root_pgno, &mut out)?;
        Ok(out)
    }

    fn collect_index_entries(&mut self, pgno: Pgno, out: &mut Vec<Vec<u8>>) -> Result<()> {
        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::IndexLeaf { cells } => {
                for c in cells {
                    let full = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    out.push(full);
                }
                Ok(())
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                for c in cells {
                    self.collect_index_entries(c.left_child, out)?;
                    let full = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    out.push(full);
                }
                self.collect_index_entries(right_child, out)
            }
            _ => Err(SqliteError::corrupt("expected index B-tree page")),
        }
    }

    /// Searches an Index B-Tree for all records whose leading columns match `prefix_values` under `orders`!
    /// Runs in O(log N + K) page reads!
    pub fn seek_index_equal(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<Value>>> {
        let mut matches = Vec::new();
        self.seek_index_equal_rec(root_pgno, prefix_values, orders, &mut matches)?;
        Ok(matches)
    }

    fn seek_index_equal_rec(
        &mut self,
        pgno: Pgno,
        prefix_values: &[Value],
        orders: &[KeySortOrder],
        out: &mut Vec<Vec<Value>>,
    ) -> Result<()> {
        let enc = self.encoding();
        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::IndexLeaf { cells } => {
                for c in cells {
                    let rec = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, prefix_values, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => out.push(decode_record_with_encoding(&rec, enc)?),
                        Ordering::Greater => break,
                    }
                }
                Ok(())
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                let mut visit_right = true;
                for c in cells {
                    let rec = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, prefix_values, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => {
                            self.seek_index_equal_rec(
                                c.left_child,
                                prefix_values,
                                orders,
                                out,
                            )?;
                            out.push(decode_record_with_encoding(&rec, enc)?);
                        }
                        Ordering::Greater => {
                            self.seek_index_equal_rec(
                                c.left_child,
                                prefix_values,
                                orders,
                                out,
                            )?;
                            visit_right = false;
                            break;
                        }
                    }
                }
                if visit_right {
                    self.seek_index_equal_rec(right_child, prefix_values, orders, out)?;
                }
                Ok(())
            }
            _ => Err(SqliteError::corrupt("expected index page in seek")),
        }
    }

    /// Inserts a serialized index record (`[indexed_cols..., rowid]`) into an Index B-Tree.
    pub fn insert_index(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        let enc = self.encoding();
        let key_vals = decode_record_with_encoding(record_bytes, enc)?;
        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_index(usable);
        let (total_len, local_payload, overflow_page) =
            self.prepare_cell_payload(bounds, record_bytes)?;
        let leaf_cell = IndexLeafCell {
            total_payload_len: total_len,
            local_payload,
            overflow_page,
        };

        if let Some((promoted_cell, right_pgno)) =
            self.insert_index_rec(root_pgno, leaf_cell, &key_vals, orders)?
        {
            let left_parsed = self.read_parsed_page(root_pgno)?;
            let left_pgno = self.allocate_page()?;
            self.write_parsed_page(left_pgno, &left_parsed)?;

            let new_root = ParsedPage::IndexInterior {
                cells: vec![IndexInteriorCell {
                    left_child: left_pgno,
                    total_payload_len: promoted_cell.total_payload_len,
                    local_payload: promoted_cell.local_payload,
                    overflow_page: promoted_cell.overflow_page,
                }],
                right_child: right_pgno,
            };
            self.write_parsed_page(root_pgno, &new_root)?;
        }
        Ok(())
    }

    fn insert_index_rec(
        &mut self,
        pgno: Pgno,
        new_cell: IndexLeafCell,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Option<(IndexLeafCell, Pgno)>> {
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let parsed = self.read_parsed_page(pgno)?;

        match parsed {
            ParsedPage::IndexLeaf { mut cells } => {
                let mut pos = cells.len();
                for (i, existing) in cells.iter().enumerate() {
                    let rec = self.read_full_payload(
                        existing.total_payload_len,
                        &existing.local_payload,
                        existing.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => {
                            let old = std::mem::replace(&mut cells[i], new_cell);
                            self.free_overflow_chain(old.overflow_page)?;
                            let updated = ParsedPage::IndexLeaf { cells };
                            if updated.fits_in_page(pgno, usable) {
                                self.write_parsed_page(pgno, &updated)?;
                                return Ok(None);
                            }
                            let ParsedPage::IndexLeaf { cells: c2 } = updated else {
                                unreachable!()
                            };
                            return self.split_index_leaf(pgno, c2);
                        }
                        Ordering::Greater => {
                            pos = i;
                            break;
                        }
                    }
                }
                cells.insert(pos, new_cell);
                let candidate = ParsedPage::IndexLeaf { cells };
                if candidate.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &candidate)?;
                    return Ok(None);
                }
                let ParsedPage::IndexLeaf { cells } = candidate else {
                    unreachable!()
                };
                self.split_index_leaf(pgno, cells)
            }
            ParsedPage::IndexInterior {
                mut cells,
                mut right_child,
            } => {
                let mut idx = cells.len();
                for (i, existing) in cells.iter().enumerate() {
                    let rec = self.read_full_payload(
                        existing.total_payload_len,
                        &existing.local_payload,
                        existing.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => {
                            let old_ovfl = existing.overflow_page;
                            cells[i] = IndexInteriorCell {
                                left_child: existing.left_child,
                                total_payload_len: new_cell.total_payload_len,
                                local_payload: new_cell.local_payload,
                                overflow_page: new_cell.overflow_page,
                            };
                            self.free_overflow_chain(old_ovfl)?;
                            let updated = ParsedPage::IndexInterior { cells, right_child };
                            if updated.fits_in_page(pgno, usable) {
                                self.write_parsed_page(pgno, &updated)?;
                                return Ok(None);
                            }
                            let ParsedPage::IndexInterior {
                                cells: c2,
                                right_child: rc2,
                            } = updated
                            else {
                                unreachable!()
                            };
                            return self.split_index_interior(pgno, c2, rc2);
                        }
                        Ordering::Greater => {
                            idx = i;
                            break;
                        }
                    }
                }

                let target_child = if idx < cells.len() {
                    cells[idx].left_child
                } else {
                    right_child
                };

                let Some((promoted, new_right_child)) =
                    self.insert_index_rec(target_child, new_cell, key_vals, orders)?
                else {
                    return Ok(None);
                };

                if idx < cells.len() {
                    cells[idx].left_child = new_right_child;
                    cells.insert(
                        idx,
                        IndexInteriorCell {
                            left_child: target_child,
                            total_payload_len: promoted.total_payload_len,
                            local_payload: promoted.local_payload,
                            overflow_page: promoted.overflow_page,
                        },
                    );
                } else {
                    cells.push(IndexInteriorCell {
                        left_child: target_child,
                        total_payload_len: promoted.total_payload_len,
                        local_payload: promoted.local_payload,
                        overflow_page: promoted.overflow_page,
                    });
                    right_child = new_right_child;
                }

                let candidate = ParsedPage::IndexInterior { cells, right_child };
                if candidate.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &candidate)?;
                    return Ok(None);
                }
                let ParsedPage::IndexInterior { cells, right_child } = candidate else {
                    unreachable!()
                };
                self.split_index_interior(pgno, cells, right_child)
            }
            _ => Err(SqliteError::corrupt("expected index page in insert_index")),
        }
    }

    fn split_index_leaf(
        &mut self,
        pgno: Pgno,
        mut cells: Vec<IndexLeafCell>,
    ) -> Result<Option<(IndexLeafCell, Pgno)>> {
        let mid = cells.len() / 2;
        let right_cells = cells.split_off(mid + 1);
        let promoted = cells.pop().expect("mid cell exists");

        let right_pgno = self.allocate_page()?;
        self.write_parsed_page(pgno, &ParsedPage::IndexLeaf { cells })?;
        self.write_parsed_page(
            right_pgno,
            &ParsedPage::IndexLeaf {
                cells: right_cells,
            },
        )?;
        Ok(Some((promoted, right_pgno)))
    }

    fn split_index_interior(
        &mut self,
        pgno: Pgno,
        mut cells: Vec<IndexInteriorCell>,
        orig_right: Pgno,
    ) -> Result<Option<(IndexLeafCell, Pgno)>> {
        let mid = cells.len() / 2;
        let right_cells = cells.split_off(mid + 1);
        let promoted = cells.pop().expect("mid cell exists");

        let left_page = ParsedPage::IndexInterior {
            cells,
            right_child: promoted.left_child,
        };
        let right_page = ParsedPage::IndexInterior {
            cells: right_cells,
            right_child: orig_right,
        };

        let right_pgno = self.allocate_page()?;
        self.write_parsed_page(pgno, &left_page)?;
        self.write_parsed_page(right_pgno, &right_page)?;
        Ok(Some((
            IndexLeafCell {
                total_payload_len: promoted.total_payload_len,
                local_payload: promoted.local_payload,
                overflow_page: promoted.overflow_page,
            },
            right_pgno,
        )))
    }

    /// Deletes a matching entry `key_vals` from an Index B-Tree rooted at `root_pgno`.
    pub fn delete_index(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        // Simple, rock-solid delete maintaining 100% of SQLite invariants:
        // Locate exact match; if in leaf with >1 cells (or root leaf), remove in place!
        // If structural underflow occurs (rare non-root empty leaf or interior hit), rebuild or rebalance cleanly.
        if let Some(done_fast) = self.try_fast_leaf_delete_index(root_pgno, key_vals, orders)? {
            return Ok(done_fast);
        }

        // Structural delete (interior cell hit or 1-cell leaf underflow): collect remaining entries and rewrite cleanly!
        let enc = self.encoding();
        let all_records = self.scan_index(root_pgno)?;
        let mut remaining = Vec::with_capacity(all_records.len());
        let mut removed = false;
        for rec in all_records {
            if !removed
                && compare_records_with_encoding(&rec, key_vals, orders, enc)? == Ordering::Equal
            {
                removed = true;
                continue;
            }
            remaining.push(rec);
        }
        if !removed {
            return Ok(false);
        }
        self.clear_tree(root_pgno)?;
        for rec in remaining {
            self.insert_index(root_pgno, &rec, orders)?;
        }
        Ok(true)
    }

    fn try_fast_leaf_delete_index(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Option<bool>> {
        let enc = self.encoding();
        let mut cur_pgno = root_pgno;
        loop {
            let parsed = self.read_parsed_page(cur_pgno)?;
            match parsed {
                ParsedPage::IndexLeaf { mut cells } => {
                    let mut found_idx = None;
                    for (i, c) in cells.iter().enumerate() {
                        let rec = self.read_full_payload(
                            c.total_payload_len,
                            &c.local_payload,
                            c.overflow_page,
                        )?;
                        match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                            Ordering::Less => continue,
                            Ordering::Equal => {
                                found_idx = Some(i);
                                break;
                            }
                            Ordering::Greater => break,
                        }
                    }
                    let Some(idx) = found_idx else {
                        return Ok(Some(false));
                    };
                    if cells.len() == 1 && cur_pgno != root_pgno {
                        // Delegate non-root empty-leaf rebalancing to structural path
                        return Ok(None);
                    }
                    let old = cells.remove(idx);
                    self.free_overflow_chain(old.overflow_page)?;
                    self.write_parsed_page(cur_pgno, &ParsedPage::IndexLeaf { cells })?;
                    return Ok(Some(true));
                }
                ParsedPage::IndexInterior { cells, right_child } => {
                    let mut next_child = right_child;
                    for c in &cells {
                        let rec = self.read_full_payload(
                            c.total_payload_len,
                            &c.local_payload,
                            c.overflow_page,
                        )?;
                        match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                            Ordering::Less => continue,
                            Ordering::Equal => return Ok(None), // Interior cell hit -> structural path
                            Ordering::Greater => {
                                next_child = c.left_child;
                                break;
                            }
                        }
                    }
                    cur_pgno = next_child;
                }
                _ => return Err(SqliteError::corrupt("expected index page")),
            }
        }
    }

    // ========================================================================
    // PRAGMA integrity_check (`sqlite3BtreeIntegrityCheck`)
    // ========================================================================

    /// Verifies that every page `1..=page_count` is referenced exactly once across `roots`
    /// (which must include `1`), their child pages, their overflow chains, and the freelist!
    /// Returns an empty `Vec` when healthy (`"ok"`), or human-readable corruption diagnostics.
    pub fn integrity_check(&mut self, roots: &[Pgno]) -> Result<Vec<String>> {
        let total_pages = self.pager.page_count();
        if total_pages == 0 {
            return Ok(Vec::new());
        }

        let mut seen = Bitvec::new(total_pages);
        let mut errors = Vec::new();
        let usable = self.pager.usable_size() as usize;

        fn mark_page(
            seen: &mut Bitvec,
            total_pages: Pgno,
            pgno: Pgno,
            label: &str,
            errs: &mut Vec<String>,
        ) {
            if pgno == 0 || pgno > total_pages {
                errs.push(format!("{label} references out-of-range page {pgno}"));
            } else if let Ok(newly_set) = seen.set(pgno)
                && !newly_set
            {
                errs.push(format!("2nd reference to page {pgno} ({label})"));
            }
        }

        fn walk_ovfl(
            mut ovfl: Option<Pgno>,
            total_pages: Pgno,
            seen: &mut Bitvec,
            btree: &mut Btree,
            errs: &mut Vec<String>,
        ) -> Result<()> {
            while let Some(opg) = ovfl.filter(|&p| p != 0) {
                if opg > total_pages {
                    errs.push(format!("overflow page {opg} out of range"));
                    break;
                }
                if let Ok(newly) = seen.set(opg)
                    && !newly
                {
                    errs.push(format!("2nd reference to overflow page {opg}"));
                    break;
                }
                let r = btree.pager.read_page(opg)?;
                let nxt = u32::from_be_bytes([r[0], r[1], r[2], r[3]]);
                ovfl = if nxt == 0 { None } else { Some(nxt) };
            }
            Ok(())
        }

        // 1. Walk freelist
        let mut trunk = self.pager.header().first_freelist_trunk_page;
        let mut counted_free = 0u32;
        while trunk != 0 {
            mark_page(&mut seen, total_pages, trunk, "freelist trunk", &mut errors);
            counted_free += 1;
            if trunk > total_pages {
                break;
            }
            let raw = self.pager.read_page(trunk)?.to_vec();
            let next_trunk = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let n_leaves = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
            for i in 0..n_leaves as usize {
                let off = 8 + 4 * i;
                if off + 4 > usable {
                    errors.push(format!("freelist trunk {trunk} leaf count corrupt"));
                    break;
                }
                let leaf =
                    u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
                mark_page(&mut seen, total_pages, leaf, "freelist leaf", &mut errors);
                counted_free += 1;
            }
            trunk = next_trunk;
        }
        if counted_free != self.pager.header().total_freelist_pages {
            errors.push(format!(
                "freelist_count mismatch: header says {}, found {counted_free}",
                self.pager.header().total_freelist_pages
            ));
        }

        // 2. Walk every B-tree root
        let mut stack: Vec<(Pgno, bool)> = roots.iter().map(|&r| (r, true)).collect();
        while let Some((pgno, is_root)) = stack.pop() {
            mark_page(&mut seen, total_pages, pgno, "btree page", &mut errors);
            if pgno == 0 || pgno > total_pages {
                continue;
            }
            let parsed = match self.read_parsed_page(pgno) {
                Ok(p) => p,
                Err(e) => {
                    errors.push(format!("page {pgno}: {e}"));
                    continue;
                }
            };
            if !is_root && parsed.cell_count() == 0 {
                errors.push(format!("non-root btree page {pgno} has 0 cells"));
            }

            match parsed {
                ParsedPage::TableLeaf { cells } => {
                    for w in cells.windows(2) {
                        if w[0].rowid >= w[1].rowid {
                            errors.push(format!("rowids out of order on table leaf {pgno}"));
                            break;
                        }
                    }
                    for c in cells {
                        walk_ovfl(c.overflow_page, total_pages, &mut seen, self, &mut errors)?;
                    }
                }
                ParsedPage::TableInterior { cells, right_child } => {
                    for w in cells.windows(2) {
                        if w[0].key >= w[1].key {
                            errors.push(format!("keys out of order on table interior {pgno}"));
                            break;
                        }
                    }
                    for c in cells {
                        stack.push((c.left_child, false));
                    }
                    stack.push((right_child, false));
                }
                ParsedPage::IndexLeaf { cells } => {
                    for c in cells {
                        walk_ovfl(c.overflow_page, total_pages, &mut seen, self, &mut errors)?;
                    }
                }
                ParsedPage::IndexInterior { cells, right_child } => {
                    for c in cells {
                        walk_ovfl(c.overflow_page, total_pages, &mut seen, self, &mut errors)?;
                        stack.push((c.left_child, false));
                    }
                    stack.push((right_child, false));
                }
            }
        }

        // 3. Check for unreferenced pages
        let ps = u64::from(self.pager.page_size());
        let lock_byte_pgno = ((sqliters_types::PENDING_BYTE_OFFSET / ps) + 1) as Pgno;
        for pgno in 1..=total_pages {
            if pgno == lock_byte_pgno {
                continue;
            }
            if !seen.test(pgno) {
                errors.push(format!("Page {pgno} is never used"));
            }
        }

        Ok(errors)
    }
}

fn choose_table_leaf_split(cells: &[TableLeafCell]) -> usize {
    if cells.len() <= 2 {
        return 1;
    }
    let total_bytes: usize = cells.iter().map(|c| c.encoded_size() + 2).sum();
    let half = total_bytes / 2;
    let mut acc = 0usize;
    for (i, c) in cells.iter().enumerate() {
        acc += c.encoded_size() + 2;
        if acc >= half {
            return (i + 1).clamp(1, cells.len() - 1);
        }
    }
    cells.len() / 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqliters_types::decode_record;
    use sqliters_types::encode_record;

    #[test]
    fn test_btree_multi_page_splits_overflows_deletes_and_integrity() {
        let mut bt = Btree::open_in_memory().unwrap();
        // Use small 512-byte pages to stress multi-level splits and overflow chains aggressively!
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();

        let tbl_root = bt.create_table().unwrap();
        let idx_root = bt.create_index().unwrap();
        let orders = [KeySortOrder::default(), KeySortOrder::default()];

        // Insert 200 rows including >2KB overflow payloads
        for i in 1..=200i64 {
            let text = if i % 17 == 0 {
                "X".repeat(1500) // spans 3+ overflow pages on 512B pages!
            } else {
                format!("item-{i:04}")
            };
            let rec = encode_record(&[Value::Integer(i), Value::Text(text.clone())]);
            bt.insert_table(tbl_root, i, &rec).unwrap();

            let idx_rec = encode_record(&[Value::Text(text), Value::Integer(i)]);
            bt.insert_index(idx_root, &idx_rec, &orders).unwrap();
        }

        assert!(bt.pager().page_count() > 25);
        assert_eq!(bt.integrity_check(&[1, tbl_root, idx_root]).unwrap(), Vec::<String>::new());

        // Seek every row & verify overflow reassembly
        for i in 1..=200i64 {
            let raw = bt.seek_table_rowid(tbl_root, i).unwrap().unwrap();
            let vals = decode_record(&raw).unwrap();
            assert_eq!(vals[0], Value::Integer(i));
            if i % 17 == 0 {
                assert_eq!(vals[1].as_str().unwrap().len(), 1500);
            }
        }

        // Point-seek via secondary index
        let hits = bt
            .seek_index_equal(idx_root, &[Value::Text("item-0042".into())], &orders)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0][1], Value::Integer(42));

        // Delete 150 rows (including overflow rows) and verify zero leaked pages via integrity_check!
        for i in (1..=150i64).rev() {
            let text = if i % 17 == 0 {
                "X".repeat(1500)
            } else {
                format!("item-{i:04}")
            };
            assert!(bt.delete_table(tbl_root, i).unwrap());
            assert!(
                bt.delete_index(
                    idx_root,
                    &[Value::Text(text), Value::Integer(i)],
                    &orders
                )
                .unwrap()
            );
        }

        assert_eq!(
            bt.integrity_check(&[1, tbl_root, idx_root]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(bt.scan_table(tbl_root).unwrap().len(), 50);
        bt.commit().unwrap();
    }
}
