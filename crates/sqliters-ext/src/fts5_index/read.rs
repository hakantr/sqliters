//! Reader half of `ext/fts5/fts5_index.c`: reverse segment iteration, position-list
//! outputs and column filtering, prefix and tokendata iterators, `sqlite3Fts5IndexQuery`
//! and the `sqlite3Fts5Iter*` interface. The function names are the C names in snake case.
#![forbid(unsafe_code)]

use super::*;

pub(crate) const FTS5INDEX_QUERY_PREFIX: i32 = 0x0001;
pub(crate) const FTS5INDEX_QUERY_DESC: i32 = 0x0002;
#[allow(dead_code)] // (SQLITE_DEBUG builds of C only)
pub(crate) const FTS5INDEX_QUERY_TEST_NOIDX: i32 = 0x0004;
pub(crate) const FTS5INDEX_QUERY_SCAN: i32 = 0x0008;
pub(crate) const FTS5INDEX_QUERY_SKIPEMPTY: i32 = 0x0010;
pub(crate) const FTS5INDEX_QUERY_NOOUTPUT: i32 = 0x0020;
pub(crate) const FTS5INDEX_QUERY_SKIPHASH: i32 = 0x0040;
pub(crate) const FTS5INDEX_QUERY_NOTOKENDATA: i32 = 0x0080;
pub(crate) const FTS5INDEX_QUERY_SCANONETERM: i32 = 0x0100;

const FTS5_MERGE_NLIST: usize = 16;

/// `fts5IndexSkipVarint`.
#[inline]
fn skip_varint(a: &[u8], i_off: &mut usize) {
    let i_end = *i_off + 9;
    loop {
        let b = byte_at(a, *i_off);
        *i_off += 1;
        if (b & 0x80) == 0 || *i_off >= i_end {
            break;
        }
    }
}

/// Appends `a[start..end]`; bytes past the end of `a` read as zero.
fn append_range(out: &mut Vec<u8>, a: &[u8], start: usize, end: usize) {
    if end <= start {
        return;
    }
    if end <= a.len() {
        out.extend_from_slice(&a[start..end]);
    } else {
        for i in start..end {
            out.push(byte_at(a, i));
        }
    }
}

/// `fts5IndexColsetTest`.
#[inline]
fn colset_test(colset: &[i32], i_col: i32) -> bool {
    colset.contains(&i_col)
}

/// `sqlite3Fts5PoslistNext64`: returns true at EOF.
pub(crate) fn poslist_next64(a: &[u8], n: i32, pi: &mut i32, pi_off: &mut i64) -> bool {
    let mut i = *pi;
    if i >= n {
        // EOF
        *pi_off = -1;
        return true;
    }
    let mut i_off = *pi_off;
    let (v, nb) = get_varint32(a, i as usize);
    i += nb as i32;
    let mut i_val = v as u32;
    if i_val <= 1 {
        if i_val == 0 {
            *pi = i;
            return false;
        }
        let (v, nb) = get_varint32(a, i as usize);
        i += nb as i32;
        i_off = i64::from(v as u32) << 32;
        let (v, nb) = get_varint32(a, i as usize);
        i += nb as i32;
        i_val = v as u32;
        if i_val < 2 {
            // This is a corrupt record. So stop parsing it here.
            *pi_off = -1;
            return true;
        }
        *pi_off = i_off + i64::from((i_val - 2) & 0x7FFF_FFFF);
    } else {
        *pi_off = (i_off & (0x7FFF_FFFFi64 << 32))
            + ((i_off.wrapping_add(i64::from(i_val - 2))) & 0x7FFF_FFFF);
    }
    *pi = i;
    false
}

/// `Fts5PoslistReader`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PoslistReader {
    pub(crate) n: i32,
    pub(crate) i: i32,
    /// "For client use" in C (marks a buffer to free): unused here.
    #[allow(dead_code)]
    pub(crate) b_flag: bool,
    pub(crate) b_eof: bool,
    pub(crate) i_pos: i64,
}

impl PoslistReader {
    /// `sqlite3Fts5PoslistReaderInit`.
    pub(crate) fn init(a: &[u8], n: i32) -> Self {
        let mut r = Self {
            n,
            ..Self::default()
        };
        r.next(a);
        r
    }
    /// `sqlite3Fts5PoslistReaderNext`.
    pub(crate) fn next(&mut self, a: &[u8]) -> bool {
        if poslist_next64(a, self.n, &mut self.i, &mut self.i_pos) {
            self.b_eof = true;
        }
        self.b_eof
    }
}

/// `sqlite3Fts5PoslistSafeAppend`.
pub(crate) fn poslist_safe_append(buf: &mut Vec<u8>, pi_prev: &mut i64, i_pos: i64) {
    if i_pos >= *pi_prev {
        const COLMASK: i64 = 0x7FFF_FFFFi64 << 32;
        if (i_pos & COLMASK) != (*pi_prev & COLMASK) {
            buf.push(1);
            put_varint(buf, (i_pos >> 32) as u64);
            *pi_prev = i_pos & COLMASK;
        }
        put_varint(buf, ((i_pos - *pi_prev) + 2) as u64);
        *pi_prev = i_pos;
    }
}

/// `fts5GetPoslistSize`: (bytes consumed, nSz/2, delete flag).
#[inline]
fn get_poslist_size(a: &[u8], off: usize) -> (usize, i32, bool) {
    let (n_sz, n) = get_varint32(a, off);
    (n, n_sz / 2, (n_sz & 1) != 0)
}

// ---------------------------------------------------------------------------------------
// Chunk callbacks (`fts5PoslistCallback`, `fts5PoslistFilterCallback`,
// `fts5PoslistOffsetsCallback`)
// ---------------------------------------------------------------------------------------

enum ChunkCb<'a> {
    /// `fts5PoslistCallback`.
    Copy,
    /// `fts5PoslistFilterCallback` (`PoslistCallbackCtx`).
    Filter { colset: &'a [i32], e_state: i32 },
    /// `fts5PoslistOffsetsCallback` (`PoslistOffsetsCtx`).
    Offsets {
        colset: &'a [i32],
        i_read: i32,
        i_write: i32,
    },
}

impl ChunkCb<'_> {
    /// One chunk `a[off..off+n]` of a position list.
    fn chunk(&mut self, out: &mut Vec<u8>, a: &[u8], off: usize, n: i32) {
        if n <= 0 {
            return;
        }
        let n_chunk = n as usize;
        match self {
            ChunkCb::Copy => append_range(out, a, off, off + n_chunk),
            ChunkCb::Offsets {
                colset,
                i_read,
                i_write,
            } => {
                let mut i = 0usize;
                while i < n_chunk {
                    let (v, nb) = get_varint32(a, off + i);
                    i += nb;
                    let i_val = v.wrapping_add(*i_read).wrapping_sub(2);
                    *i_read = i_val;
                    if colset_test(colset, i_val) {
                        put_varint(
                            out,
                            i64::from(i_val.wrapping_add(2).wrapping_sub(*i_write)) as u64,
                        );
                        *i_write = i_val;
                    }
                }
            }
            ChunkCb::Filter { colset, e_state } => {
                // Search through to find the first varint with value 1. This is the start
                // of the next columns hits.
                let mut i = 0usize;
                let mut i_start = 0usize;
                if *e_state == 2 {
                    let (i_col, nb) = get_varint32(a, off + i);
                    i += nb;
                    if colset_test(colset, i_col) {
                        *e_state = 1;
                        put_varint(out, 1);
                    } else {
                        *e_state = 0;
                    }
                }
                loop {
                    while i < n_chunk && byte_at(a, off + i) != 0x01 {
                        let mut p = off + i;
                        skip_varint(a, &mut p);
                        i = p - off;
                    }
                    if *e_state != 0 {
                        append_range(out, a, off + i_start, off + i);
                    }
                    if i < n_chunk {
                        i_start = i;
                        i += 1;
                        if i >= n_chunk {
                            *e_state = 2;
                        } else {
                            let (i_col, nb) = get_varint32(a, off + i);
                            i += nb;
                            *e_state = i32::from(colset_test(colset, i_col));
                            if *e_state != 0 {
                                append_range(out, a, off + i_start, off + i);
                                i_start = i;
                            }
                        }
                    }
                    if i >= n_chunk {
                        break;
                    }
                }
            }
        }
    }
}

/// `fts5IndexExtractColset`: `Some((start, len))` when the result is the contiguous part
/// `a[start..start+len]` of the input (`pColset->nCol==1`), `None` when it was appended to
/// `out`.
fn index_extract_colset(
    colset: &[i32],
    a: &[u8],
    pos: usize,
    n_pos: usize,
    out: &mut Vec<u8>,
) -> Option<(usize, usize)> {
    let mut p = pos;
    let mut a_copy = p;
    let p_end = pos + n_pos;
    let mut i = 0usize;
    let mut i_current: i32 = 0;
    loop {
        while colset[i] < i_current {
            i += 1;
            if i == colset.len() {
                return None;
            }
        }
        // Advance pointer p until it points to pEnd or an 0x01 byte that is not part of a
        // varint
        while p < p_end && byte_at(a, p) != 0x01 {
            while p < p_end {
                let b = byte_at(a, p);
                p += 1;
                if (b & 0x80) == 0 {
                    break;
                }
            }
        }
        if colset[i] == i_current {
            if colset.len() == 1 {
                return Some((a_copy, p - a_copy));
            }
            append_range(out, a, a_copy, p);
        }
        if p >= p_end {
            return None;
        }
        a_copy = p;
        p += 1;
        i_current = i32::from(byte_at(a, p));
        p += 1;
        if (i_current & 0x80) != 0 {
            p -= 1;
            let (v, nb) = get_varint32(a, p);
            p += nb;
            i_current = v;
        }
    }
}

impl Fts5Index {
    // -----------------------------------------------------------------------------------
    // Reverse segment iteration
    // -----------------------------------------------------------------------------------

    /// `fts5SegIterReverseInitPage`.
    pub(super) fn seg_iter_reverse_init_page(&self, it: &mut SegIter) {
        let Some(leaf) = it.leaf.as_ref() else {
            return;
        };
        let mut n = i64::from(leaf.sz_leaf);
        let mut i = it.i_leaf_offset;
        let a = &leaf.p;
        if n > i64::from(it.i_endof_doclist) {
            n = i64::from(it.i_endof_doclist);
        }
        it.a_rowid_offset.clear();
        loop {
            if i >= n {
                break;
            }
            if self.config.detail == Detail::None {
                if i < n && byte_at(a, i as usize) == 0 {
                    i += 1;
                    if i < n && byte_at(a, i as usize) == 0 {
                        i += 1;
                    }
                }
            } else {
                let (nb, n_pos, _) = get_poslist_size(a, i as usize);
                i += nb as i64;
                i += i64::from(n_pos);
            }
            if i >= n {
                break;
            }
            let (delta, nb) = get_varint(a, i as usize);
            i += nb as i64;
            it.i_rowid = it.i_rowid.wrapping_add(delta as i64);
            it.a_rowid_offset.push(it.i_leaf_offset as i32);
            it.i_leaf_offset = i;
        }
        it.i_rowid_offset = it.a_rowid_offset.len() as i32;
        self.seg_iter_load_npos(it);
    }

    /// `fts5SegIterReverseNewPage`.
    pub(super) fn seg_iter_reverse_new_page(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
    ) -> Result<()> {
        it.leaf = None;
        while it.i_leaf_pgno > it.i_term_leaf_pgno {
            it.i_leaf_pgno -= 1;
            let new = self.leaf_read(ctx, segment_rowid(it.seg.i_segid, it.i_leaf_pgno))?;
            // iTermLeafOffset may be equal to szLeaf if the term is the last thing on the
            // page - i.e. the first rowid is on the following page. In this case leave
            // pIter->pLeaf==0, this iterator is at EOF.
            let mut take = false;
            if it.i_leaf_pgno == it.i_term_leaf_pgno {
                if it.i_term_leaf_offset < new.sz_leaf {
                    take = true;
                    it.i_leaf_offset = i64::from(it.i_term_leaf_offset);
                }
            } else {
                let i_rowid_off = new.first_rowid_off();
                if i_rowid_off != 0 {
                    if i_rowid_off >= new.sz_leaf {
                        return Err(self.corrupt_iter(it));
                    }
                    take = true;
                    it.i_leaf_offset = i64::from(i_rowid_off);
                }
            }
            if take {
                let (v, nb) = get_varint(&new.p, it.i_leaf_offset as usize);
                it.i_rowid = v as i64;
                it.i_leaf_offset += nb as i64;
                it.leaf = Some(new);
                break;
            }
        }
        if let Some(nn) = it.leaf.as_ref().map(|l| l.nn) {
            it.i_endof_doclist = nn + 1;
            self.seg_iter_reverse_init_page(it);
        }
        Ok(())
    }

    /// `fts5SegIterNext_Reverse`.
    pub(super) fn seg_iter_next_reverse(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
    ) -> Result<()> {
        if it.i_rowid_offset > 0 {
            it.i_rowid_offset -= 1;
            it.i_leaf_offset = i64::from(
                it.a_rowid_offset
                    .get(it.i_rowid_offset as usize)
                    .copied()
                    .unwrap_or(0),
            );
            self.seg_iter_load_npos(it);
            let mut i_off = it.i_leaf_offset;
            if self.config.detail != Detail::None {
                i_off += i64::from(it.n_pos);
            }
            if let Some(leaf) = it.leaf.as_ref() {
                let (delta, _) = get_varint(&leaf.p, i_off as usize);
                it.i_rowid = it.i_rowid.wrapping_sub(delta as i64);
            }
            Ok(())
        } else {
            self.seg_iter_reverse_new_page(ctx, it)
        }
    }

    /// `fts5SegIterReverse`: the iterator points to the first rowid of a doclist; set it up
    /// to iterate through the doclist in reverse order.
    pub(super) fn seg_iter_reverse(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        it: &mut SegIter,
    ) -> Result<()> {
        let mut last: Option<Data> = None;
        let mut pgno_last = 0;
        sqliters_types::dtrace!(
            "TR|f5q|rev|segid={} dl={}",
            it.seg.i_segid,
            i32::from(it.dlidx.is_some() && self.config.i_version == FTS5_CURRENT_VERSION)
        );
        if it.dlidx.is_some() && self.config.i_version == FTS5_CURRENT_VERSION {
            let i_segid = it.seg.i_segid;
            pgno_last = it.dlidx.as_ref().map_or(0, |d| d.a_lvl[0].i_leaf_pgno);
            last = Some(self.leaf_read(ctx, segment_rowid(i_segid, pgno_last))?);
        } else {
            let Some(leaf) = it.leaf.as_ref() else {
                return Ok(());
            };
            // Currently, Fts5SegIter.iLeafOffset points to the first byte of
            // position-list content for the current rowid. Back it up so that it points
            // to the start of the position-list size field.
            let mut i_poslist: usize = if it.i_term_leaf_pgno == it.i_leaf_pgno {
                it.i_term_leaf_offset.max(0) as usize
            } else {
                4
            };
            skip_varint(&leaf.p, &mut i_poslist);
            it.i_leaf_offset = i_poslist as i64;

            // If this condition is true then the largest rowid for the current term may
            // not be stored on the current page. So search forward to see where said
            // rowid really is.
            if it.i_endof_doclist >= leaf.sz_leaf {
                let seg = it.seg.clone();
                // The last rowid in the doclist may not be on the current page. Search
                // forward to find the page containing the last rowid.
                let mut pgno = it.i_leaf_pgno + 1;
                while pgno <= seg.pgno_last {
                    let i_abs = segment_rowid(seg.i_segid, pgno);
                    let new = self.leaf_read(ctx, i_abs)?;
                    let i_rowid = new.first_rowid_off();
                    let b_termless = new.is_termless();
                    if i_rowid != 0 {
                        last = Some(new);
                        pgno_last = pgno;
                    }
                    if !b_termless {
                        break;
                    }
                    pgno += 1;
                }
            }
        }

        // If pLast is NULL at this point, then the last rowid for this doclist lies on
        // the page currently indicated by the iterator. In this case pIter->iLeafOffset
        // is already set to point to the position-list size field associated with the
        // first relevant rowid on the page.
        //
        // Or, if pLast is non-NULL, then it is the page that contains the last rowid. In
        // this case configure the iterator so that it points to the first rowid on this
        // page.
        if let Some(last) = last {
            it.i_leaf_pgno = pgno_last;
            let mut i_off = last.first_rowid_off();
            if i_off > last.sz_leaf {
                it.leaf = Some(last);
                return Err(self.corrupt_iter(it));
            }
            let (v, nb) = get_varint(&last.p, i_off as usize);
            it.i_rowid = v as i64;
            i_off += nb as i32;
            it.i_leaf_offset = i64::from(i_off);
            if last.is_termless() {
                it.i_endof_doclist = last.nn + 1;
            } else {
                it.i_endof_doclist = last.first_term_off();
            }
            it.leaf = Some(last);
        }
        self.seg_iter_reverse_init_page(it);
        Ok(())
    }

    /// `SELECT pgno FROM %_idx WHERE segid=? AND term>? ORDER BY term ASC LIMIT 1`.
    fn idx_lookup_gt(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_segid: i32,
        term: &[u8],
    ) -> Result<Option<i64>> {
        if let Some(hit) = ctx.shadow_wr_seek(&self.idx_tbl, i64::from(i_segid), term, false)? {
            return Ok(hit.and_then(|r| r.get(2).map(Value::to_integer_lossy)));
        }
        let rows = ctx.query_sql(
            &self.sql_idx_next_select,
            &[
                Value::Integer(i64::from(i_segid)),
                Value::Blob(term.to_vec()),
            ],
        )?;
        Ok(rows
            .first()
            .and_then(|r| r.first())
            .map(Value::to_integer_lossy))
    }

    /// `fts5SegIterNextInit`: like `fts5SegIterSeekInit`, but the iterator points to the
    /// first term following the page with `term` on it.
    fn seg_iter_next_init(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        term: &[u8],
        seg_pos: (usize, usize),
        seg: &StructureSegment,
        it: &mut SegIter,
    ) -> Result<()> {
        let mut i_pg: i32 = -1;
        let mut b_dlidx = false;
        if let Some(val) = self.idx_lookup_gt(ctx, seg.i_segid, term)? {
            i_pg = (val >> 1) as i32;
            b_dlidx = (val & 1) != 0;
        }
        sqliters_types::dtrace!(
            "TR|f5q|idxn|segid={} pg={} dlidx={}",
            seg.i_segid,
            i_pg,
            i32::from(b_dlidx)
        );
        *it = SegIter::default();
        it.seg_pos = Some(seg_pos);
        it.seg = seg.clone();
        it.flags |= FTS5_SEGITER_ONETERM;
        if i_pg >= 0 {
            it.i_leaf_pgno = i_pg - 1;
            self.seg_iter_next_page(ctx, it)?;
        }
        if let Some(leaf) = it.leaf.as_ref() {
            it.i_pgidx_off = leaf.sz_leaf;
            let (i_term_off, nb) = get_varint32(&leaf.p, it.i_pgidx_off as usize);
            it.i_pgidx_off += nb as i32;
            if i_term_off > leaf.sz_leaf {
                return Err(corrupt());
            }
            it.i_leaf_offset = i64::from(i_term_off);
            self.seg_iter_load_term(ctx, it, 0)?;
            self.seg_iter_load_npos(it);
            if b_dlidx {
                self.seg_iter_load_dlidx(ctx, it)?;
            }
        }
        Ok(())
    }

    /// `fts5SegIterHashInit`.
    pub(super) fn seg_iter_hash_init_q(&mut self, term: Option<&[u8]>, flags: i32, it: &mut SegIter) {
        *it = SegIter::default();
        it.is_hash = true;
        let mut leaf: Option<Data> = None;
        if term.is_none() || (flags & FTS5INDEX_QUERY_SCAN) != 0 {
            let prefix = term.unwrap_or(&[]);
            let entries = match self.hash.as_mut() {
                Some(h) => {
                    h.scan_touch(prefix);
                    h.scan_copy(prefix)
                }
                None => Vec::new(),
            };
            // The call to sqlite3Fts5HashScanInit() causes the hash table to fill the size
            // field of all existing position lists. This means they can no longer be
            // appended to. Since the only scenario in which they can be appended to is if
            // the previous operation on this table was a DELETE, by clearing the
            // Fts5Index.bDelete flag we can avoid this possibility altogether.
            self.b_delete = false;
            if let Some((key, list)) = entries.first() {
                it.term.extend_from_slice(key);
                leaf = Some(Data::from_blob(list.clone()));
            }
            it.hash_entries = Some(entries);
            it.hash_pos = 0;
        } else if let Some(t) = term {
            if let Some(list) = self.hash.as_ref().and_then(|h| h.query(t)) {
                it.term.extend_from_slice(t);
                leaf = Some(Data::from_blob(list));
            }
            it.flags |= FTS5_SEGITER_ONETERM;
        }
        sqliters_types::dtrace!(
            "TR|f5q|hash|scan={} hit={}",
            i32::from(term.is_none() || (flags & FTS5INDEX_QUERY_SCAN) != 0),
            i32::from(leaf.is_some())
        );
        if let Some(mut d) = leaf {
            d.sz_leaf = d.nn;
            let (rowid, n) = get_varint(&d.p, 0);
            it.i_rowid = rowid as i64;
            it.i_leaf_offset = n as i64;
            it.i_endof_doclist = d.nn;
            it.leaf = Some(d);
            if (flags & FTS5INDEX_QUERY_DESC) != 0 {
                it.flags |= FTS5_SEGITER_REVERSE;
                self.seg_iter_reverse_init_page(it);
            } else {
                self.seg_iter_load_npos(it);
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // Position-list outputs
    // -----------------------------------------------------------------------------------

    /// `fts5ChunkIterate`.
    fn chunk_iterate(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &mut SegIter,
        out: &mut Vec<u8>,
        cb: &mut ChunkCb<'_>,
    ) -> Result<()> {
        let mut n_rem = i64::from(seg.n_pos);
        let Some(leaf) = seg.leaf.as_ref() else {
            return Ok(());
        };
        let n_chunk = n_rem.min(i64::from(leaf.sz_leaf) - seg.i_leaf_offset);
        let mut pgno = seg.i_leaf_pgno;
        let mut pgno_save = 0;
        if (seg.flags & FTS5_SEGITER_REVERSE) == 0 {
            pgno_save = pgno + 1;
        }
        cb.chunk(out, &leaf.p, seg.i_leaf_offset.max(0) as usize, n_chunk as i32);
        n_rem -= n_chunk;
        loop {
            if n_rem <= 0 {
                break;
            } else if seg.is_hash {
                return Err(self.corrupt_idx());
            }
            pgno += 1;
            let data = self.leaf_read(ctx, segment_rowid(seg.seg.i_segid, pgno))?;
            let n_chunk = n_rem.min(i64::from(data.sz_leaf) - 4);
            cb.chunk(out, &data.p, 4, n_chunk as i32);
            n_rem -= n_chunk;
            if pgno == pgno_save {
                seg.next_leaf = Some(data);
            }
        }
        Ok(())
    }

    /// `fts5SegiterPoslist`: appends the position list of the current entry (without its
    /// size field), filtered by `colset`.
    pub(super) fn segiter_poslist(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        seg: &mut SegIter,
        colset: Option<&[i32]>,
        buf: &mut Vec<u8>,
    ) -> Result<()> {
        match colset {
            None => self.chunk_iterate(ctx, seg, buf, &mut ChunkCb::Copy),
            Some(cs) => {
                if self.config.detail == Detail::Full {
                    let mut cb = ChunkCb::Filter {
                        colset: cs,
                        e_state: i32::from(colset_test(cs, 0)),
                    };
                    self.chunk_iterate(ctx, seg, buf, &mut cb)
                } else {
                    let mut cb = ChunkCb::Offsets {
                        colset: cs,
                        i_read: 0,
                        i_write: 0,
                    };
                    self.chunk_iterate(ctx, seg, buf, &mut cb)
                }
            }
        }
    }

    /// `fts5IterSetOutputCb`.
    pub(super) fn iter_set_output_cb(&self, mi: &mut MultiIter) {
        mi.x_set_outputs = if self.config.detail == Detail::None {
            SetOutputs::None
        } else if mi.colset.is_none() {
            SetOutputs::Nocolset
        } else if mi.colset.as_ref().is_some_and(Vec::is_empty) {
            SetOutputs::ZeroColset
        } else if self.config.detail == Detail::Full {
            SetOutputs::Full
        } else if self.config.n_col <= 100 {
            SetOutputs::Col100
        } else {
            SetOutputs::Col
        };
    }

    /// `pIter->xSetOutputs(pIter, pSeg)` for the segment iterator at `i_seg`.
    pub(super) fn set_outputs(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        i_seg: usize,
    ) -> Result<()> {
        match mi.x_set_outputs {
            SetOutputs::Noop => Ok(()),
            SetOutputs::None => {
                let s = &mi.a_seg[i_seg];
                mi.i_rowid = s.i_rowid;
                mi.n_data = s.n_pos;
                Ok(())
            }
            SetOutputs::ZeroColset => {
                mi.n_data = 0;
                mi.b_eof = true;
                Ok(())
            }
            SetOutputs::Nocolset => {
                let s = &mi.a_seg[i_seg];
                mi.i_rowid = s.i_rowid;
                mi.n_data = s.n_pos;
                let on_page = s
                    .leaf
                    .as_ref()
                    .is_some_and(|l| s.i_leaf_offset + i64::from(s.n_pos) <= i64::from(l.sz_leaf));
                if on_page {
                    // All data is stored on the current page. Populate the output
                    // variables to point into the body of the page object.
                    mi.data_src = DataSrc::Leaf(i_seg, s.i_leaf_offset.max(0) as usize);
                    Ok(())
                } else {
                    // The data is distributed over two or more pages. Copy it into the
                    // Fts5Iter.poslist buffer and then set the output pointer to point to
                    // this buffer.
                    let mut buf = std::mem::take(&mut mi.poslist);
                    buf.clear();
                    let mut seg = std::mem::take(&mut mi.a_seg[i_seg]);
                    let r = self.segiter_poslist(ctx, &mut seg, None, &mut buf);
                    mi.a_seg[i_seg] = seg;
                    mi.poslist = buf;
                    mi.data_src = DataSrc::Poslist(0);
                    r
                }
            }
            SetOutputs::Col => self.set_outputs_col(ctx, mi, i_seg),
            SetOutputs::Col100 => {
                let n_col = self.config.n_col as i64;
                let s = &mi.a_seg[i_seg];
                let Some(leaf) = s.leaf.as_ref() else {
                    return Ok(());
                };
                if s.i_leaf_offset + i64::from(s.n_pos) > i64::from(leaf.sz_leaf)
                    || i64::from(s.n_pos) > n_col
                {
                    return self.set_outputs_col(ctx, mi, i_seg);
                }
                let mut out = std::mem::take(&mut mi.poslist);
                out.clear();
                {
                    let a = &leaf.p;
                    let mut k = s.i_leaf_offset.max(0) as usize;
                    let end = k + s.n_pos.max(0) as usize;
                    let mut i_prev: i32 = 0;
                    let empty: Vec<i32> = Vec::new();
                    let ai_col = mi.colset.as_ref().unwrap_or(&empty);
                    let mut c = 0usize;
                    let mut i_prev_out: i32 = 0;
                    'outer: while k < end {
                        i_prev += i32::from(byte_at(a, k)) - 2;
                        k += 1;
                        while c < ai_col.len() && ai_col[c] < i_prev {
                            c += 1;
                            if c == ai_col.len() {
                                break 'outer;
                            }
                        }
                        if c < ai_col.len() && ai_col[c] == i_prev {
                            out.push(((i_prev - i_prev_out) + 2) as u8);
                            i_prev_out = i_prev;
                        }
                    }
                }
                mi.i_rowid = s.i_rowid;
                mi.n_data = out.len() as i32;
                mi.poslist = out;
                mi.data_src = DataSrc::Poslist(0);
                Ok(())
            }
            SetOutputs::Full => {
                let s = &mi.a_seg[i_seg];
                mi.i_rowid = s.i_rowid;
                let Some(leaf) = s.leaf.as_ref() else {
                    return Ok(());
                };
                let mut buf = std::mem::take(&mut mi.poslist);
                buf.clear();
                if s.i_leaf_offset + i64::from(s.n_pos) <= i64::from(leaf.sz_leaf) {
                    // All data is stored on the current page. Populate the output
                    // variables to point into the body of the page object.
                    let empty: Vec<i32> = Vec::new();
                    let cs = mi.colset.as_ref().unwrap_or(&empty);
                    let off = s.i_leaf_offset.max(0) as usize;
                    match index_extract_colset(cs, &leaf.p, off, s.n_pos.max(0) as usize, &mut buf)
                    {
                        Some((start, len)) => {
                            mi.data_src = DataSrc::Leaf(i_seg, start);
                            mi.n_data = len as i32;
                        }
                        None => {
                            mi.data_src = DataSrc::Poslist(0);
                            mi.n_data = buf.len() as i32;
                        }
                    }
                    mi.poslist = buf;
                    Ok(())
                } else {
                    // The data is distributed over two or more pages. Copy it into the
                    // Fts5Iter.poslist buffer and then set the output pointer to point to
                    // this buffer.
                    let cs = mi.colset.clone();
                    let mut seg = std::mem::take(&mut mi.a_seg[i_seg]);
                    let r = self.segiter_poslist(ctx, &mut seg, cs.as_deref(), &mut buf);
                    mi.a_seg[i_seg] = seg;
                    mi.n_data = buf.len() as i32;
                    mi.poslist = buf;
                    mi.data_src = DataSrc::Poslist(0);
                    r
                }
            }
        }
    }

    /// `fts5IterSetOutputs_Col`.
    fn set_outputs_col(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        i_seg: usize,
    ) -> Result<()> {
        let mut buf = std::mem::take(&mut mi.poslist);
        buf.clear();
        let cs = mi.colset.clone();
        let mut seg = std::mem::take(&mut mi.a_seg[i_seg]);
        let r = self.segiter_poslist(ctx, &mut seg, cs.as_deref(), &mut buf);
        mi.i_rowid = seg.i_rowid;
        mi.a_seg[i_seg] = seg;
        mi.n_data = buf.len() as i32;
        mi.poslist = buf;
        mi.data_src = DataSrc::Poslist(0);
        r
    }

    // -----------------------------------------------------------------------------------
    // Multi-iterator construction
    // -----------------------------------------------------------------------------------

    /// `fts5MultiIterNew`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn multi_iter_new(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        st: &Structure,
        flags: i32,
        colset: Option<&[i32]>,
        term: Option<&[u8]>,
        i_level: i32,
        n_segment: usize,
    ) -> Result<MultiIter> {
        let with_hash = self.hash.is_some() && (flags & FTS5INDEX_QUERY_SKIPHASH) == 0;
        let n_seg = if i_level < 0 {
            st.n_segment.max(0) as usize + usize::from(with_hash)
        } else {
            st.a_level[i_level as usize].a_seg.len().min(n_segment)
        };
        let mut mi = MultiIter::alloc(n_seg);
        mi.b_rev = (flags & FTS5INDEX_QUERY_DESC) != 0;
        mi.b_skip_empty = (flags & FTS5INDEX_QUERY_SKIPEMPTY) != 0;
        mi.colset = colset.map(<[i32]>::to_vec);
        if (flags & FTS5INDEX_QUERY_NOOUTPUT) == 0 {
            self.iter_set_output_cb(&mut mi);
        }
        // Initialize each of the component segment iterators.
        let mut i_iter = 0usize;
        if i_level < 0 {
            if with_hash {
                // Add a segment iterator for the current contents of the hash table.
                let mut it = SegIter::default();
                self.seg_iter_hash_init_q(term, flags, &mut it);
                mi.a_seg[i_iter] = it;
                i_iter += 1;
            }
            for (i_lvl, lvl) in st.a_level.iter().enumerate() {
                for i_seg in (0..lvl.a_seg.len()).rev() {
                    let mut it = SegIter::default();
                    match term {
                        None => self.seg_iter_init(ctx, (i_lvl, i_seg), &lvl.a_seg[i_seg], &mut it)?,
                        Some(t) => self.seg_iter_seek_init(
                            ctx,
                            t,
                            flags,
                            (i_lvl, i_seg),
                            &lvl.a_seg[i_seg],
                            &mut it,
                        )?,
                    }
                    if i_iter < mi.a_seg.len() {
                        mi.a_seg[i_iter] = it;
                    }
                    i_iter += 1;
                }
            }
        } else {
            let lvl = &st.a_level[i_level as usize];
            for i_seg in (0..n_seg).rev() {
                let mut it = SegIter::default();
                self.seg_iter_init(ctx, (i_level as usize, i_seg), &lvl.a_seg[i_seg], &mut it)?;
                mi.a_seg[i_iter] = it;
                i_iter += 1;
            }
        }
        // If the above was successful, each component iterator now points to the first
        // entry in its segment. In this case initialize the aFirst[] array.
        self.multi_iter_finish_setup(ctx, &mut mi)?;
        Ok(mi)
    }

    /// `fts5MultiIterNew2`: an iterator over the doclist `data`.
    fn multi_iter_new2(&mut self, data: Data, b_desc: bool) -> MultiIter {
        let mut mi = MultiIter::alloc(2);
        let it = &mut mi.a_seg[1];
        it.flags = FTS5_SEGITER_ONETERM;
        it.is_hash = true;
        if data.sz_leaf > 0 {
            let (v, nb) = get_varint(&data.p, 0);
            it.i_rowid = v as i64;
            it.i_leaf_offset = nb as i64;
            it.i_endof_doclist = data.nn;
            it.leaf = Some(data);
            mi.a_first[1].i_first = 1;
            if b_desc {
                mi.b_rev = true;
                let mut seg = std::mem::take(&mut mi.a_seg[1]);
                seg.flags |= FTS5_SEGITER_REVERSE;
                self.seg_iter_reverse_init_page(&mut seg);
                mi.a_seg[1] = seg;
            } else {
                let mut seg = std::mem::take(&mut mi.a_seg[1]);
                self.seg_iter_load_npos(&mut seg);
                mi.a_seg[1] = seg;
            }
        } else {
            mi.b_eof = true;
        }
        mi
    }

    /// `fts5MultiIterNext2`.
    fn multi_iter_next2(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        pb_new_term: &mut bool,
    ) -> Result<()> {
        *pb_new_term = false;
        loop {
            let i_first = mi.first();
            let mut b_new_term = false;
            let mut seg = std::mem::take(&mut mi.a_seg[i_first]);
            let r = self.seg_iter_next(ctx, &mut seg, Some(&mut b_new_term));
            let at_eof = seg.leaf.is_none();
            mi.a_seg[i_first] = seg;
            r?;
            if at_eof || b_new_term || mi.advance_rowid(i_first).0 {
                self.multi_iter_advanced(ctx, mi, i_first, 1)?;
                mi.set_eof();
                *pb_new_term = true;
            }
            let s = &mi.a_seg[mi.first()];
            let empty = s.leaf.is_some() && s.n_pos == 0;
            if !(empty || self.multi_iter_is_deleted(ctx, mi)?) {
                break;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// Doclist merging for prefix queries
// ---------------------------------------------------------------------------------------

/// `Fts5DoclistIter` over a buffer: `a_poslist` is the offset of the size field of the
/// current entry (`None` at EOF).
#[derive(Debug, Clone, Copy, Default)]
struct DoclistIter {
    a_eof: usize,
    i_rowid: i64,
    a_poslist: Option<usize>,
    n_poslist: i32,
    n_size: i32,
}

impl DoclistIter {
    /// `fts5DoclistIterInit`.
    fn init(buf: &[u8]) -> Self {
        let mut it = Self::default();
        if !buf.is_empty() {
            it.a_poslist = Some(0);
            it.a_eof = buf.len();
            it.next(buf);
        }
        it
    }
    /// `fts5DoclistIterNext`.
    fn next(&mut self, buf: &[u8]) {
        let Some(base) = self.a_poslist else {
            return;
        };
        let mut p = base + self.n_size.max(0) as usize + self.n_poslist.max(0) as usize;
        if p >= self.a_eof {
            self.a_poslist = None;
        } else {
            let (delta, nb) = get_varint(buf, p);
            p += nb;
            self.i_rowid = self.i_rowid.wrapping_add(delta as i64);
            // Read position list size
            if (byte_at(buf, p) & 0x80) != 0 {
                let (n_pos, nb) = get_varint32(buf, p);
                self.n_size = nb as i32;
                self.n_poslist = n_pos >> 1;
            } else {
                self.n_poslist = i32::from(byte_at(buf, p)) >> 1;
                self.n_size = 1;
            }
            self.a_poslist = Some(p);
            if p + self.n_poslist.max(0) as usize > self.a_eof {
                self.a_poslist = None;
            }
        }
    }
}

/// `fts5NextRowid`.
fn next_rowid(buf: &[u8], pi_off: &mut i32, pi_rowid: &mut i64) {
    let i = *pi_off;
    if i as usize >= buf.len() {
        *pi_off = -1;
    } else {
        let (v, nb) = get_varint(buf, i as usize);
        *pi_off = i + nb as i32;
        *pi_rowid = pi_rowid.wrapping_add(v as i64);
    }
}

/// `PrefixMerger`.
#[derive(Debug, Clone, Copy, Default)]
struct PrefixMerger {
    iter: DoclistIter,
    i_pos: i64,
    i_off: i32,
    a_pos: usize,
    next: Option<usize>,
}

impl Fts5Index {
    /// `fts5MergeRowidLists`: the detail=none equivalent of `fts5MergePrefixLists`.
    fn merge_rowid_lists(p1: &mut Vec<u8>, a_buf: &mut [Vec<u8>]) {
        let mut i1: i32 = 0;
        let mut i2: i32 = 0;
        let mut i_rowid1: i64 = 0;
        let mut i_rowid2: i64 = 0;
        let mut i_out: i64 = 0;
        let p2 = &a_buf[0];
        let mut out: Vec<u8> = Vec::with_capacity(p1.len() + p2.len() + 9);
        next_rowid(p1, &mut i1, &mut i_rowid1);
        next_rowid(p2, &mut i2, &mut i_rowid2);
        while i1 >= 0 || i2 >= 0 {
            if i1 >= 0 && (i2 < 0 || i_rowid1 < i_rowid2) {
                put_varint(&mut out, (i_rowid1 as u64).wrapping_sub(i_out as u64));
                i_out = i_rowid1;
                next_rowid(p1, &mut i1, &mut i_rowid1);
            } else {
                put_varint(&mut out, (i_rowid2 as u64).wrapping_sub(i_out as u64));
                i_out = i_rowid2;
                if i1 >= 0 && i_rowid1 == i_rowid2 {
                    next_rowid(p1, &mut i1, &mut i_rowid1);
                }
                next_rowid(p2, &mut i2, &mut i_rowid2);
            }
        }
        *p1 = out;
    }

    /// `fts5MergePrefixLists`: merges the `a_buf` doclists into `p1`.
    fn merge_prefix_lists(&self, p1: &mut Vec<u8>, a_buf: &mut [Vec<u8>]) -> Result<()> {
        let n_buf = a_buf.len();
        let mut a_merger = [PrefixMerger::default(); FTS5_MERGE_NLIST];
        let mut n_out: usize = 0;
        let mut out: Vec<u8> = Vec::new();
        let mut tmp: Vec<u8> = Vec::new();
        let mut i_last_rowid: i64 = 0;

        // The buffer merger `i` reads: a_buf[i] for i<n_buf, p1 for i==n_buf.
        fn src<'a>(i: usize, n_buf: usize, p1: &'a [u8], a_buf: &'a [Vec<u8>]) -> &'a [u8] {
            if i == n_buf { p1 } else { &a_buf[i] }
        }
        // fts5PrefixMergerInsertByRowid
        fn insert_by_rowid(a: &mut [PrefixMerger], head: &mut Option<usize>, p: usize) {
            if a[p].iter.a_poslist.is_some() {
                let rowid = a[p].iter.i_rowid;
                let mut prev: Option<usize> = None;
                let mut cur = *head;
                while let Some(c) = cur {
                    if rowid > a[c].iter.i_rowid {
                        prev = Some(c);
                        cur = a[c].next;
                    } else {
                        break;
                    }
                }
                a[p].next = cur;
                match prev {
                    Some(pv) => a[pv].next = Some(p),
                    None => *head = Some(p),
                }
            }
        }
        // fts5PrefixMergerInsertByPosition
        fn insert_by_position(a: &mut [PrefixMerger], head: &mut Option<usize>, p: usize) {
            if a[p].i_pos >= 0 {
                let pos = a[p].i_pos;
                let mut prev: Option<usize> = None;
                let mut cur = *head;
                while let Some(c) = cur {
                    if pos > a[c].i_pos {
                        prev = Some(c);
                        cur = a[c].next;
                    } else {
                        break;
                    }
                }
                a[p].next = cur;
                match prev {
                    Some(pv) => a[pv].next = Some(p),
                    None => *head = Some(p),
                }
            }
        }

        // Initialize a doclist-iterator for each input buffer. Arrange them in a
        // linked-list starting at pHead in ascending order of rowid. Avoid linking any
        // iterators already at EOF into the linked list at all.
        let mut head: Option<usize> = Some(n_buf);
        a_merger[n_buf].iter = DoclistIter::init(p1);
        for i in 0..n_buf {
            a_merger[i].iter = DoclistIter::init(&a_buf[i]);
            insert_by_rowid(&mut a_merger, &mut head, i);
            n_out += a_buf[i].len();
        }
        if n_out == 0 {
            return Ok(());
        }
        // (the head merger is linked even at EOF, exactly as in C: `pHead = &aMerger[nBuf]`)
        let mut result: Result<()> = Ok(());
        'outer: while let Some(h) = head {
            // fts5MergeAppendDocid
            let h_rowid = a_merger[h].iter.i_rowid;
            put_varint(&mut out, (h_rowid as u64).wrapping_sub(i_last_rowid as u64));
            i_last_rowid = h_rowid;

            let nxt = a_merger[h].next;
            if nxt.is_some_and(|nx| i_last_rowid == a_merger[nx].iter.i_rowid) {
                // Merge data from two or more poslists
                let mut i_prev: i64 = 0;
                let mut n_tmp: i64 = FTS5_DATA_ZERO_PADDING as i64;
                let mut n_merge = 0i64;
                let mut save = Some(h);
                head = None;
                while let Some(sv) = save {
                    if a_merger[sv].iter.i_rowid != i_last_rowid {
                        break;
                    }
                    let next = a_merger[sv].next;
                    a_merger[sv].i_off = 0;
                    a_merger[sv].i_pos = 0;
                    a_merger[sv].a_pos = a_merger[sv].iter.a_poslist.unwrap_or(0)
                        + a_merger[sv].iter.n_size.max(0) as usize;
                    {
                        let m = &mut a_merger[sv];
                        let b = src(sv, n_buf, p1, a_buf);
                        let sl = b.get(m.a_pos..).unwrap_or(&[]);
                        poslist_next64(sl, m.iter.n_poslist, &mut m.i_off, &mut m.i_pos);
                    }
                    n_tmp += i64::from(a_merger[sv].iter.n_poslist) + 10;
                    n_merge += 1;
                    insert_by_position(&mut a_merger, &mut head, sv);
                    save = next;
                }
                let Some(h2) = head else {
                    result = Err(self.corrupt_idx());
                    break 'outer;
                };
                if a_merger[h2].next.is_none() {
                    result = Err(self.corrupt_idx());
                    break 'outer;
                }
                tmp.clear();
                let mut this = h2;
                head = a_merger[this].next;
                poslist_safe_append(&mut tmp, &mut i_prev, a_merger[this].i_pos);
                {
                    let m = &mut a_merger[this];
                    let b = src(this, n_buf, p1, a_buf);
                    let sl = b.get(m.a_pos..).unwrap_or(&[]);
                    poslist_next64(sl, m.iter.n_poslist, &mut m.i_off, &mut m.i_pos);
                }
                insert_by_position(&mut a_merger, &mut head, this);
                loop {
                    let Some(hh) = head else {
                        result = Err(self.corrupt_idx());
                        break 'outer;
                    };
                    if a_merger[hh].next.is_none() {
                        break;
                    }
                    this = hh;
                    if a_merger[this].i_pos != i_prev {
                        poslist_safe_append(&mut tmp, &mut i_prev, a_merger[this].i_pos);
                    }
                    {
                        let m = &mut a_merger[this];
                        let b = src(this, n_buf, p1, a_buf);
                        let sl = b.get(m.a_pos..).unwrap_or(&[]);
                        poslist_next64(sl, m.iter.n_poslist, &mut m.i_off, &mut m.i_pos);
                    }
                    head = a_merger[this].next;
                    insert_by_position(&mut a_merger, &mut head, this);
                }
                let Some(hh) = head else {
                    result = Err(self.corrupt_idx());
                    break 'outer;
                };
                if a_merger[hh].i_pos != i_prev {
                    poslist_safe_append(&mut tmp, &mut i_prev, a_merger[hh].i_pos);
                }
                let n_tail = i64::from(a_merger[hh].iter.n_poslist) - i64::from(a_merger[hh].i_off);
                // WRITEPOSLISTSIZE
                if tmp.len() as i64 + n_tail > n_tmp - FTS5_DATA_ZERO_PADDING as i64 {
                    result = Err(self.corrupt_idx());
                    break 'outer;
                }
                let _ = n_merge;
                put_varint(&mut out, ((tmp.len() as i64 + n_tail) * 2) as u64);
                out.extend_from_slice(&tmp);
                if n_tail > 0 {
                    let b = src(hh, n_buf, p1, a_buf);
                    let st = a_merger[hh].a_pos + a_merger[hh].i_off.max(0) as usize;
                    append_range(&mut out, b, st, st + n_tail as usize);
                }
                head = save;
                for i in 0..=n_buf {
                    if a_merger[i].iter.a_poslist.is_some()
                        && a_merger[i].iter.i_rowid == i_last_rowid
                    {
                        let b = src(i, n_buf, p1, a_buf);
                        a_merger[i].iter.next(b);
                        insert_by_rowid(&mut a_merger, &mut head, i);
                    }
                }
            } else {
                // Copy poslist from pHead to output
                let this = h;
                let it = a_merger[this].iter;
                let b = src(this, n_buf, p1, a_buf);
                let st = it.a_poslist.unwrap_or(0);
                append_range(
                    &mut out,
                    b,
                    st,
                    st + it.n_poslist.max(0) as usize + it.n_size.max(0) as usize,
                );
                a_merger[this].iter.next(b);
                head = a_merger[this].next;
                insert_by_rowid(&mut a_merger, &mut head, this);
            }
        }
        *p1 = out;
        result
    }
}

const FTS5_DATA_ZERO_PADDING: usize = 8;

// ---------------------------------------------------------------------------------------
// Prefix and tokendata iterators
// ---------------------------------------------------------------------------------------

/// `PrefixSetupCtx` + `TokendataSetupCtx`.
#[derive(Default)]
struct PrefixSetupCtx {
    i_last_rowid: i64,
    n_merge: usize,
    a_buf: Vec<Vec<u8>>,
    doclist: Vec<u8>,
    /// `pTokendata`: the map being filled, the offset of the current term in `terms`
    /// and its size.
    p_t: Option<Box<TokenDataIter>>,
    i_term_off: i32,
    n_term_byte: i32,
    /// `prefixIterSetupCb` (false: only `prefixIterSetupTokendataCb`).
    b_doclist: bool,
}

/// `fts5TokendataMerge`.
fn tokendata_merge(a1: &[TokenDataMap], a2: &[TokenDataMap], out: &mut [TokenDataMap]) {
    let (n1, n2) = (a1.len(), a2.len());
    let mut i1 = 0;
    let mut i2 = 0;
    while i1 < n1 || i2 < n2 {
        let o = i1 + i2;
        if i2 >= n2
            || (i1 < n1
                && (a1[i1].i_rowid < a2[i2].i_rowid
                    || (a1[i1].i_rowid == a2[i2].i_rowid && a1[i1].i_pos <= a2[i2].i_pos)))
        {
            out[o] = a1[i1];
            i1 += 1;
        } else {
            out[o] = a2[i2];
            i2 += 1;
        }
    }
}

/// `fts5TokendataIterSortMap`.
fn tokendata_iter_sort_map(p_t: &mut TokenDataIter) {
    let n_map = p_t.a_map.len();
    let mut tmp = vec![TokenDataMap::default(); n_map];
    let mut in_map = true; // a1 == pT->aMap
    let mut n_half = 1usize;
    while n_half < n_map {
        {
            let (a1, a2): (&[TokenDataMap], &mut [TokenDataMap]) = if in_map {
                (&p_t.a_map, &mut tmp)
            } else {
                (&tmp, &mut p_t.a_map)
            };
            let mut i1 = 0usize;
            while i1 < n_map {
                let n1 = n_half.min(n_map - i1);
                let n2 = n_half.min(n_map - i1 - n1);
                tokendata_merge(
                    &a1[i1..i1 + n1],
                    &a1[i1 + n1..i1 + n1 + n2],
                    &mut a2[i1..i1 + n1 + n2],
                );
                i1 += n_half * 2;
            }
        }
        in_map = !in_map;
        n_half *= 2;
    }
    if !in_map {
        p_t.a_map.copy_from_slice(&tmp);
    }
}

/// `fts5IsTokendataPrefix`.
fn is_tokendata_prefix(buf: &[u8], token: &[u8]) -> bool {
    buf.len() >= token.len()
        && buf[..token.len()] == *token
        && (buf.len() == token.len() || buf[token.len()] == 0x00)
}

impl Fts5Index {
    /// `prefixIterSetupTokendataCb`.
    fn prefix_iter_setup_tokendata_cb(s: &mut PrefixSetupCtx, p1: &MultiIter, new_term: Option<&[u8]>) {
        let Some(p_t) = s.p_t.as_mut() else {
            return;
        };
        let mut i_pos_off: i32 = 0;
        let mut i_pos: i64 = 0;
        if let Some(t) = new_term {
            s.n_term_byte = t.len() as i32 - 1;
            s.i_term_off = p_t.terms.len() as i32;
            p_t.terms.extend_from_slice(t.get(1..).unwrap_or(&[]));
        }
        let data = p1.data();
        while !poslist_next64(data, p1.n_data, &mut i_pos_off, &mut i_pos) {
            p_t.a_map.push(TokenDataMap {
                i_rowid: p1.i_rowid,
                i_pos,
                i_iter: s.i_term_off,
                n_byte: s.n_term_byte,
            });
        }
    }

    /// `prefixIterSetupCb`.
    fn prefix_iter_setup_cb(
        &mut self,
        s: &mut PrefixSetupCtx,
        p1: &MultiIter,
        new_term: Option<&[u8]>,
    ) -> Result<()> {
        if s.b_doclist {
            let n_merge = s.n_merge;
            if p1.n_data > 0 {
                if p1.i_rowid <= s.i_last_rowid && !s.doclist.is_empty() {
                    let mut i = 0usize;
                    while !s.doclist.is_empty() {
                        let i1 = i * n_merge;
                        let mut i_store = i1;
                        while i_store < i1 + n_merge {
                            if s.a_buf[i_store].is_empty() {
                                std::mem::swap(&mut s.doclist, &mut s.a_buf[i_store]);
                                s.doclist.clear();
                                break;
                            }
                            i_store += 1;
                        }
                        if i_store == i1 + n_merge {
                            let mut dl = std::mem::take(&mut s.doclist);
                            let r = self.x_merge(&mut dl, &mut s.a_buf[i1..i1 + n_merge]);
                            s.doclist = dl;
                            r?;
                            for b in &mut s.a_buf[i1..i1 + n_merge] {
                                b.clear();
                            }
                        }
                        i += 1;
                    }
                    s.i_last_rowid = 0;
                }
                // xAppend
                let delta = (p1.i_rowid as u64).wrapping_sub(s.i_last_rowid as u64);
                if self.config.detail == Detail::None {
                    // fts5AppendRowid
                    put_varint(&mut s.doclist, delta);
                } else {
                    // fts5AppendPoslist
                    put_varint(&mut s.doclist, delta);
                    put_varint(&mut s.doclist, (p1.n_data as u64) * 2);
                    s.doclist.extend_from_slice(p1.data());
                }
                s.i_last_rowid = p1.i_rowid;
            }
        }
        if s.p_t.is_some() {
            Self::prefix_iter_setup_tokendata_cb(s, p1, new_term);
        }
        Ok(())
    }

    /// `PrefixSetupCtx.xMerge`.
    fn x_merge(&self, p1: &mut Vec<u8>, a_buf: &mut [Vec<u8>]) -> Result<()> {
        if self.config.detail == Detail::None {
            Self::merge_rowid_lists(p1, a_buf);
            Ok(())
        } else {
            self.merge_prefix_lists(p1, a_buf)
        }
    }

    /// `fts5VisitEntries`: visits the entries of term `token` (or, with `b_prefix`, of all
    /// terms that start with it) and hands each to the setup callback.
    fn visit_entries(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        colset: Option<&[i32]>,
        token: &[u8],
        b_prefix: bool,
        s: &mut PrefixSetupCtx,
    ) -> Result<()> {
        let flags = (if b_prefix { FTS5INDEX_QUERY_SCAN } else { 0 })
            | FTS5INDEX_QUERY_SKIPEMPTY
            | FTS5INDEX_QUERY_NOOUTPUT;
        let mut b_new_term = true;
        let st = self.structure_read_c(ctx)?.clone();
        let mut p1 = self.multi_iter_new(ctx, &st, flags, colset, Some(token), -1, 0)?;
        self.iter_set_output_cb(&mut p1);
        while !p1.b_eof {
            let i_first = p1.first();
            self.set_outputs(ctx, &mut p1, i_first)?;
            let mut new_term: Option<Vec<u8>> = None;
            if b_new_term {
                let t = &p1.a_seg[i_first].term;
                if t.len() < token.len() || t[..token.len()] != *token {
                    break;
                }
                new_term = Some(t.clone());
            }
            self.prefix_iter_setup_cb(s, &p1, new_term.as_deref())?;
            self.multi_iter_next2(ctx, &mut p1, &mut b_new_term)?;
        }
        Ok(())
    }

    /// `fts5SetupPrefixIter`.
    fn setup_prefix_iter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        b_desc: bool,
        i_idx: i32,
        token: &mut [u8],
        colset: Option<&[i32]>,
    ) -> Result<MultiIter> {
        let mut s = PrefixSetupCtx {
            n_merge: 1,
            b_doclist: true,
            ..PrefixSetupCtx::default()
        };
        let mut n_buf = 32usize;
        if i_idx == 0 && self.config.detail == Detail::Full && self.config.b_prefix_insttoken {
            s.p_t = Some(Box::default());
        }
        if self.config.detail != Detail::None {
            s.n_merge = FTS5_MERGE_NLIST - 1;
            n_buf = s.n_merge * 8; // Sufficient to merge (16^8)==(2^32) lists
        }
        s.a_buf = vec![Vec::new(); n_buf];
        sqliters_types::dtrace!(
            "TR|f5q|prefix|idx={} desc={} tok={}",
            i_idx,
            i32::from(b_desc),
            i32::from(s.p_t.is_some())
        );
        self.structure_read_c(ctx)?;

        // If iIdx is non-zero, then it is the number of a prefix-index for prefixes 1
        // character longer than the prefix being queried for. That index contains all the
        // doclists required, except for the one corresponding to the prefix itself. That
        // one is extracted from the main term index here.
        if i_idx != 0 {
            token[0] = FTS5_MAIN_PREFIX;
            self.visit_entries(ctx, colset, token, false, &mut s)?;
        }
        token[0] = FTS5_MAIN_PREFIX + i_idx as u8;
        self.visit_entries(ctx, colset, token, true, &mut s)?;

        let mut i = 0usize;
        while i < n_buf {
            let mut dl = std::mem::take(&mut s.doclist);
            let r = self.x_merge(&mut dl, &mut s.a_buf[i..i + s.n_merge]);
            s.doclist = dl;
            r?;
            for b in &mut s.a_buf[i..i + s.n_merge] {
                *b = Vec::new();
            }
            i += s.n_merge;
        }

        let mut data = Data::from_blob(std::mem::take(&mut s.doclist));
        data.sz_leaf = data.nn;
        let mut mi = self.multi_iter_new2(data, b_desc);
        if let Some(mut p_t) = s.p_t.take() {
            tokendata_iter_sort_map(&mut p_t);
            mi.token_data = Some(p_t);
        }
        Ok(mi)
    }

    /// `fts5IterSetOutputsTokendata`.
    fn iter_set_outputs_tokendata(&self, mi: &mut MultiIter) {
        let mut n_hit = 0;
        let mut i_rowid = i64::MIN;
        let mut i_min = 0usize;
        let detail = self.config.detail;
        mi.n_data = 0;
        mi.data_src = DataSrc::Null;
        let Some(p_t) = mi.token_data.as_mut() else {
            return;
        };
        for (ii, p) in p_t.ap_iter.iter().enumerate() {
            if !p.b_eof {
                if n_hit == 0 || p.i_rowid < i_rowid {
                    i_rowid = p.i_rowid;
                    n_hit = 1;
                    mi.data_src = DataSrc::Sub(ii);
                    mi.n_data = p.n_data;
                    i_min = ii;
                } else if p.i_rowid == i_rowid {
                    n_hit += 1;
                }
            }
        }
        if n_hit == 0 {
            mi.b_eof = true;
        } else {
            mi.b_eof = false;
            mi.i_rowid = i_rowid;
            if n_hit == 1 && detail == Detail::Full {
                p_t.a_map.push(TokenDataMap {
                    i_rowid,
                    i_pos: -1,
                    i_iter: i_min as i32,
                    n_byte: 0,
                });
            } else if n_hit > 1 && detail != Detail::None {
                let mut i_prev: i64 = 0;
                // Populate an iterator for each poslist that will be merged
                let mut readers: Vec<(usize, PoslistReader)> = Vec::new();
                for (ii, p) in p_t.ap_iter.iter().enumerate() {
                    if i_rowid == p.i_rowid {
                        readers.push((ii, PoslistReader::init(p.data(), p.n_data)));
                    }
                }
                mi.poslist.clear();
                loop {
                    let mut i_min_pos = i64::MAX;
                    // Find smallest position
                    let mut i_min_r = 0usize;
                    for (k, (_, r)) in readers.iter().enumerate() {
                        if !r.b_eof && r.i_pos < i_min_pos {
                            i_min_pos = r.i_pos;
                            i_min_r = k;
                        }
                    }
                    // If all readers were at EOF, break out of the loop.
                    if i_min_pos == i64::MAX {
                        break;
                    }
                    poslist_safe_append(&mut mi.poslist, &mut i_prev, i_min_pos);
                    let (ii, r) = &mut readers[i_min_r];
                    let sub = &p_t.ap_iter[*ii];
                    r.next(sub.data());
                    if detail == Detail::Full {
                        p_t.a_map.push(TokenDataMap {
                            i_rowid,
                            i_pos: i_min_pos,
                            i_iter: *ii as i32,
                            n_byte: 0,
                        });
                    }
                }
                mi.data_src = DataSrc::Poslist(0);
                mi.n_data = mi.poslist.len() as i32;
            }
        }
    }

    /// `fts5TokendataIterNext`.
    fn tokendata_iter_next(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        b_from: bool,
        i_from: i64,
    ) -> Result<()> {
        let cur_rowid = mi.i_rowid;
        if let Some(p_t) = mi.token_data.as_mut() {
            for p in &mut p_t.ap_iter {
                if !p.b_eof && (p.i_rowid == cur_rowid || (b_from && p.i_rowid < i_from)) {
                    self.multi_iter_next(ctx, p, b_from, i_from)?;
                    while b_from && !p.b_eof && p.i_rowid < i_from {
                        self.multi_iter_next(ctx, p, false, 0)?;
                    }
                }
            }
        }
        self.iter_set_outputs_tokendata(mi);
        Ok(())
    }

    /// `fts5SetupTokendataIter`: an iterator for a non-prefix query on a tokendata=1 table.
    fn setup_tokendata_iter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        token: &[u8],
        colset: Option<&[i32]>,
    ) -> Result<MultiIter> {
        let flags = FTS5INDEX_QUERY_SCANONETERM | FTS5INDEX_QUERY_SCAN;
        let mut p_set: Option<Box<TokenDataIter>> = None;
        let mut p_small: Option<Vec<u8>> = None;

        self.index_flush(ctx)?;
        let st = self.structure_read_c(ctx)?.clone();

        loop {
            let mut new = MultiIter::alloc(st.n_segment.max(0) as usize);
            let b_seek: Vec<u8> = match p_small.as_ref() {
                Some(sm) => {
                    let mut b = sm.clone();
                    b.push(0);
                    b
                }
                None => token.to_vec(),
            };
            let has_prev = p_set.as_ref().is_some_and(|s| !s.ap_iter.is_empty());
            let mut i_new = 0usize;
            for (i_lvl, lvl) in st.a_level.iter().enumerate() {
                for i_seg in (0..lvl.a_seg.len()).rev() {
                    let seg = &lvl.a_seg[i_seg];
                    let mut b_done = false;
                    let mut b_moved = false;
                    let mut new_it = SegIter::default();
                    let mut prev_tomb: Option<Arc<Mutex<TombstoneArray>>> = None;
                    if has_prev
                        && let Some(set) = p_set.as_mut()
                        && let Some(prev) = set.ap_iter.last_mut()
                        && let Some(prev_it) = prev.a_seg.get_mut(i_new)
                    {
                        let small = p_small.as_deref().unwrap_or(&[]);
                        if small != prev_it.term.as_slice() {
                            new_it = std::mem::take(prev_it);
                            b_done = true;
                            b_moved = true;
                        } else {
                            prev_tomb = prev_it.tomb.clone();
                            if prev_it
                                .leaf
                                .as_ref()
                                .is_some_and(|l| prev_it.i_endof_doclist > l.sz_leaf)
                            {
                                self.seg_iter_next_init(
                                    ctx,
                                    &b_seek[..b_seek.len() - 1],
                                    (i_lvl, i_seg),
                                    seg,
                                    &mut new_it,
                                )?;
                                b_done = true;
                            }
                        }
                    }
                    if !b_done {
                        self.seg_iter_seek_init(
                            ctx,
                            &b_seek,
                            flags,
                            (i_lvl, i_seg),
                            seg,
                            &mut new_it,
                        )?;
                    }
                    if has_prev {
                        if !b_moved && let Some(t) = prev_tomb {
                            new_it.tomb = Some(t);
                        }
                    } else {
                        Self::seg_iter_alloc_tombstone(&mut new_it);
                    }
                    if i_new < new.a_seg.len() {
                        new.a_seg[i_new] = new_it;
                    }
                    i_new += 1;
                }
            }
            // fts5TokendataSetTermIfEof(pPrev, pSmall)
            if let Some(set) = p_set.as_mut()
                && let Some(prev) = set.ap_iter.last_mut()
                && let Some(s0) = prev.a_seg.get_mut(0)
                && s0.leaf.is_none()
            {
                s0.term = p_small.clone().unwrap_or_default();
            }

            new.b_skip_empty = true;
            new.colset = colset.map(<[i32]>::to_vec);
            self.iter_set_output_cb(&mut new);

            // Loop through all segments in the new iterator. Find the smallest term that
            // any segment-iterator points to. Iterator pNew will be used for this term.
            // Also, set any iterator that points to a term that does not match
            // pToken/nToken to point to EOF
            p_small = None;
            for ii in 0..new.n_seg {
                let p_ii = &mut new.a_seg[ii];
                if !is_tokendata_prefix(&p_ii.term, token) {
                    p_ii.leaf = None;
                }
                if p_ii.leaf.is_some()
                    && p_small
                        .as_ref()
                        .is_none_or(|sm| sm.as_slice() > p_ii.term.as_slice())
                {
                    p_small = Some(p_ii.term.clone());
                }
            }

            // If pSmall is still NULL at this point, then the new iterator does not point
            // to any terms that match the query. So delete it and break out of the loop -
            // all required iterators have been collected.
            if p_small.is_none() {
                break;
            }
            // Append this iterator to the set and continue.
            p_set.get_or_insert_with(Box::default).ap_iter.push(new);
        }

        if let Some(set) = p_set.as_mut() {
            for it in &mut set.ap_iter {
                for seg in &mut it.a_seg {
                    seg.flags |= FTS5_SEGITER_ONETERM;
                }
                self.multi_iter_finish_setup(ctx, it)?;
            }
        }

        let mut ret = MultiIter::alloc(0);
        ret.n_seg = 0;
        ret.a_seg.clear();
        if p_set.is_some() {
            ret.token_data = p_set;
            self.iter_set_outputs_tokendata(&mut ret);
        } else {
            ret.b_eof = true;
        }
        Ok(ret)
    }

    // -----------------------------------------------------------------------------------
    // Public interface
    // -----------------------------------------------------------------------------------

    /// `fts5IndexDataVersion`: `PRAGMA data_version` of the database the table is in.
    pub(super) fn index_data_version(&mut self, ctx: &mut dyn VTabHostContext) -> Result<i64> {
        let rows = ctx.query_sql(&self.sql_data_version, &[])?;
        Ok(rows
            .first()
            .and_then(|r| r.first())
            .map_or(0, Value::to_integer_lossy))
    }

    /// `fts5StructureRead`: the cached structure, read (after noting the data version)
    /// when there is none.
    pub(crate) fn structure_read_c(
        &mut self,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<&mut Structure> {
        self.structure_read(ctx)
    }

    /// `sqlite3Fts5IndexReset`.
    pub(crate) fn reset(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if self.index_data_version(ctx)? != self.i_struct_version {
            sqliters_types::dtrace!("TR|f5q|struct|reset-invalidate");
            self.structure_invalidate();
        }
        Ok(())
    }

    /// `sqlite3Fts5IndexQuery`: opens an iterator over the rowids that match `token` (or
    /// the tokens it is a prefix of).
    pub(crate) fn query(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        token: &[u8],
        flags: i32,
        colset: Option<&[i32]>,
    ) -> Result<MultiIter> {
        let n_token = token.len();
        let mut buf: Vec<u8> = Vec::with_capacity(n_token + 1);
        buf.push(0);
        buf.extend_from_slice(token);
        let mut i_idx: i32 = 0; // Index to search
        let mut i_prefix_idx: i32 = 0; // +1 prefix index
        let mut b_tokendata = self.config.b_tokendata;
        let n_prefix = self.config.prefixes.len() as i32;

        // The NOTOKENDATA flag is set when each token in a tokendata=1 table should be
        // treated individually, instead of merging all those with a common prefix into a
        // single entry. This is used, for example, by queries performed as part of an
        // integrity-check, or by the fts5vocab module.
        if (flags & (FTS5INDEX_QUERY_NOTOKENDATA | FTS5INDEX_QUERY_SCAN)) != 0 {
            b_tokendata = false;
        }

        // Figure out which index to search and set iIdx accordingly. If this is a prefix
        // query for which there is no prefix index, set iIdx to greater than
        // pConfig->nPrefix to indicate that the query will be satisfied by scanning
        // multiple terms in the main index.
        if (flags & FTS5INDEX_QUERY_PREFIX) != 0 {
            let n_char = index_charlen(token) as usize;
            i_idx = 1;
            while i_idx <= n_prefix {
                let n_idx_char = self.config.prefixes[(i_idx - 1) as usize];
                if n_idx_char == n_char {
                    break;
                }
                if n_idx_char == n_char + 1 {
                    i_prefix_idx = i_idx;
                }
                i_idx += 1;
            }
        }
        sqliters_types::dtrace!(
            "TR|f5q|query|n={} flags={} idx={} pidx={} td={}",
            n_token,
            flags,
            i_idx,
            i_prefix_idx,
            i32::from(b_tokendata)
        );

        if b_tokendata && i_idx == 0 {
            buf[0] = FTS5_MAIN_PREFIX;
            self.setup_tokendata_iter(ctx, &buf, colset)
        } else if i_idx <= n_prefix {
            // Straight index lookup
            let st = self.structure_read_c(ctx)?.clone();
            buf[0] = FTS5_MAIN_PREFIX + i_idx as u8;
            self.multi_iter_new(
                ctx,
                &st,
                flags | FTS5INDEX_QUERY_SKIPEMPTY,
                colset,
                Some(&buf),
                -1,
                0,
            )
        } else {
            // Scan multiple terms in the main index for a prefix query.
            let b_desc = (flags & FTS5INDEX_QUERY_DESC) != 0;
            let mut ret = self.setup_prefix_iter(ctx, b_desc, i_prefix_idx, &mut buf, colset)?;
            self.iter_set_output_cb(&mut ret);
            let i_first = ret.first();
            if ret.a_seg[i_first].leaf.is_some() {
                self.set_outputs(ctx, &mut ret, i_first)?;
            }
            Ok(ret)
        }
    }

    /// `sqlite3Fts5IterNext`.
    pub(crate) fn iter_next(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
    ) -> Result<()> {
        if mi.n_seg == 0 {
            self.tokendata_iter_next(ctx, mi, false, 0)
        } else {
            self.multi_iter_next(ctx, mi, false, 0)
        }
    }

    /// `sqlite3Fts5IterNextScan` (fts5vocab).
    pub(crate) fn iter_next_scan(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
    ) -> Result<()> {
        self.multi_iter_next(ctx, mi, false, 0)?;
        let i_first = mi.first();
        let seg = &mut mi.a_seg[i_first];
        if seg.leaf.is_some() && seg.term.first().copied() != Some(FTS5_MAIN_PREFIX) {
            seg.leaf = None;
            mi.b_eof = true;
        }
        Ok(())
    }

    /// `sqlite3Fts5IterNextFrom`.
    pub(crate) fn iter_next_from(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        i_match: i64,
    ) -> Result<()> {
        if mi.n_seg == 0 {
            self.tokendata_iter_next(ctx, mi, true, i_match)
        } else {
            self.multi_iter_next_from(ctx, mi, i_match)
        }
    }

    /// `fts5SetupPrefixIterTokendata` (xInstToken side).
    #[allow(dead_code)]
    fn setup_prefix_iter_tokendata(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        token: &[u8],
    ) -> Result<()> {
        let mut s = PrefixSetupCtx {
            p_t: Some(Box::default()),
            ..PrefixSetupCtx::default()
        };
        let mut tok: Vec<u8> = Vec::with_capacity(token.len() + 1);
        tok.push(FTS5_MAIN_PREFIX);
        tok.extend_from_slice(token);
        self.visit_entries(ctx, None, &tok, true, &mut s)?;
        if let Some(mut p_t) = s.p_t.take() {
            tokendata_iter_sort_map(&mut p_t);
            mi.token_data = Some(p_t);
        }
        Ok(())
    }

    /// `sqlite3Fts5IterToken`: the token at offset `i_off` of column `i_col` of row
    /// `i_rowid` (xInstToken).
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn iter_token(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        mi: &mut MultiIter,
        token: &[u8],
        i_rowid: i64,
        i_col: i32,
        i_off: i32,
    ) -> Result<Option<Vec<u8>>> {
        let i_pos = (i64::from(i_col) << 32) + i64::from(i_off);
        if mi.token_data.is_none() {
            self.setup_prefix_iter_tokendata(ctx, mi, token)?;
        }
        let Some(p_t) = mi.token_data.as_ref() else {
            return Ok(None);
        };
        let a_map = &p_t.a_map;
        let mut i1: usize = 0;
        let mut i2: usize = a_map.len();
        let mut i_test = 0usize;
        while i2 > i1 {
            i_test = (i1 + i2) / 2;
            if a_map[i_test].i_rowid < i_rowid {
                i1 = i_test + 1;
            } else if a_map[i_test].i_rowid > i_rowid {
                i2 = i_test;
            } else if a_map[i_test].i_pos < i_pos {
                if a_map[i_test].i_pos < 0 {
                    break;
                }
                i1 = i_test + 1;
            } else if a_map[i_test].i_pos > i_pos {
                i2 = i_test;
            } else {
                break;
            }
        }
        if i2 > i1 {
            if mi.n_seg == 0 {
                let sub = p_t.ap_iter.get(a_map[i_test].i_iter.max(0) as usize);
                let term = sub.and_then(|s| s.a_seg.first()).map(|s| s.term.as_slice());
                return Ok(term.map(|t| t.get(1..).unwrap_or(&[]).to_vec()));
            }
            let m = &a_map[i_test];
            let st = m.i_iter.max(0) as usize;
            let en = (st + m.n_byte.max(0) as usize).min(p_t.terms.len());
            return Ok(Some(p_t.terms.get(st..en).unwrap_or(&[]).to_vec()));
        }
        Ok(None)
    }

    /// `sqlite3Fts5IndexIterClearTokendata`.
    pub(crate) fn iter_clear_tokendata(&self, mi: &mut MultiIter) {
        let n_seg = mi.n_seg;
        if let Some(p_t) = mi.token_data.as_mut()
            && (n_seg == 0 || self.config.detail != Detail::Full)
        {
            p_t.a_map.clear();
        }
    }

    /// `sqlite3Fts5IndexIterWriteTokendata`.
    pub(crate) fn iter_write_tokendata(
        &self,
        mi: &mut MultiIter,
        token: &[u8],
        i_rowid: i64,
        i_col: i32,
        i_off: i32,
    ) {
        let i_pos = (i64::from(i_col) << 32) + i64::from(i_off);
        if mi.n_seg > 0 {
            // This is a prefix term iterator.
            let p_t = mi.token_data.get_or_insert_with(Box::default);
            p_t.a_map.push(TokenDataMap {
                i_rowid,
                i_pos,
                i_iter: p_t.terms.len() as i32,
                n_byte: token.len() as i32,
            });
            p_t.terms.extend_from_slice(token);
        } else if let Some(p_t) = mi.token_data.as_mut() {
            let found = p_t.ap_iter.iter().position(|it| {
                it.a_seg
                    .first()
                    .is_some_and(|s| s.term.get(1..).is_some_and(|t| t == token))
            });
            if let Some(ii) = found {
                p_t.a_map.push(TokenDataMap {
                    i_rowid,
                    i_pos,
                    i_iter: ii as i32,
                    n_byte: 0,
                });
            }
        }
    }
}

impl MultiIter {
    /// `sqlite3Fts5IterTerm`: the current term without its index-id byte.
    pub(crate) fn term(&self) -> &[u8] {
        let t = &self.a_seg[self.first()].term;
        t.get(1..).unwrap_or(&[])
    }
    /// `sqlite3Fts5IterEof`.
    #[inline]
    pub(crate) fn eof(&self) -> bool {
        self.b_eof
    }
}

/// `fts5IndexCharlen`: the number of characters of a UTF-8 buffer.
fn index_charlen(p: &[u8]) -> i32 {
    let mut n_char = 0;
    let mut i = 0usize;
    while i < p.len() {
        let b = p[i];
        i += 1;
        if b >= 0xc0 {
            while i < p.len() && (p[i] & 0xc0) == 0x80 {
                i += 1;
            }
        }
        n_char += 1;
    }
    n_char
}
