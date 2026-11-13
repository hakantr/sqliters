//! R-Tree and R*-Tree spatial index virtual table modules (`rtree` and `rtree_i32`)
//! and companion SQL functions (`rtreedepth`, `rtreenode`, `rtreecheck`).
//!
//! A port of SQLite 3.54.1 `sqlite/ext/rtree/rtree.c`: the same tree algorithm
//! (`ChooseLeaf`, `AdjustTree`, `SplitNode` / `splitNodeStartree`, `deleteCell` /
//! `removeNode` / `reinsertNodeContent`), the same node objects (reference counted, written
//! to `<name>_node` when the last reference is released, never cached between calls) and the
//! same shadow-table statements in the same order, so that the three shadow tables
//! (`<name>_node`, `<name>_rowid`, `<name>_parent`), the connection counters and the order
//! of query results are those of C.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use sqliters_types::{
    ErrorCode, Result, ShadowBlobRead, ShadowHint, SqliteError, VTabConfigOption, VTabConflictMode,
    VTabConstraintOp, VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value,
    VirtualCursor, VirtualTable, VirtualTableModule, cmp_int_float,
};

const RTREE_MAX_DIMENSIONS: usize = 5;
const RTREE_MAX_COORDS: usize = RTREE_MAX_DIMENSIONS * 2;
const RTREE_MAX_AUX_COLUMN: usize = 100;
const RTREE_MAXCELLS: usize = 51;
const RTREE_MAX_DEPTH: usize = 40;
const RTREE_DEFAULT_ROWEST: i64 = 1_048_576;
const RTREE_MIN_ROWEST: i64 = 100;
const RTREE_CHECK_MAX_ERROR: usize = 100;
/// Number of entries in the cursor RtreeNode cache.
const RTREE_CACHE_SZ: usize = 5;
/// The null node pointer.
const NIL: usize = usize::MAX;

const SQLITE_CORRUPT_VTAB: i32 = 11 | (1 << 8);
const SQLITE_LOCKED_VTAB: i32 = 6 | (2 << 8);

// Possible values for RtreeConstraint.op
const RTREE_EQ: u8 = 0x41;
const RTREE_LE: u8 = 0x42;
const RTREE_LT: u8 = 0x43;
const RTREE_GE: u8 = 0x44;
const RTREE_GT: u8 = 0x45;
const RTREE_MATCH: u8 = 0x46;
const RTREE_TRUE: u8 = 0x3f;
const RTREE_FALSE: u8 = 0x40;

// Values for eWithin
const PARTLY_WITHIN: u8 = 1;
const FULLY_WITHIN: u8 = 2;

/// `SQLITE_CORRUPT_VTAB`, with the text `sqlite3ErrStr` gives for it.
#[cold]
#[inline(never)]
fn corrupt_vtab() -> SqliteError {
    SqliteError::with_extended(SQLITE_CORRUPT_VTAB, "database disk image is malformed")
}

/// `SQLITE_LOCKED_VTAB` (`rtreeUpdate` while a cursor holds nodes).
#[cold]
#[inline(never)]
fn locked_vtab() -> SqliteError {
    SqliteError::with_extended(SQLITE_LOCKED_VTAB, "database table is locked")
}

/// Coordinate representation for an R-Tree module (`f32` vs `i32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RTreeCoordMode {
    Float32,
    Int32,
}

/// `struct RtreeCell`: a deserialized cell. `c[i]` holds the bits of `RtreeCoord` number `i`
/// (a C union of `float f`, `int i` and `u32 u`).
#[derive(Debug, Clone, Copy)]
struct RtreeCell {
    rowid: i64,
    c: [u32; RTREE_MAX_COORDS],
}

impl RtreeCell {
    #[inline]
    const fn zero() -> Self {
        Self {
            rowid: 0,
            c: [0; RTREE_MAX_COORDS],
        }
    }

    /// `aCoord[i].f`
    #[inline(always)]
    fn f(&self, i: usize) -> f32 {
        f32::from_bits(self.c[i])
    }

    /// `aCoord[i].i`
    #[inline(always)]
    fn i(&self, i: usize) -> i32 {
        self.c[i] as i32
    }
}

/// `struct RtreeNode`. `parent` is a slot of [`Rtree::nodes`] or [`NIL`]. A node on the
/// `pDeleted` list keeps the height of its sub-tree in `i_node`, as in C.
#[derive(Debug)]
struct RtreeNode {
    parent: usize,
    i_node: i64,
    n_ref: i32,
    dirty: bool,
    data: Vec<u8>,
}

/// `NCELL(pNode)`
#[inline(always)]
fn ncell(data: &[u8]) -> usize {
    ((data[2] as usize) << 8) + data[3] as usize
}

/// `readInt16`
#[inline(always)]
fn read_int16(p: &[u8]) -> i32 {
    ((p[0] as i32) << 8) + p[1] as i32
}

/// `writeInt16`
#[inline(always)]
fn write_int16(p: &mut [u8], i: i32) {
    p[0] = ((i >> 8) & 0xFF) as u8;
    p[1] = (i & 0xFF) as u8;
}

#[inline(always)]
fn read_coord(p: &[u8]) -> u32 {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]])
}

/// FNV-1a over a node image; only the decision trace prints it.
#[allow(dead_code)]
fn trace_hash(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h
}

/// `struct RtreeSearchPoint`
#[derive(Debug, Clone, Copy)]
struct RtreeSearchPoint {
    r_score: f64,
    id: i64,
    i_level: u8,
    e_within: u8,
    i_cell: u8,
}

impl RtreeSearchPoint {
    const fn zero() -> Self {
        Self {
            r_score: 0.0,
            id: 0,
            i_level: 0,
            e_within: 0,
            i_cell: 0,
        }
    }
}

/// `struct RtreeConstraint` (coordinate constraints only: the engine has no
/// `sqlite3_rtree_geometry_callback` / `sqlite3_rtree_query_callback`).
#[derive(Debug, Clone, Copy)]
struct RtreeConstraint {
    i_coord: usize,
    op: u8,
    r_value: f64,
}

/// The search state of `struct RtreeCursor`.
#[derive(Debug)]
struct RtreeSearch {
    at_eof: bool,
    b_point: bool,
    a_constraint: Vec<RtreeConstraint>,
    a_point: Vec<RtreeSearchPoint>,
    s_point: RtreeSearchPoint,
    a_node: [usize; RTREE_CACHE_SZ],
}

impl RtreeSearch {
    fn new() -> Self {
        Self {
            at_eof: false,
            b_point: false,
            a_constraint: Vec::new(),
            a_point: Vec::new(),
            s_point: RtreeSearchPoint::zero(),
            a_node: [NIL; RTREE_CACHE_SZ],
        }
    }
}

/// Where `rtreeSearchPointNew` put the new point.
#[derive(Clone, Copy)]
enum PointLoc {
    SPoint,
    APoint(usize),
}

/// `struct Rtree`: the virtual-table object.
#[derive(Debug)]
struct Rtree {
    db_name: String,
    name: String,
    node_tbl: String,
    rowid_tbl: String,
    parent_tbl: String,
    node_hint: ShadowHint,
    rowid_hint: ShadowHint,
    parent_hint: ShadowHint,
    coord_int: bool,
    n_dim: usize,
    n_dim2: usize,
    n_aux: usize,
    bytes_per_cell: usize,
    node_size: usize,
    #[allow(dead_code)]
    in_wr_trans: bool,
    /// `iDepth`
    depth: i32,
    /// `nNodeRef`: number of nodes with a positive reference count.
    n_node_ref: u32,
    /// Node objects (`sqlite3_malloc64(sizeof(RtreeNode)+iNodeSize)` in C); `free` lists the
    /// slots that are not in use, whose buffers are reused.
    nodes: Vec<RtreeNode>,
    free: Vec<usize>,
    /// `aHash`: the nodes that have a node number, `(iNode, slot)`. At most a path of the
    /// tree plus the nodes of a split are in memory at a time, so a short list is searched.
    hash: Vec<(i64, usize)>,
    /// `pDeleted`: stack of removed nodes (C links them at the head of a list).
    deleted: Vec<usize>,
    col_names: Vec<String>,
    /// `pWriteRowid` when there are auxiliary columns (an UPSERT).
    write_rowid_upsert_sql: String,
    /// `pWriteAux`
    write_aux_sql: String,
    /// Buffers of the last finished scan, kept for the next one.
    search_pool: Option<RtreeSearch>,
}

impl Rtree {
    // ------------------------------------------------------------------------------------
    // Node objects
    // ------------------------------------------------------------------------------------

    fn alloc_slot(&mut self) -> usize {
        if let Some(idx) = self.free.pop() {
            return idx;
        }
        self.nodes.push(RtreeNode {
            parent: NIL,
            i_node: 0,
            n_ref: 0,
            dirty: false,
            data: Vec::with_capacity(self.node_size),
        });
        self.nodes.len() - 1
    }

    #[inline]
    fn free_slot(&mut self, idx: usize) {
        let n = &mut self.nodes[idx];
        n.parent = NIL;
        n.i_node = 0;
        n.n_ref = 0;
        n.dirty = false;
        self.free.push(idx);
    }

    /// Drops every in-memory node (after an error left references behind; C would keep them
    /// until the table is disconnected).
    fn reset_nodes(&mut self) {
        self.hash.clear();
        self.deleted.clear();
        self.free.clear();
        for i in 0..self.nodes.len() {
            let n = &mut self.nodes[i];
            n.parent = NIL;
            n.i_node = 0;
            n.n_ref = 0;
            n.dirty = false;
            self.free.push(i);
        }
        self.n_node_ref = 0;
        self.depth = -1;
    }

    /// `nodeReference`
    #[inline(always)]
    fn node_reference(&mut self, p: usize) {
        if p != NIL {
            self.nodes[p].n_ref += 1;
        }
    }

    /// `nodeHashLookup`
    #[inline(always)]
    fn node_hash_lookup(&self, i_node: i64) -> usize {
        for &(k, idx) in &self.hash {
            if k == i_node {
                return idx;
            }
        }
        NIL
    }

    /// `nodeHashInsert`
    #[inline(always)]
    fn node_hash_insert(&mut self, idx: usize) {
        self.hash.push((self.nodes[idx].i_node, idx));
    }

    /// `nodeHashDelete`
    #[inline(always)]
    fn node_hash_delete(&mut self, idx: usize) {
        if self.nodes[idx].i_node != 0
            && let Some(pos) = self.hash.iter().position(|e| e.1 == idx)
        {
            self.hash.swap_remove(pos);
        }
    }

    /// `nodeNew`
    fn node_new(&mut self, parent: usize) -> usize {
        let idx = self.alloc_slot();
        let node_size = self.node_size;
        let n = &mut self.nodes[idx];
        n.data.clear();
        n.data.resize(node_size, 0);
        n.i_node = 0;
        n.n_ref = 1;
        n.parent = parent;
        n.dirty = true;
        self.n_node_ref += 1;
        self.node_reference(parent);
        idx
    }

    /// `nodeAcquire`
    fn node_acquire(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_node: i64,
        parent: usize,
    ) -> Result<usize> {
        // Check if the requested node is already in the hash table.
        let found = self.node_hash_lookup(i_node);
        if found != NIL {
            if parent != NIL && parent != self.nodes[found].parent {
                return Err(corrupt_vtab());
            }
            self.nodes[found].n_ref += 1;
            return Ok(found);
        }

        let idx = self.alloc_slot();
        let node_size = self.node_size;
        let read = {
            let n = &mut self.nodes[idx];
            if n.data.len() != node_size {
                n.data.resize(node_size, 0);
            }
            ctx.shadow_blob_read(&self.node_tbl, &mut self.node_hint, i_node, 1, &mut n.data)
        };
        // sqlite3_blob_open failing with SQLITE_ERROR "can only be because the shadow tables
        // hold erroneous data"; iNode<=0 and a blob of another size are corrupt as well.
        match read {
            Err(e) => {
                self.free_slot(idx);
                return Err(e);
            }
            Ok(ShadowBlobRead::Size(n)) if i_node > 0 && n == node_size => {}
            Ok(_) => {
                self.free_slot(idx);
                return Err(corrupt_vtab());
            }
        }
        {
            let n = &mut self.nodes[idx];
            n.parent = parent;
            n.n_ref = 1;
            n.i_node = i_node;
            n.dirty = false;
        }
        self.n_node_ref += 1;

        // If the root node was just loaded, set pRtree->iDepth to the height of the r-tree
        // structure.
        let mut ok = true;
        if i_node == 1 {
            self.depth = read_int16(&self.nodes[idx].data);
            if self.depth >= RTREE_MAX_DEPTH as i32 {
                ok = false;
            }
        }
        // The "number of entries" field must not be too large.
        if ok && ncell(&self.nodes[idx].data) > (self.node_size - 4) / self.bytes_per_cell {
            ok = false;
        }
        if ok {
            self.node_reference(parent);
            self.node_hash_insert(idx);
            Ok(idx)
        } else {
            self.n_node_ref -= 1;
            self.free_slot(idx);
            Err(corrupt_vtab())
        }
    }

    /// `nodeGetRowid`
    #[inline(always)]
    fn node_get_rowid(&self, node: usize, i_cell: usize) -> i64 {
        let off = 4 + self.bytes_per_cell * i_cell;
        let d = &self.nodes[node].data[off..off + 8];
        i64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
    }

    /// `nodeGetCell`
    #[inline(always)]
    fn node_get_cell(&self, node: usize, i_cell: usize) -> RtreeCell {
        let off = 4 + self.bytes_per_cell * i_cell;
        let d = &self.nodes[node].data[off..off + self.bytes_per_cell];
        let mut cell = RtreeCell::zero();
        cell.rowid = i64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]);
        for (slot, ch) in cell.c[..self.n_dim2]
            .iter_mut()
            .zip(d[8..].as_chunks::<4>().0)
        {
            *slot = u32::from_be_bytes(*ch);
        }
        cell
    }

    /// `nodeOverwriteCell`
    #[inline(always)]
    fn node_overwrite_cell(&mut self, node: usize, cell: &RtreeCell, i_cell: usize) {
        let off = 4 + self.bytes_per_cell * i_cell;
        let n_dim2 = self.n_dim2;
        let n = &mut self.nodes[node];
        let d = &mut n.data[off..off + 8 + 4 * n_dim2];
        d[..8].copy_from_slice(&cell.rowid.to_be_bytes());
        for (ch, v) in d[8..]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&cell.c[..n_dim2])
        {
            *ch = v.to_be_bytes();
        }
        n.dirty = true;
    }

    /// `nodeDeleteCell`: the cells after `i_cell` move down; the bytes of the former last cell
    /// stay where they were.
    fn node_delete_cell(&mut self, node: usize, i_cell: usize) {
        let bpc = self.bytes_per_cell;
        let n = &mut self.nodes[node];
        let n_cell = ncell(&n.data);
        let dst = 4 + bpc * i_cell;
        let n_byte = (n_cell - i_cell - 1) * bpc;
        n.data.copy_within(dst + bpc..dst + bpc + n_byte, dst);
        write_int16(&mut n.data[2..4], n_cell as i32 - 1);
        n.dirty = true;
    }

    /// `nodeInsertCell`: returns true ("SQLITE_FULL") when there is no room.
    #[inline(always)]
    fn node_insert_cell(&mut self, node: usize, cell: &RtreeCell) -> bool {
        let n_max_cell = (self.node_size - 4) / self.bytes_per_cell;
        let n_cell = ncell(&self.nodes[node].data);
        if n_cell < n_max_cell {
            self.node_overwrite_cell(node, cell, n_cell);
            let n = &mut self.nodes[node];
            write_int16(&mut n.data[2..4], n_cell as i32 + 1);
            n.dirty = true;
        }
        n_cell == n_max_cell
    }

    /// `nodeWrite`
    fn node_write(&mut self, ctx: &mut dyn VTabHostContext, node: usize) -> Result<()> {
        if !self.nodes[node].dirty {
            return Ok(());
        }
        let n = &mut self.nodes[node];
        let key = if n.i_node != 0 { Some(n.i_node) } else { None };
        n.dirty = false;
        let written = ctx.shadow_replace_blob(&self.node_tbl, &mut self.node_hint, key, &n.data)?;
        sqliters_types::dtrace!(
            "TR|rt|w|tbl=node k={} v={:08x}",
            written,
            trace_hash(&n.data)
        );
        if n.i_node == 0 {
            n.i_node = written;
            if written == 0 {
                // "the shadow table schema has been corrupted somehow"
                return Err(corrupt_vtab());
            }
            sqliters_types::dtrace!("TR|rt|nodeno|new={}", written);
            self.node_hash_insert(node);
        }
        Ok(())
    }

    /// `nodeRelease`
    fn node_release(&mut self, ctx: &mut dyn VTabHostContext, node: usize) -> Result<()> {
        if node == NIL {
            return Ok(());
        }
        let n = &mut self.nodes[node];
        debug_assert!(n.n_ref > 0);
        n.n_ref -= 1;
        if n.n_ref != 0 {
            return Ok(());
        }
        self.n_node_ref -= 1;
        if n.i_node == 1 {
            self.depth = -1;
        }
        let parent = n.parent;
        let mut rc = Ok(());
        if parent != NIL {
            rc = self.node_release(ctx, parent);
        }
        if rc.is_ok() {
            rc = self.node_write(ctx, node);
        }
        self.node_hash_delete(node);
        self.free_slot(node);
        rc
    }

    /// `nodeRowidIndex`
    fn node_rowid_index(&self, node: usize, rowid: i64) -> Result<usize> {
        let n_cell = ncell(&self.nodes[node].data);
        for ii in 0..n_cell {
            if self.node_get_rowid(node, ii) == rowid {
                return Ok(ii);
            }
        }
        Err(corrupt_vtab())
    }

    /// `nodeParentIndex` (the node has a parent).
    #[inline]
    fn node_parent_index(&self, node: usize) -> Result<usize> {
        let n = &self.nodes[node];
        if n.parent == NIL {
            return Err(corrupt_vtab());
        }
        self.node_rowid_index(n.parent, n.i_node)
    }

    // ------------------------------------------------------------------------------------
    // Cell arithmetic
    // ------------------------------------------------------------------------------------

    /// `cellArea`: for REAL32 each side is a `float` difference; the product is a `double`.
    #[inline(always)]
    fn cell_area(&self, p: &RtreeCell) -> f64 {
        let mut area: f64 = 1.0;
        if self.coord_int {
            let n = self.n_dim;
            if n >= 5 {
                area = (i64::from(p.i(9)) - i64::from(p.i(8))) as f64;
            }
            if n >= 4 {
                area *= (i64::from(p.i(7)) - i64::from(p.i(6))) as f64;
            }
            if n >= 3 {
                area *= (i64::from(p.i(5)) - i64::from(p.i(4))) as f64;
            }
            if n >= 2 {
                area *= (i64::from(p.i(3)) - i64::from(p.i(2))) as f64;
            }
            area *= (i64::from(p.i(1)) - i64::from(p.i(0))) as f64;
        } else {
            let n = self.n_dim;
            if n >= 5 {
                area = f64::from(p.f(9) - p.f(8));
            }
            if n >= 4 {
                area *= f64::from(p.f(7) - p.f(6));
            }
            if n >= 3 {
                area *= f64::from(p.f(5) - p.f(4));
            }
            if n >= 2 {
                area *= f64::from(p.f(3) - p.f(2));
            }
            area *= f64::from(p.f(1) - p.f(0));
        }
        area
    }

    /// `DCOORD`
    #[inline(always)]
    fn dcoord(&self, p: &RtreeCell, i: usize) -> f64 {
        if self.coord_int {
            f64::from(p.i(i))
        } else {
            f64::from(p.f(i))
        }
    }

    /// `cellMargin`
    #[inline]
    fn cell_margin(&self, p: &RtreeCell) -> f64 {
        let mut margin: f64 = 0.0;
        let mut ii = self.n_dim2 as isize - 2;
        loop {
            let i = ii as usize;
            margin += self.dcoord(p, i + 1) - self.dcoord(p, i);
            ii -= 2;
            if ii < 0 {
                break;
            }
        }
        margin
    }

    /// `cellUnion`. In the amalgamation (the build of the reference library) `MIN` and `MAX`
    /// are the macros of sqliteInt.h — `MIN(A,B) ((A)<(B)?(A):(B))`, `MAX(A,B)
    /// ((A)>(B)?(A):(B))` — because rtree.c only defines its own when there are none: on a
    /// tie the SECOND operand is taken, which is visible for -0.0 against +0.0.
    // The comparisons are written as in C so that a NaN takes the same branch.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    #[inline(always)]
    fn cell_union(&self, p1: &mut RtreeCell, p2: &RtreeCell) {
        let mut ii = 0;
        if self.coord_int {
            loop {
                if !(p1.i(ii) < p2.i(ii)) {
                    p1.c[ii] = p2.c[ii];
                }
                if !(p1.i(ii + 1) > p2.i(ii + 1)) {
                    p1.c[ii + 1] = p2.c[ii + 1];
                }
                ii += 2;
                if ii >= self.n_dim2 {
                    break;
                }
            }
        } else {
            loop {
                if !(p1.f(ii) < p2.f(ii)) {
                    p1.c[ii] = p2.c[ii];
                }
                if !(p1.f(ii + 1) > p2.f(ii + 1)) {
                    p1.c[ii + 1] = p2.c[ii + 1];
                }
                ii += 2;
                if ii >= self.n_dim2 {
                    break;
                }
            }
        }
    }

    /// `cellContains`: true if `p2` is a subset of `p1`.
    #[inline(always)]
    fn cell_contains(&self, p1: &RtreeCell, p2: &RtreeCell) -> bool {
        let mut ii = 0;
        if self.coord_int {
            while ii < self.n_dim2 {
                if p2.i(ii) < p1.i(ii) || p2.i(ii + 1) > p1.i(ii + 1) {
                    return false;
                }
                ii += 2;
            }
        } else {
            while ii < self.n_dim2 {
                if p2.f(ii) < p1.f(ii) || p2.f(ii + 1) > p1.f(ii + 1) {
                    return false;
                }
                ii += 2;
            }
        }
        true
    }

    /// `cellContains(nodeGetCell(node, i_cell), p2)` without deserializing the whole cell:
    /// the coordinates are read pair by pair and the first pair that fails ends the test.
    #[inline(always)]
    fn node_cell_contains(&self, node: usize, i_cell: usize, p2: &RtreeCell) -> bool {
        let off = 12 + self.bytes_per_cell * i_cell;
        let d = &self.nodes[node].data[off..off + 4 * self.n_dim2];
        let pairs = d.as_chunks::<8>().0;
        if self.coord_int {
            for (ii, pr) in pairs.iter().enumerate() {
                let lo = i32::from_be_bytes([pr[0], pr[1], pr[2], pr[3]]);
                let hi = i32::from_be_bytes([pr[4], pr[5], pr[6], pr[7]]);
                if p2.i(2 * ii) < lo || p2.i(2 * ii + 1) > hi {
                    return false;
                }
            }
        } else {
            for (ii, pr) in pairs.iter().enumerate() {
                let lo = f32::from_bits(u32::from_be_bytes([pr[0], pr[1], pr[2], pr[3]]));
                let hi = f32::from_bits(u32::from_be_bytes([pr[4], pr[5], pr[6], pr[7]]));
                if p2.f(2 * ii) < lo || p2.f(2 * ii + 1) > hi {
                    return false;
                }
            }
        }
        true
    }

    /// `cellOverlap(pRtree, p, aCell, 1)` (the only call passes one cell).
    #[inline]
    fn cell_overlap(&self, p: &RtreeCell, other: &RtreeCell) -> f64 {
        let mut overlap: f64 = 0.0;
        let mut o: f64 = 1.0;
        let mut jj = 0;
        while jj < self.n_dim2 {
            // x1 = MAX(a, b), x2 = MIN(a2, b2) with the macros of sqliteInt.h
            let a = self.dcoord(p, jj);
            let b = self.dcoord(other, jj);
            let x1 = if a > b { a } else { b };
            let a2 = self.dcoord(p, jj + 1);
            let b2 = self.dcoord(other, jj + 1);
            let x2 = if a2 < b2 { a2 } else { b2 };
            if x2 < x1 {
                o = 0.0;
                break;
            }
            o *= x2 - x1;
            jj += 2;
        }
        overlap += o;
        overlap
    }

    // ------------------------------------------------------------------------------------
    // Insertion
    // ------------------------------------------------------------------------------------

    /// `ChooseLeaf` (ChooseSubTree in R*-tree terminology).
    fn choose_leaf(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        cell: &RtreeCell,
        i_height: i32,
    ) -> Result<usize> {
        let mut node = self.node_acquire(ctx, 1, NIL)?;
        let mut ii = 0;
        while ii < self.depth - i_height {
            let mut i_best: i64 = 0;
            let mut found = false;
            let mut f_min_growth: f64 = 0.0;
            let mut f_min_area: f64 = 0.0;
            let n_cell = ncell(&self.nodes[node].data);

            // First check to see if there are any cells in pNode that completely contain
            // pCell. If two or more do, pick the smallest.
            for i_cell in 0..n_cell {
                if !self.node_cell_contains(node, i_cell, cell) {
                    continue;
                }
                let c = self.node_get_cell(node, i_cell);
                let area = self.cell_area(&c);
                if !found || area < f_min_area {
                    i_best = c.rowid;
                    f_min_area = area;
                    found = true;
                }
            }
            if !found {
                // No cell of pNode completely contains pCell: pick the cell that grows by the
                // least amount when pCell is added. Break ties by selecting the smaller cell.
                for i_cell in 0..n_cell {
                    let mut c = self.node_get_cell(node, i_cell);
                    let area = self.cell_area(&c);
                    self.cell_union(&mut c, cell);
                    let growth = self.cell_area(&c) - area;
                    if i_cell == 0
                        || growth < f_min_growth
                        || (growth == f_min_growth && area < f_min_area)
                    {
                        f_min_growth = growth;
                        f_min_area = area;
                        i_best = c.rowid;
                    }
                }
            }
            sqliters_types::dtrace!(
                "TR|rt|choose|h={} lvl={} node={} ncell={} best={} o={}",
                i_height,
                ii,
                self.nodes[node].i_node,
                n_cell,
                i_best,
                if found { "contain" } else { "grow" }
            );

            let child = self.node_acquire(ctx, i_best, node);
            let _ = self.node_release(ctx, node);
            node = child?;
            ii += 1;
        }
        Ok(node)
    }

    /// `AdjustTree`: a cell with the content of `cell` has just been inserted into `node`;
    /// update the bounding boxes in all ancestors.
    fn adjust_tree(&mut self, node: usize, cell: &RtreeCell) -> Result<()> {
        let mut p = node;
        let mut cnt = 0;
        while self.nodes[p].parent != NIL {
            let parent = self.nodes[p].parent;
            cnt += 1;
            if cnt > 100 {
                return Err(corrupt_vtab());
            }
            let Ok(i_cell) = self.node_parent_index(p) else {
                return Err(corrupt_vtab());
            };
            let mut c = self.node_get_cell(parent, i_cell);
            if !self.cell_contains(&c, cell) {
                self.cell_union(&mut c, cell);
                self.node_overwrite_cell(parent, &c, i_cell);
                sqliters_types::dtrace!(
                    "TR|rt|adjust|node={} cell={} o=rewrite",
                    self.nodes[parent].i_node,
                    i_cell
                );
            } else {
                sqliters_types::dtrace!(
                    "TR|rt|adjust|node={} cell={} o=keep",
                    self.nodes[parent].i_node,
                    i_cell
                );
            }
            p = parent;
        }
        Ok(())
    }

    /// `rowidWrite`
    fn rowid_write(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        i_node: i64,
    ) -> Result<()> {
        sqliters_types::dtrace!("TR|rt|w|tbl=rowid k={} v={}", rowid, i_node);
        if self.n_aux == 0 {
            ctx.shadow_replace_int(
                &self.rowid_tbl,
                &mut self.rowid_hint,
                Some(rowid),
                Some(i_node),
            )?;
        } else {
            ctx.execute_sql_counted(
                &self.write_rowid_upsert_sql,
                &[Value::Integer(rowid), Value::Integer(i_node)],
            )?;
        }
        Ok(())
    }

    /// `parentWrite`
    fn parent_write(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_node: i64,
        i_par: i64,
    ) -> Result<()> {
        sqliters_types::dtrace!("TR|rt|w|tbl=parent k={} v={}", i_node, i_par);
        ctx.shadow_replace_int(
            &self.parent_tbl,
            &mut self.parent_hint,
            Some(i_node),
            Some(i_par),
        )?;
        Ok(())
    }

    /// `SortByDimension`: merge sort of `a_idx` by the minimum of dimension `i_dim`, the
    /// maximum breaking ties. On a full tie the merge takes the right run first.
    fn sort_by_dimension(
        &self,
        a_idx: &mut [usize],
        i_dim: usize,
        a_cell: &[RtreeCell],
        a_spare: &mut [usize],
    ) {
        let n_idx = a_idx.len();
        if n_idx > 1 {
            let n_left = n_idx / 2;
            let n_right = n_idx - n_left;
            {
                let (l, r) = a_idx.split_at_mut(n_left);
                self.sort_by_dimension(l, i_dim, a_cell, a_spare);
                self.sort_by_dimension(r, i_dim, a_cell, a_spare);
            }
            a_spare[..n_left].copy_from_slice(&a_idx[..n_left]);
            let mut i_left = 0;
            let mut i_right = 0;
            while i_left < n_left || i_right < n_right {
                let take_left = if i_left == n_left {
                    false
                } else if i_right == n_right {
                    true
                } else {
                    let lc = &a_cell[a_spare[i_left]];
                    let rc = &a_cell[a_idx[n_left + i_right]];
                    let xleft1 = self.dcoord(lc, i_dim * 2);
                    let xleft2 = self.dcoord(lc, i_dim * 2 + 1);
                    let xright1 = self.dcoord(rc, i_dim * 2);
                    let xright2 = self.dcoord(rc, i_dim * 2 + 1);
                    xleft1 < xright1 || (xleft1 == xright1 && xleft2 < xright2)
                };
                if take_left {
                    a_idx[i_left + i_right] = a_spare[i_left];
                    i_left += 1;
                } else {
                    a_idx[i_left + i_right] = a_idx[n_left + i_right];
                    i_right += 1;
                }
            }
        }
    }

    /// `splitNodeStartree`: the R*-tree variant of SplitNode from Beckman[1990].
    fn split_node_startree(
        &mut self,
        a_cell: &[RtreeCell],
        left: usize,
        right: usize,
    ) -> (RtreeCell, RtreeCell) {
        let n_cell = a_cell.len();
        let n_dim = self.n_dim;
        let min_cells = self.min_cells();
        let mut sorted = [[0usize; RTREE_MAXCELLS + 1]; RTREE_MAX_DIMENSIONS];
        let mut spare = [0usize; RTREE_MAXCELLS + 1];

        let mut i_best_dim = 0;
        let mut i_best_split = 0;
        let mut f_best_margin: f64 = 0.0;

        for (ii, row) in sorted.iter_mut().enumerate().take(n_dim) {
            for (jj, slot) in row[..n_cell].iter_mut().enumerate() {
                *slot = jj;
            }
            self.sort_by_dimension(&mut row[..n_cell], ii, a_cell, &mut spare);
        }

        for (ii, row) in sorted.iter().enumerate().take(n_dim) {
            let mut margin: f64 = 0.0;
            let mut f_best_overlap: f64 = 0.0;
            let mut f_best_area: f64 = 0.0;
            let mut i_best_left = 0;

            // C rebuilds the two boxes of every candidate distribution from scratch (`left` =
            // cells 0..nLeft in sorted order, `right` = the last cell and cells nLeft..nCell-1).
            // The same boxes come out of running unions: `pre[k]` covers the sorted cells
            // 0..=k, `suf[k]` the cells k..nCell-1. (Which of -0.0 / +0.0 a box edge carries
            // can differ from C's order of unions; margin, overlap and area — all that is
            // compared here — do not depend on it. The boxes that are stored are built below
            // in C's order.)
            let mut pre = [RtreeCell::zero(); RTREE_MAXCELLS + 1];
            let mut suf = [RtreeCell::zero(); RTREE_MAXCELLS + 1];
            pre[0] = a_cell[row[0]];
            for kk in 1..n_cell {
                let mut b = pre[kk - 1];
                self.cell_union(&mut b, &a_cell[row[kk]]);
                pre[kk] = b;
            }
            suf[n_cell - 1] = a_cell[row[n_cell - 1]];
            for kk in (0..n_cell - 1).rev() {
                let mut b = suf[kk + 1];
                self.cell_union(&mut b, &a_cell[row[kk]]);
                suf[kk] = b;
            }

            let mut n_left = min_cells;
            while n_left + min_cells <= n_cell {
                let l = pre[n_left - 1];
                let r = suf[n_left];
                margin += self.cell_margin(&l);
                margin += self.cell_margin(&r);
                let overlap = self.cell_overlap(&l, &r);
                let area = self.cell_area(&l) + self.cell_area(&r);
                if n_left == min_cells
                    || overlap < f_best_overlap
                    || (overlap == f_best_overlap && area < f_best_area)
                {
                    i_best_left = n_left;
                    f_best_overlap = overlap;
                    f_best_area = area;
                }
                n_left += 1;
            }

            if ii == 0 || margin < f_best_margin {
                i_best_dim = ii;
                f_best_margin = margin;
                i_best_split = i_best_left;
            }
        }
        sqliters_types::dtrace!(
            "TR|rt|splitdist|ncell={} dim={} nleft={}",
            n_cell,
            i_best_dim,
            i_best_split
        );

        let row = &sorted[i_best_dim];
        let mut bbox_left = a_cell[row[0]];
        let mut bbox_right = a_cell[row[i_best_split]];
        for (ii, &ci) in row[..n_cell].iter().enumerate() {
            let c = &a_cell[ci];
            if ii < i_best_split {
                self.node_insert_cell(left, c);
                self.cell_union(&mut bbox_left, c);
            } else {
                self.node_insert_cell(right, c);
                self.cell_union(&mut bbox_right, c);
            }
        }
        (bbox_left, bbox_right)
    }

    /// `RTREE_MINCELLS`
    #[inline(always)]
    fn min_cells(&self) -> usize {
        ((self.node_size - 4) / self.bytes_per_cell) / 3
    }

    /// `updateMapping`
    fn update_mapping(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        node: usize,
        i_height: i32,
    ) -> Result<()> {
        if i_height > 0 {
            let child = self.node_hash_lookup(rowid);
            let mut p = node;
            while p != NIL {
                if p == child {
                    return Err(corrupt_vtab());
                }
                p = self.nodes[p].parent;
            }
            if child != NIL {
                let old_parent = self.nodes[child].parent;
                let _ = self.node_release(ctx, old_parent);
                self.node_reference(node);
                self.nodes[child].parent = node;
            }
        }
        let i_node = self.nodes[node].i_node;
        if i_height == 0 {
            self.rowid_write(ctx, rowid, i_node)
        } else {
            self.parent_write(ctx, rowid, i_node)
        }
    }

    /// `SplitNode`
    fn split_node(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        node: usize,
        cell: &RtreeCell,
        i_height: i32,
    ) -> Result<()> {
        let mut n_cell = ncell(&self.nodes[node].data);
        // A copy of pCell and of all cells of the node; then the node is zeroed.
        let mut a_cell = [RtreeCell::zero(); RTREE_MAXCELLS + 1];
        for (i, slot) in a_cell.iter_mut().enumerate().take(n_cell) {
            *slot = self.node_get_cell(node, i);
        }
        // nodeZero
        {
            let n = &mut self.nodes[node];
            n.data[2..].fill(0);
            n.dirty = true;
        }
        a_cell[n_cell] = *cell;
        n_cell += 1;

        let is_root = self.nodes[node].i_node == 1;
        let (left, right);
        if is_root {
            right = self.node_new(node);
            left = self.node_new(node);
            self.depth += 1;
            let depth = self.depth;
            let n = &mut self.nodes[node];
            n.dirty = true;
            write_int16(&mut n.data[0..2], depth);
            sqliters_types::dtrace!("TR|rt|depth|o=grow d={}", depth);
        } else {
            left = node;
            let parent = self.nodes[left].parent;
            right = self.node_new(parent);
            self.nodes[left].n_ref += 1;
        }
        self.nodes[left].data.fill(0);
        self.nodes[right].data.fill(0);

        let res = self.split_node_tail(ctx, node, left, right, &a_cell[..n_cell], cell, i_height);
        let _ = self.node_release(ctx, right);
        let _ = self.node_release(ctx, left);
        res
    }

    /// The part of `SplitNode` between the allocation of the two nodes and `splitnode_out`.
    #[allow(clippy::too_many_arguments)]
    fn split_node_tail(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        node: usize,
        left: usize,
        right: usize,
        a_cell: &[RtreeCell],
        cell: &RtreeCell,
        i_height: i32,
    ) -> Result<()> {
        let is_root = self.nodes[node].i_node == 1;
        let (mut leftbbox, mut rightbbox) = self.split_node_startree(a_cell, left, right);

        // Ensure both child nodes have node numbers assigned to them by calling nodeWrite().
        // Node pRight always needs a node number; pLeft sometimes already has one.
        self.node_write(ctx, right)?;
        if self.nodes[left].i_node == 0 {
            self.node_write(ctx, left)?;
        }
        rightbbox.rowid = self.nodes[right].i_node;
        leftbbox.rowid = self.nodes[left].i_node;
        sqliters_types::dtrace!(
            "TR|rt|split|node={} h={} o={} left={} nl={} right={} nr={}",
            self.nodes[node].i_node,
            i_height,
            if is_root { "root" } else { "nonroot" },
            leftbbox.rowid,
            ncell(&self.nodes[left].data),
            rightbbox.rowid,
            ncell(&self.nodes[right].data)
        );

        if is_root {
            let parent = self.nodes[left].parent;
            self.rtree_insert_cell(ctx, parent, &leftbbox, i_height + 1)?;
        } else {
            let parent = self.nodes[left].parent;
            let i_cell = self.node_parent_index(left)?;
            self.node_overwrite_cell(parent, &leftbbox, i_cell);
            self.adjust_tree(parent, &leftbbox)?;
        }
        let right_parent = self.nodes[right].parent;
        self.rtree_insert_cell(ctx, right_parent, &rightbbox, i_height + 1)?;

        let mut new_cell_is_right = false;
        let nr = ncell(&self.nodes[right].data);
        for i in 0..nr {
            let rowid = self.node_get_rowid(right, i);
            let rc = self.update_mapping(ctx, rowid, right, i_height);
            if rowid == cell.rowid {
                new_cell_is_right = true;
            }
            rc?;
        }
        if is_root {
            let nl = ncell(&self.nodes[left].data);
            for i in 0..nl {
                let rowid = self.node_get_rowid(left, i);
                self.update_mapping(ctx, rowid, left, i_height)?;
            }
        } else if !new_cell_is_right {
            self.update_mapping(ctx, cell.rowid, left, i_height)?;
        }
        Ok(())
    }

    /// `rtreeInsertCell`: insert `cell` into `node`, the head of a sub-tree `i_height` high.
    fn rtree_insert_cell(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        node: usize,
        cell: &RtreeCell,
        i_height: i32,
    ) -> Result<()> {
        if i_height > 0 {
            let child = self.node_hash_lookup(cell.rowid);
            if child != NIL {
                let old_parent = self.nodes[child].parent;
                let _ = self.node_release(ctx, old_parent);
                self.node_reference(node);
                self.nodes[child].parent = node;
            }
        }
        if self.node_insert_cell(node, cell) {
            sqliters_types::dtrace!(
                "TR|rt|ins|node={} h={} id={} o=split",
                self.nodes[node].i_node,
                i_height,
                cell.rowid
            );
            self.split_node(ctx, node, cell, i_height)
        } else {
            sqliters_types::dtrace!(
                "TR|rt|ins|node={} h={} id={} o=fit",
                self.nodes[node].i_node,
                i_height,
                cell.rowid
            );
            self.adjust_tree(node, cell)?;
            let i_node = self.nodes[node].i_node;
            if i_height == 0 {
                self.rowid_write(ctx, cell.rowid, i_node)
            } else {
                self.parent_write(ctx, cell.rowid, i_node)
            }
        }
    }

    // ------------------------------------------------------------------------------------
    // Deletion
    // ------------------------------------------------------------------------------------

    /// `fixLeafParent`: load the ancestors of `leaf` and populate the `pParent` chain up to
    /// the root.
    fn fix_leaf_parent(&mut self, ctx: &mut dyn VTabHostContext, leaf: usize) -> Result<()> {
        let mut child = leaf;
        while self.nodes[child].i_node != 1 && self.nodes[child].parent == NIL {
            let i_child = self.nodes[child].i_node;
            let row = ctx.shadow_int_read(&self.parent_tbl, &mut self.parent_hint, i_child, 1)?;
            if let Some(i_node) = row {
                // Before setting pChild->pParent, test that we are not creating a loop of
                // references.
                let mut test = leaf;
                while test != NIL && self.nodes[test].i_node != i_node {
                    test = self.nodes[test].parent;
                }
                if test == NIL {
                    let parent = self.node_acquire(ctx, i_node, NIL)?;
                    self.nodes[child].parent = parent;
                }
            }
            if self.nodes[child].parent == NIL {
                return Err(corrupt_vtab());
            }
            child = self.nodes[child].parent;
        }
        Ok(())
    }

    /// `removeNode`
    fn remove_node(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        node: usize,
        i_height: i32,
    ) -> Result<()> {
        debug_assert!(self.nodes[node].n_ref == 1);
        // Remove the entry in the parent cell.
        let mut parent = NIL;
        let mut rc = match self.node_parent_index(node) {
            Ok(i_cell) => {
                parent = self.nodes[node].parent;
                self.nodes[node].parent = NIL;
                self.delete_cell(ctx, parent, i_cell, i_height + 1)
            }
            Err(e) => Err(e),
        };
        let rc2 = self.node_release(ctx, parent);
        if rc.is_ok() {
            rc = rc2;
        }
        rc?;

        let i_node = self.nodes[node].i_node;
        // Remove the xxx_node entry.
        sqliters_types::dtrace!("TR|rt|d|tbl=node k={}", i_node);
        ctx.shadow_delete_counted(&self.node_tbl, &mut self.node_hint, i_node)?;
        // Remove the xxx_parent entry.
        sqliters_types::dtrace!("TR|rt|d|tbl=parent k={}", i_node);
        ctx.shadow_delete_counted(&self.parent_tbl, &mut self.parent_hint, i_node)?;

        // Remove the node from the in-memory hash table and link it into the
        // Rtree.pDeleted list. Its contents will be re-inserted later on.
        self.node_hash_delete(node);
        self.nodes[node].i_node = i64::from(i_height);
        self.nodes[node].n_ref += 1;
        self.deleted.push(node);
        Ok(())
    }

    /// `fixBoundingBox`
    fn fix_bounding_box(&mut self, node: usize) -> Result<()> {
        let mut node = node;
        loop {
            let parent = self.nodes[node].parent;
            if parent == NIL {
                return Ok(());
            }
            let n_cell = ncell(&self.nodes[node].data);
            let mut bx = self.node_get_cell(node, 0);
            for ii in 1..n_cell {
                let c = self.node_get_cell(node, ii);
                self.cell_union(&mut bx, &c);
            }
            bx.rowid = self.nodes[node].i_node;
            let ii = self.node_parent_index(node)?;
            self.node_overwrite_cell(parent, &bx, ii);
            node = parent;
        }
    }

    /// `deleteCell`: delete the cell at index `i_cell` of `node`, then adjust the tree.
    fn delete_cell(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        node: usize,
        i_cell: usize,
        i_height: i32,
    ) -> Result<()> {
        self.fix_leaf_parent(ctx, node)?;

        // Remove the cell from the node. This call just moves bytes around the in-memory
        // node image, so it cannot fail.
        self.node_delete_cell(node, i_cell);

        // If the node is not the tree root and now has less than the minimum number of
        // cells, remove it from the tree. Otherwise, update the cell in the parent node so
        // that it tightly contains the updated node.
        if self.nodes[node].parent != NIL {
            if ncell(&self.nodes[node].data) < self.min_cells() {
                sqliters_types::dtrace!(
                    "TR|rt|delcell|node={} h={} ncell={} o=remove",
                    self.nodes[node].i_node,
                    i_height,
                    ncell(&self.nodes[node].data)
                );
                self.remove_node(ctx, node, i_height)
            } else {
                sqliters_types::dtrace!(
                    "TR|rt|delcell|node={} h={} ncell={} o=fix",
                    self.nodes[node].i_node,
                    i_height,
                    ncell(&self.nodes[node].data)
                );
                self.fix_bounding_box(node)
            }
        } else {
            sqliters_types::dtrace!(
                "TR|rt|delcell|node={} h={} ncell={} o=root",
                self.nodes[node].i_node,
                i_height,
                ncell(&self.nodes[node].data)
            );
            Ok(())
        }
    }

    /// `reinsertNodeContent`
    fn reinsert_node_content(&mut self, ctx: &mut dyn VTabHostContext, node: usize) -> Result<()> {
        let n_cell = ncell(&self.nodes[node].data);
        let height = self.nodes[node].i_node as i32;
        for ii in 0..n_cell {
            let cell = self.node_get_cell(node, ii);
            sqliters_types::dtrace!("TR|rt|reins|h={} id={}", height, cell.rowid);
            // Find a node to store this cell in. pNode->iNode currently contains the height
            // of the sub-tree headed by the cell.
            let insert = self.choose_leaf(ctx, &cell, height)?;
            let rc = self.rtree_insert_cell(ctx, insert, &cell, height);
            let rc2 = self.node_release(ctx, insert);
            rc?;
            rc2?;
        }
        Ok(())
    }

    /// `rtreeNewRowid`: select a currently unused rowid for a new r-tree record.
    fn rtree_new_rowid(&mut self, ctx: &mut dyn VTabHostContext) -> Result<i64> {
        let rowid = if self.n_aux == 0 {
            ctx.shadow_replace_int(&self.rowid_tbl, &mut self.rowid_hint, None, None)?
        } else {
            ctx.execute_sql_counted(&self.write_rowid_upsert_sql, &[Value::Null, Value::Null])?;
            let rows = ctx.query_sql("SELECT last_insert_rowid()", &[])?;
            rows.first()
                .and_then(|r| r.first())
                .and_then(Value::as_i64)
                .unwrap_or(0)
        };
        sqliters_types::dtrace!("TR|rt|newrowid|id={}", rowid);
        Ok(rowid)
    }

    /// `findLeafNode`: the leaf holding `rowid` and its node number, or `None`.
    fn find_leaf_node(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<(usize, i64)>> {
        match ctx.shadow_int_read(&self.rowid_tbl, &mut self.rowid_hint, rowid, 1)? {
            Some(i_node) => {
                let leaf = self.node_acquire(ctx, i_node, NIL)?;
                Ok(Some((leaf, i_node)))
            }
            None => Ok(None),
        }
    }

    /// `rtreeDeleteRowid`: remove the entry with rowid `i_delete` from the r-tree structure.
    fn rtree_delete_rowid(&mut self, ctx: &mut dyn VTabHostContext, i_delete: i64) -> Result<()> {
        // Obtain a reference to the root node to initialize Rtree.iDepth.
        let root = self.node_acquire(ctx, 1, NIL)?;
        let mut rc: Result<()> = Ok(());

        // Obtain a reference to the leaf node that contains the entry about to be deleted.
        let mut leaf = NIL;
        match self.find_leaf_node(ctx, i_delete) {
            Ok(Some((l, _))) => leaf = l,
            Ok(None) => {}
            Err(e) => rc = Err(e),
        }

        // Delete the cell in question from the leaf node.
        if rc.is_ok() && leaf != NIL {
            rc = match self.node_rowid_index(leaf, i_delete) {
                Ok(i_cell) => self.delete_cell(ctx, leaf, i_cell, 0),
                Err(e) => Err(e),
            };
            let rc2 = self.node_release(ctx, leaf);
            if rc.is_ok() {
                rc = rc2;
            }
        }

        // Delete the corresponding entry in the <rtree>_rowid table.
        if rc.is_ok() {
            sqliters_types::dtrace!("TR|rt|d|tbl=rowid k={}", i_delete);
            rc = ctx.shadow_delete_counted(&self.rowid_tbl, &mut self.rowid_hint, i_delete);
        }

        // Check if the root node now has exactly one child. If so, remove it, schedule the
        // contents of the child for reinsertion and reduce the tree height by one.
        if rc.is_ok() && self.depth > 0 && ncell(&self.nodes[root].data) == 1 {
            let i_child = self.node_get_rowid(root, 0);
            let mut child = NIL;
            rc = match self.node_acquire(ctx, i_child, root) {
                Ok(c) => {
                    child = c;
                    let depth = self.depth;
                    self.remove_node(ctx, c, depth - 1)
                }
                Err(e) => Err(e),
            };
            let rc2 = self.node_release(ctx, child);
            if rc.is_ok() {
                rc = rc2;
            }
            if rc.is_ok() {
                self.depth -= 1;
                let depth = self.depth;
                let n = &mut self.nodes[root];
                write_int16(&mut n.data[0..2], depth);
                n.dirty = true;
                sqliters_types::dtrace!("TR|rt|depth|o=shrink d={}", depth);
            }
        }

        // Re-insert the contents of any underfull nodes removed from the tree.
        while let Some(del) = self.deleted.pop() {
            if rc.is_ok() {
                rc = self.reinsert_node_content(ctx, del);
            }
            self.n_node_ref -= 1;
            self.free_slot(del);
        }

        // Release the reference to the root node.
        let rc2 = self.node_release(ctx, root);
        if rc.is_ok() {
            rc = rc2;
        }
        rc
    }

    // ------------------------------------------------------------------------------------
    // xUpdate
    // ------------------------------------------------------------------------------------

    /// `rtreeConstraintError`
    #[cold]
    #[inline(never)]
    fn constraint_error(&self, i_col: usize) -> SqliteError {
        let name = |i: usize| self.col_names.get(i).map_or("", String::as_str);
        if i_col == 0 {
            SqliteError::new(
                ErrorCode::Constraint,
                format!("UNIQUE constraint failed: {}.{}", self.name, name(0)),
            )
        } else {
            SqliteError::new(
                ErrorCode::Constraint,
                format!(
                    "rtree constraint failed: {}.({}<={})",
                    self.name,
                    name(i_col),
                    name(i_col + 1)
                ),
            )
        }
    }

    /// `rtreeUpdate`. `old` is `aData[0]`; `values` are `aData[2..]` when `nData > 1`.
    fn rtree_update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        old: &Value,
        values: Option<&[Value]>,
    ) -> Result<Option<i64>> {
        if self.n_node_ref != 0 || ctx.vtab_cursor_positioned() {
            // Unable to write to the btree while another cursor is reading from it, since
            // the write might do a rebalance which would disrupt the read cursor. (C's
            // cursor holds node references — `nNodeRef` — while it is positioned on a row;
            // here the scan has run to its end inside xFilter, and the host tells whether a
            // statement is still handing out the rows of such a scan.)
            return Err(locked_vtab());
        }
        let mut cell = RtreeCell::zero();
        let mut have_rowid = false;
        let mut rc: Result<()> = Ok(());
        const NULL: Value = Value::Null;

        if let Some(vals) = values {
            // nData-4, capped at nDim2: the first coordinate is aData[3].
            let mut nn = (vals.len() + 2).saturating_sub(4);
            if nn > self.n_dim2 {
                nn = self.n_dim2;
            }
            let mut ii = 0;
            if self.coord_int {
                while ii < nn {
                    let lo = rtree_value_int32(vals.get(ii + 1).unwrap_or(&NULL));
                    let hi = rtree_value_int32(vals.get(ii + 2).unwrap_or(&NULL));
                    cell.c[ii] = lo as u32;
                    cell.c[ii + 1] = hi as u32;
                    if lo > hi {
                        return Err(self.constraint_error(ii + 1));
                    }
                    ii += 2;
                }
            } else {
                while ii < nn {
                    let lo = rtree_value_down(value_to_f64(vals.get(ii + 1).unwrap_or(&NULL)));
                    let hi = rtree_value_up(value_to_f64(vals.get(ii + 2).unwrap_or(&NULL)));
                    cell.c[ii] = lo.to_bits();
                    cell.c[ii + 1] = hi.to_bits();
                    if lo > hi {
                        return Err(self.constraint_error(ii + 1));
                    }
                    ii += 2;
                }
            }

            // If a rowid value was supplied, check if it is already present in the table.
            // If so, the constraint has failed.
            let id = vals.first().unwrap_or(&NULL);
            if !id.is_null() {
                cell.rowid = value_to_i64(id);
                if old.is_null() || value_to_i64(old) != cell.rowid {
                    let exists = ctx
                        .shadow_int_read(&self.rowid_tbl, &mut self.rowid_hint, cell.rowid, 1)?
                        .is_some();
                    if exists {
                        if ctx.vtab_on_conflict() == VTabConflictMode::Replace {
                            rc = self.rtree_delete_rowid(ctx, cell.rowid);
                        } else {
                            return Err(self.constraint_error(0));
                        }
                    }
                }
                have_rowid = true;
            }
        }

        // If aData[0] is not an SQL NULL value, it is the rowid of a record to delete from
        // the r-tree table.
        if !old.is_null() {
            rc = self.rtree_delete_rowid(ctx, value_to_i64(old));
        }

        // If the aData[] array contains more than one element, elements
        // (aData[2]..aData[argc-1]) contain a new record to insert into the r-tree structure.
        let mut out = None;
        if rc.is_ok()
            && let Some(vals) = values
        {
            if !have_rowid {
                match self.rtree_new_rowid(ctx) {
                    Ok(r) => cell.rowid = r,
                    Err(e) => rc = Err(e),
                }
            }
            out = Some(cell.rowid);
            if rc.is_ok() {
                match self.choose_leaf(ctx, &cell, 0) {
                    Ok(leaf) => {
                        rc = self.rtree_insert_cell(ctx, leaf, &cell, 0);
                        let rc2 = self.node_release(ctx, leaf);
                        if rc.is_ok() {
                            rc = rc2;
                        }
                    }
                    Err(e) => rc = Err(e),
                }
            }
            if rc.is_ok() && self.n_aux > 0 {
                let mut params = Vec::with_capacity(self.n_aux + 1);
                params.push(Value::Integer(cell.rowid));
                for jj in 0..self.n_aux {
                    params.push(
                        vals.get(self.n_dim2 + 1 + jj)
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                }
                sqliters_types::dtrace!("TR|rt|w|tbl=aux k={}", cell.rowid);
                rc = ctx
                    .execute_sql_counted(&self.write_aux_sql, &params)
                    .map(|_| ());
            }
        }
        rc.map(|()| out)
    }

    // ------------------------------------------------------------------------------------
    // Search (xFilter / xNext)
    // ------------------------------------------------------------------------------------

    /// `rtreeSearchPointFirst`
    #[inline(always)]
    fn search_first(cur: &RtreeSearch) -> Option<RtreeSearchPoint> {
        if cur.b_point {
            Some(cur.s_point)
        } else {
            cur.a_point.first().copied()
        }
    }

    /// `rtreeSearchPointCompare`
    #[inline(always)]
    fn search_point_compare(a: &RtreeSearchPoint, b: &RtreeSearchPoint) -> i32 {
        if a.r_score < b.r_score {
            return -1;
        }
        if a.r_score > b.r_score {
            return 1;
        }
        if a.i_level < b.i_level {
            return -1;
        }
        if a.i_level > b.i_level {
            return 1;
        }
        0
    }

    /// `rtreeSearchPointSwap`
    fn search_point_swap(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        p: &mut RtreeSearch,
        i: usize,
        j: usize,
    ) {
        p.a_point.swap(i, j);
        let (i, j) = (i + 1, j + 1);
        if i < RTREE_CACHE_SZ {
            if j >= RTREE_CACHE_SZ {
                let _ = self.node_release(ctx, p.a_node[i]);
                p.a_node[i] = NIL;
            } else {
                p.a_node.swap(i, j);
            }
        }
    }

    /// `rtreeNodeOfFirstSearchPoint`
    fn node_of_first_search_point(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        cur: &mut RtreeSearch,
    ) -> Result<usize> {
        let ii = usize::from(!cur.b_point);
        if cur.a_node[ii] == NIL {
            let id = if ii != 0 {
                cur.a_point[0].id
            } else {
                cur.s_point.id
            };
            cur.a_node[ii] = self.node_acquire(ctx, id, NIL)?;
        }
        Ok(cur.a_node[ii])
    }

    /// `rtreeEnqueue`: push a new element onto the priority queue; returns its slot.
    fn enqueue(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        cur: &mut RtreeSearch,
        r_score: f64,
        i_level: u8,
    ) -> usize {
        let mut i = cur.a_point.len();
        // (C leaves id / eWithin / iCell of the new slot unset; the caller fills them.)
        cur.a_point.push(RtreeSearchPoint {
            r_score,
            id: 0,
            i_level,
            e_within: 0,
            i_cell: 0,
        });
        while i > 0 {
            let j = (i - 1) / 2;
            if Self::search_point_compare(&cur.a_point[i], &cur.a_point[j]) >= 0 {
                break;
            }
            self.search_point_swap(ctx, cur, j, i);
            i = j;
        }
        i
    }

    /// `rtreeSearchPointNew`
    fn search_point_new(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        cur: &mut RtreeSearch,
        r_score: f64,
        i_level: u8,
    ) -> PointLoc {
        let first = Self::search_first(cur);
        let better = match first {
            None => true,
            Some(f) => f.r_score > r_score || (f.r_score == r_score && f.i_level > i_level),
        };
        if better {
            if cur.b_point {
                let slot = self.enqueue(ctx, cur, r_score, i_level);
                let ii = slot + 1;
                if ii < RTREE_CACHE_SZ {
                    cur.a_node[ii] = cur.a_node[0];
                } else {
                    let _ = self.node_release(ctx, cur.a_node[0]);
                }
                cur.a_node[0] = NIL;
                cur.a_point[slot] = cur.s_point;
            }
            cur.s_point.r_score = r_score;
            cur.s_point.i_level = i_level;
            cur.b_point = true;
            PointLoc::SPoint
        } else {
            PointLoc::APoint(self.enqueue(ctx, cur, r_score, i_level))
        }
    }

    /// `rtreeSearchPointPop`: remove the search point with the lowest current score.
    fn search_point_pop(&mut self, ctx: &mut dyn VTabHostContext, p: &mut RtreeSearch) {
        let i = usize::from(!p.b_point);
        if p.a_node[i] != NIL {
            let _ = self.node_release(ctx, p.a_node[i]);
            p.a_node[i] = NIL;
        }
        if p.b_point {
            p.b_point = false;
        } else if !p.a_point.is_empty() {
            let n = p.a_point.len() - 1;
            p.a_point[0] = p.a_point[n];
            p.a_point.truncate(n);
            if n < RTREE_CACHE_SZ - 1 {
                p.a_node[1] = p.a_node[n + 1];
                p.a_node[n + 1] = NIL;
            }
            let mut i = 0;
            loop {
                let j = i * 2 + 1;
                if j >= n {
                    break;
                }
                let k = j + 1;
                if k < n && Self::search_point_compare(&p.a_point[k], &p.a_point[j]) < 0 {
                    if Self::search_point_compare(&p.a_point[k], &p.a_point[i]) < 0 {
                        self.search_point_swap(ctx, p, i, k);
                        i = k;
                    } else {
                        break;
                    }
                } else if Self::search_point_compare(&p.a_point[j], &p.a_point[i]) < 0 {
                    self.search_point_swap(ctx, p, i, j);
                    i = j;
                } else {
                    break;
                }
            }
        }
    }

    /// `RTREE_DECODE_COORD`
    #[inline(always)]
    fn decode_coord(e_int: bool, a: &[u8]) -> f64 {
        let u = read_coord(a);
        if e_int {
            f64::from(u as i32)
        } else {
            f64::from(f32::from_bits(u))
        }
    }

    /// `rtreeNonleafConstraint`: true when the constraint cannot be satisfied by any child
    /// within the cell (`*peWithin = NOT_WITHIN`).
    // The comparisons are written as in C so that a NaN takes the same branch.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    #[inline(always)]
    fn nonleaf_rejects(p: &RtreeConstraint, e_int: bool, cell_data: &[u8]) -> bool {
        // p->iCoord might point to either a lower or upper bound coordinate in a coordinate
        // pair. But make pCellData point to the lower bound.
        let base = 8 + 4 * (p.i_coord & 0xfe);
        match p.op {
            RTREE_TRUE => false,
            RTREE_FALSE => true,
            RTREE_EQ => {
                let lo = Self::decode_coord(e_int, &cell_data[base..]);
                if p.r_value >= lo {
                    let hi = Self::decode_coord(e_int, &cell_data[base + 4..]);
                    if p.r_value <= hi {
                        return false;
                    }
                }
                true
            }
            RTREE_LE | RTREE_LT => {
                let lo = Self::decode_coord(e_int, &cell_data[base..]);
                !(p.r_value >= lo)
            }
            _ => {
                let hi = Self::decode_coord(e_int, &cell_data[base + 4..]);
                !(p.r_value <= hi)
            }
        }
    }

    /// `rtreeLeafConstraint`: true when the constraint is not satisfied.
    // The comparisons are written as in C so that a NaN takes the same branch.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    #[inline(always)]
    fn leaf_rejects(p: &RtreeConstraint, e_int: bool, cell_data: &[u8]) -> bool {
        let x_n = Self::decode_coord(e_int, &cell_data[8 + p.i_coord * 4..]);
        match p.op {
            RTREE_TRUE => false,
            RTREE_FALSE => true,
            RTREE_LE => !(x_n <= p.r_value),
            RTREE_LT => !(x_n < p.r_value),
            RTREE_GE => !(x_n >= p.r_value),
            RTREE_GT => !(x_n > p.r_value),
            _ => !(x_n == p.r_value),
        }
    }

    /// `rtreeStepToLeaf`: continue the search until the front of the queue contains an
    /// entry suitable for returning as a result-set row, or until the queue is empty.
    fn step_to_leaf(&mut self, ctx: &mut dyn VTabHostContext, cur: &mut RtreeSearch) -> Result<()> {
        let e_int = self.coord_int;
        let bpc = self.bytes_per_cell;
        loop {
            let Some(p) = Self::search_first(cur) else {
                cur.at_eof = true;
                return Ok(());
            };
            if p.i_level == 0 {
                cur.at_eof = false;
                return Ok(());
            }
            let node = self.node_of_first_search_point(ctx, cur)?;
            let n_cell = ncell(&self.nodes[node].data);
            if n_cell > RTREE_MAXCELLS {
                return Err(corrupt_vtab());
            }
            let mut i_cell = p.i_cell as usize;
            let mut pushed = false;
            while i_cell < n_cell {
                let off = 4 + bpc * i_cell;
                let cell_data = &self.nodes[node].data[off..off + bpc];
                let mut rejected = false;
                if p.i_level == 1 {
                    for c in &cur.a_constraint {
                        if Self::leaf_rejects(c, e_int, cell_data) {
                            rejected = true;
                            break;
                        }
                    }
                } else {
                    for c in &cur.a_constraint {
                        if Self::nonleaf_rejects(c, e_int, cell_data) {
                            rejected = true;
                            break;
                        }
                    }
                }
                i_cell += 1;
                if rejected {
                    continue;
                }
                // (eWithin stays FULLY_WITHIN: only the callback constraints lower it.)
                let x_level = p.i_level - 1;
                let (x_id, x_cell);
                if x_level != 0 {
                    x_id = i64::from_be_bytes([
                        cell_data[0],
                        cell_data[1],
                        cell_data[2],
                        cell_data[3],
                        cell_data[4],
                        cell_data[5],
                        cell_data[6],
                        cell_data[7],
                    ]);
                    for pt in &cur.a_point {
                        if pt.id == x_id {
                            return Err(corrupt_vtab());
                        }
                    }
                    x_cell = 0u8;
                } else {
                    x_id = p.id;
                    x_cell = (i_cell - 1) as u8;
                }
                // p->iCell was advanced in place.
                if cur.b_point {
                    cur.s_point.i_cell = i_cell as u8;
                } else {
                    cur.a_point[0].i_cell = i_cell as u8;
                }
                if i_cell >= n_cell {
                    self.search_point_pop(ctx, cur);
                }
                let loc = self.search_point_new(ctx, cur, 0.0, x_level);
                let np = match loc {
                    PointLoc::SPoint => &mut cur.s_point,
                    PointLoc::APoint(i) => &mut cur.a_point[i],
                };
                np.e_within = FULLY_WITHIN;
                np.id = x_id;
                np.i_cell = x_cell;
                sqliters_types::dtrace!(
                    "TR|rt|push|lvl={} id={} cell={} at={}",
                    x_level,
                    x_id,
                    x_cell,
                    match loc {
                        PointLoc::SPoint => -1i64,
                        PointLoc::APoint(i) => i as i64,
                    }
                );
                pushed = true;
                break;
            }
            if !pushed {
                // Every remaining cell was rejected: p->iCell>=nCell.
                self.search_point_pop(ctx, cur);
            }
        }
    }

    /// `resetCursor`: release the nodes the cursor holds.
    fn search_reset(&mut self, ctx: &mut dyn VTabHostContext, cur: &mut RtreeSearch) {
        for ii in 0..RTREE_CACHE_SZ {
            let _ = self.node_release(ctx, cur.a_node[ii]);
            cur.a_node[ii] = NIL;
        }
        cur.a_point.clear();
        cur.a_constraint.clear();
        cur.b_point = false;
        cur.at_eof = false;
    }

    /// `rtreeFilter`, positioned on the first row (or at EOF).
    fn search_start(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        cur: &mut RtreeSearch,
        idx_num: i32,
        idx_str: Option<&str>,
        argv: &[Value],
    ) -> Result<()> {
        sqliters_types::dtrace!(
            "TR|rt|scan|idx={} str={} argc={}",
            idx_num,
            idx_str.unwrap_or(""),
            argv.len()
        );
        if idx_num == 1 {
            // Special case - lookup by rowid.
            let arg = argv.first().unwrap_or(&Value::Null);
            let i_rowid = value_to_i64(arg);
            let is_rowid = match value_numeric_type(arg) {
                NumericArg::Int(_) => true,
                NumericArg::Float(f) => cmp_int_float(i_rowid, f).is_eq(),
                _ => false,
            };
            let found = if is_rowid {
                self.find_leaf_node(ctx, i_rowid)?
            } else {
                None
            };
            if let Some((leaf, i_node)) = found {
                let loc = self.search_point_new(ctx, cur, 0.0, 0);
                debug_assert!(matches!(loc, PointLoc::SPoint));
                cur.a_node[0] = leaf;
                cur.s_point.id = i_node;
                cur.s_point.e_within = PARTLY_WITHIN;
                let i_cell = self.node_rowid_index(leaf, i_rowid)?;
                cur.s_point.i_cell = i_cell as u8;
                sqliters_types::dtrace!("TR|rt|push|lvl=0 id={} cell={} at=-1", i_node, i_cell);
            } else {
                cur.at_eof = true;
            }
            Ok(())
        } else {
            // Normal case - r-tree scan. Set up the RtreeCursor.aConstraint array with the
            // configured constraints.
            let root = self.node_acquire(ctx, 1, NIL)?;
            let idx = idx_str.map_or(&[][..], str::as_bytes);
            for (ii, arg) in argv.iter().enumerate() {
                let (Some(&op_ch), Some(&coord_ch)) = (idx.get(ii * 2), idx.get(ii * 2 + 1)) else {
                    break;
                };
                let mut op = op_ch;
                let i_coord = coord_ch.wrapping_sub(b'0') as usize;
                let mut r_value = 0.0;
                if op >= RTREE_MATCH {
                    // A MATCH operator: the right-hand side must be the pointer value that a
                    // geometry callback function returns (deserializeGeometry); there are no
                    // such functions here, so this is the SQLITE_ERROR C returns for any
                    // other value.
                    let _ = self.node_release(ctx, root);
                    return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
                }
                match value_numeric_type(arg) {
                    NumericArg::Int(i_val) => {
                        r_value = i_val as f64;
                        if i_val >= (1i64 << 48) || i_val <= -(1i64 << 48) {
                            if op == RTREE_LT {
                                op = RTREE_LE;
                            }
                            if op == RTREE_GT {
                                op = RTREE_GE;
                            }
                        }
                    }
                    NumericArg::Float(f) => r_value = f,
                    NumericArg::Null => op = RTREE_FALSE,
                    NumericArg::TextOrBlob => {
                        op = if op == RTREE_LT || op == RTREE_LE {
                            RTREE_TRUE
                        } else {
                            RTREE_FALSE
                        };
                    }
                }
                if i_coord >= self.n_dim2 && op != RTREE_TRUE && op != RTREE_FALSE {
                    // (Cannot come from rtreeBestIndex; C would read past the cell.)
                    let _ = self.node_release(ctx, root);
                    return Err(corrupt_vtab());
                }
                cur.a_constraint.push(RtreeConstraint {
                    i_coord,
                    op,
                    r_value,
                });
            }
            let lvl = (self.depth + 1) as u8;
            let loc = self.search_point_new(ctx, cur, 0.0, lvl);
            debug_assert!(matches!(loc, PointLoc::SPoint));
            cur.s_point.id = 1;
            cur.s_point.i_cell = 0;
            cur.s_point.e_within = PARTLY_WITHIN;
            cur.a_node[0] = root;
            sqliters_types::dtrace!("TR|rt|push|lvl={} id=1 cell=0 at=-1", lvl);
            self.step_to_leaf(ctx, cur)
        }
    }

    /// Runs a whole scan (`rtreeFilter`, then `rtreeColumn` / `rtreeRowid` / `rtreeNext` until
    /// EOF) and collects the cells in the order the cursor returns them.
    fn search_collect(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        argv: &[Value],
        out: &mut Vec<RtreeCell>,
    ) -> Result<()> {
        // (The queue and constraint buffers of the last scan are reused.)
        let mut cur = match self.search_pool.take() {
            Some(c) => c,
            None => RtreeSearch::new(),
        };
        let res = (|| {
            self.search_start(ctx, &mut cur, idx_num, idx_str, argv)?;
            while !cur.at_eof {
                let Some(p) = Self::search_first(&cur) else {
                    break;
                };
                let node = self.node_of_first_search_point(ctx, &mut cur)?;
                if p.i_cell as usize >= ncell(&self.nodes[node].data) {
                    return Err(SqliteError::new(ErrorCode::Abort, "query aborted"));
                }
                let cell = self.node_get_cell(node, p.i_cell as usize);
                sqliters_types::dtrace!("TR|rt|row|id={}", cell.rowid);
                out.push(cell);
                // rtreeNext
                self.search_point_pop(ctx, &mut cur);
                self.step_to_leaf(ctx, &mut cur)?;
            }
            Ok(())
        })();
        self.search_reset(ctx, &mut cur);
        self.search_pool = Some(cur);
        if res.is_err() && self.n_node_ref != 0 {
            self.reset_nodes();
        }
        res
    }
}

/// `rtreeValueInt32`
#[inline]
fn rtree_value_int32(v: &Value) -> i32 {
    let v64 = value_to_i64(v);
    if v64 > 2_147_483_647 {
        return 2_147_483_647;
    }
    if v64 < -2_147_483_648 {
        return -2_147_483_648;
    }
    v64 as i32
}

/// Built-in R-Tree spatial virtual table module (`rtree` or `rtree_i32`).
#[derive(Debug, Clone)]
pub struct RTreeModule {
    module_name: &'static str,
    mode: RTreeCoordMode,
}

impl RTreeModule {
    /// Standard 32-bit floating-point R-Tree module (`USING rtree(...)`).
    pub const fn new_f32() -> Self {
        Self {
            module_name: "rtree",
            mode: RTreeCoordMode::Float32,
        }
    }

    /// 32-bit signed integer R-Tree module (`USING rtree_i32(...)`).
    pub const fn new_i32() -> Self {
        Self {
            module_name: "rtree_i32",
            mode: RTreeCoordMode::Int32,
        }
    }
}

impl VirtualTableModule for RTreeModule {
    fn name(&self) -> &str {
        self.module_name
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn is_eponymous(&self) -> bool {
        false
    }

    fn shadow_table_suffixes(&self) -> &[&str] {
        &["node", "rowid", "parent"]
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        RTreeTable::init(ctx, args, self.mode, true).map(|t| Box::new(t) as Box<dyn VirtualTable>)
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        RTreeTable::init(ctx, args, self.mode, false).map(|t| Box::new(t) as Box<dyn VirtualTable>)
    }
}

/// The `sqlite3_vtab` of an r-tree. Cursors share the [`Rtree`] object (`RTREE_OF_CURSOR`).
#[derive(Debug, Clone)]
struct RTreeTable {
    rt: Arc<Mutex<Rtree>>,
    n_dim2: usize,
    n_aux: usize,
    coord_int: bool,
    /// `nRowEst` (fixed when the table is connected).
    n_row_est: i64,
}

impl RTreeTable {
    /// `rtreeInit`: xCreate and xConnect.
    fn init(
        ctx: &mut dyn VTabHostContext,
        args: &[String],
        mode: RTreeCoordMode,
        is_create: bool,
    ) -> Result<Self> {
        // argc<6 || argc>RTREE_MAX_AUX_COLUMN+3
        if args.len() < 3 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "Too few columns for an rtree table",
            ));
        }
        if args.len() > RTREE_MAX_AUX_COLUMN {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "Too many columns for an rtree table",
            ));
        }

        ctx.set_vtab_config(VTabConfigOption::ConstraintSupport)?;
        ctx.set_vtab_config(VTabConfigOption::Innocuous)?;

        let db_name = ctx.db_name().to_owned();
        let table_name = ctx.table_name().to_owned();
        let coord_int = mode == RTreeCoordMode::Int32;

        let id_tok = extract_first_sql_token(&args[0]);
        if id_tok.is_empty() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "vtable constructor failed: invalid id column",
            ));
        }
        let mut col_names = vec![dequote_ident(id_tok)];
        let mut declare_sql = format!("CREATE TABLE x({id_tok} INT");

        let mut n_dim2 = 0usize;
        let mut n_aux = 0usize;
        let mut aux_not_last = false;
        for raw_arg in &args[1..] {
            let trimmed = raw_arg.trim_start();
            if let Some(after_plus) = trimmed.strip_prefix('+') {
                let aux_tok = extract_first_sql_token(after_plus);
                if aux_tok.is_empty() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "vtable constructor failed: invalid auxiliary column",
                    ));
                }
                n_aux += 1;
                col_names.push(dequote_ident(aux_tok));
                let _ = write!(declare_sql, ",{aux_tok}");
            } else if n_aux > 0 {
                aux_not_last = true;
                break;
            } else {
                let coord_tok = extract_first_sql_token(trimmed);
                if coord_tok.is_empty() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "vtable constructor failed: invalid coordinate column",
                    ));
                }
                n_dim2 += 1;
                col_names.push(dequote_ident(coord_tok));
                if coord_int {
                    let _ = write!(declare_sql, ",{coord_tok} INT");
                } else {
                    let _ = write!(declare_sql, ",{coord_tok} REAL");
                }
            }
        }
        declare_sql.push(')');
        if aux_not_last {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "Auxiliary rtree columns must be last",
            ));
        }
        ctx.declare_vtab(&declare_sql)?;

        let n_dim = n_dim2 / 2;
        if n_dim < 1 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "Too few columns for an rtree table",
            ));
        }
        if n_dim2 > RTREE_MAX_DIMENSIONS * 2 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "Too many columns for an rtree table",
            ));
        }
        if !n_dim2.is_multiple_of(2) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "Wrong number of columns for an rtree table",
            ));
        }

        let bytes_per_cell = 8 + n_dim2 * 4;
        let q_db = quote_ident(&db_name);
        let node_tbl = format!("{table_name}_node");
        let rowid_tbl = format!("{table_name}_rowid");
        let parent_tbl = format!("{table_name}_parent");
        let q_rowid = quote_ident(&rowid_tbl);
        let q_node = quote_ident(&node_tbl);
        let q_parent = quote_ident(&parent_tbl);

        // getNodeSize
        let node_size = if is_create {
            let page_rows = ctx.query_sql(&format!("PRAGMA {q_db}.page_size"), &[])?;
            let page_size = page_rows
                .first()
                .and_then(|r| r.first())
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let mut sz = page_size - 64;
            let max_by_cells = (4 + bytes_per_cell * RTREE_MAXCELLS) as i64;
            if max_by_cells < sz {
                sz = max_by_cells;
            }
            sz.max(0) as usize
        } else {
            let rows = ctx.query_sql(
                &format!("SELECT length(data) FROM {q_db}.{q_node} WHERE nodeno = 1"),
                &[],
            )?;
            let sz = rows
                .first()
                .and_then(|r| r.first())
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if sz < (512 - 64) {
                return Err(SqliteError::with_extended(
                    SQLITE_CORRUPT_VTAB,
                    format!("undersize RTree blobs in \"{node_tbl}\""),
                ));
            }
            sz as usize
        };

        // rtreeSqlInit
        if is_create {
            let mut create_rowid =
                format!("CREATE TABLE {q_db}.{q_rowid}(rowid INTEGER PRIMARY KEY,nodeno");
            for i in 0..n_aux {
                let _ = write!(create_rowid, ",a{i}");
            }
            create_rowid.push(')');
            ctx.execute_sql(&create_rowid, &[])?;
            ctx.execute_sql(
                &format!("CREATE TABLE {q_db}.{q_node}(nodeno INTEGER PRIMARY KEY,data)"),
                &[],
            )?;
            ctx.execute_sql(
                &format!("CREATE TABLE {q_db}.{q_parent}(nodeno INTEGER PRIMARY KEY,parentnode)"),
                &[],
            )?;
            // "INSERT INTO x_node VALUES(1,zeroblob(N))": the last statement of the
            // sqlite3_exec() string, an ordinary counted statement.
            ctx.execute_sql_counted(
                &format!("INSERT INTO {q_db}.{q_node}VALUES(1,zeroblob({node_size}))"),
                &[],
            )?;
        }

        // rtreeQueryStat1
        let n_row_est = rtree_query_stat1(ctx, &q_db, &db_name, &table_name);

        let mut write_aux_sql = String::new();
        let mut write_rowid_upsert_sql = String::new();
        if n_aux > 0 {
            write_rowid_upsert_sql = format!(
                "INSERT INTO{q_db}.{q_rowid}(rowid,nodeno)VALUES(?1,?2)\
                 ON CONFLICT(rowid)DO UPDATE SET nodeno=excluded.nodeno"
            );
            write_aux_sql = format!("UPDATE {q_db}.{q_rowid}SET ");
            for ii in 0..n_aux {
                if ii > 0 {
                    write_aux_sql.push(',');
                }
                let _ = write!(write_aux_sql, "a{ii}=?{}", ii + 2);
            }
            write_aux_sql.push_str(" WHERE rowid=?1");
        }

        let rt = Rtree {
            db_name,
            name: table_name,
            node_tbl,
            rowid_tbl,
            parent_tbl,
            node_hint: ShadowHint::NONE,
            rowid_hint: ShadowHint::NONE,
            parent_hint: ShadowHint::NONE,
            coord_int,
            n_dim,
            n_dim2,
            n_aux,
            bytes_per_cell,
            node_size,
            in_wr_trans: false,
            depth: -1,
            n_node_ref: 0,
            nodes: Vec::new(),
            free: Vec::new(),
            hash: Vec::new(),
            deleted: Vec::new(),
            col_names,
            write_rowid_upsert_sql,
            write_aux_sql,
            search_pool: None,
        };
        Ok(Self {
            rt: Arc::new(Mutex::new(rt)),
            n_dim2,
            n_aux,
            coord_int,
            n_row_est,
        })
    }
}

/// `rtreeQueryStat1`: the estimated number of rows, from `sqlite_stat1` when it exists.
fn rtree_query_stat1(
    ctx: &mut dyn VTabHostContext,
    q_db: &str,
    db_name: &str,
    table_name: &str,
) -> i64 {
    // sqlite3_table_column_metadata(db, zDb, "sqlite_stat1", ...) != SQLITE_OK
    let has_stat1 = ctx
        .query_sql(
            &format!(
                "SELECT 1 FROM {q_db}.sqlite_schema WHERE name='sqlite_stat1' AND type='table'"
            ),
            &[],
        )
        .map(|rows| !rows.is_empty())
        .unwrap_or(false);
    if !has_stat1 {
        return RTREE_DEFAULT_ROWEST;
    }
    let _ = db_name;
    let mut n_row = RTREE_MIN_ROWEST;
    if let Ok(rows) = ctx.query_sql(
        &format!("SELECT stat FROM {q_db}.sqlite_stat1 WHERE tbl = ?1"),
        &[Value::Text(format!("{table_name}_rowid"))],
    ) && let Some(v) = rows.first().and_then(|r| r.first())
    {
        n_row = value_to_i64(v);
    }
    n_row.max(RTREE_MIN_ROWEST)
}

impl VirtualTable for RTreeTable {
    /// `rtreeBestIndex`
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        // Check if there exists a MATCH constraint - even an unusable one. If there is, do
        // not consider the lookup-by-rowid plan.
        let b_match = info
            .constraints
            .iter()
            .any(|c| c.op == VTabConstraintOp::Match);

        const IDX_MAX: usize = RTREE_MAX_DIMENSIONS * 8;
        let mut idx_str = String::new();
        let n_constraint = info.constraints.len();
        let mut ii = 0;
        while ii < n_constraint && idx_str.len() < IDX_MAX {
            let p = info.constraints[ii].clone();
            if !b_match && p.usable && p.i_column <= 0 && p.op == VTabConstraintOp::Eq {
                // We have an equality constraint on the rowid. Use strategy 1.
                for u in &mut info.constraint_usage[..ii] {
                    u.argv_index = 0;
                    u.omit = false;
                }
                info.idx_num = 1;
                info.idx_str = None;
                info.constraint_usage[ii].argv_index = 1;
                info.constraint_usage[ii].omit = true;
                // Two rowid lookups on B-Tree structures and then a linear search of an
                // R-Tree node; expected to return a single row.
                info.estimated_cost = 30.0;
                info.estimated_rows = 1;
                info.idx_flags = 1; // SQLITE_INDEX_SCAN_UNIQUE
                sqliters_types::dtrace!("TR|rt|plan|idx=1 str= rows=1");
                return Ok(());
            }

            if p.usable
                && ((p.i_column > 0 && (p.i_column as usize) <= self.n_dim2)
                    || p.op == VTabConstraintOp::Match)
            {
                let (op, do_omit) = match p.op {
                    VTabConstraintOp::Eq => (RTREE_EQ, false),
                    VTabConstraintOp::Gt => (RTREE_GT, false),
                    VTabConstraintOp::Le => (RTREE_LE, true),
                    VTabConstraintOp::Lt => (RTREE_LT, false),
                    VTabConstraintOp::Ge => (RTREE_GE, true),
                    VTabConstraintOp::Match => (RTREE_MATCH, true),
                    _ => (0, true),
                };
                if op != 0 {
                    idx_str.push(char::from(op));
                    idx_str.push(char::from((p.i_column - 1 + i32::from(b'0')) as u8));
                    info.constraint_usage[ii].argv_index = idx_str.len() / 2;
                    info.constraint_usage[ii].omit = do_omit;
                }
            }
            ii += 1;
        }

        info.idx_num = 2;
        let n_pairs = idx_str.len() / 2;
        let n_row = self.n_row_est >> n_pairs;
        sqliters_types::dtrace!("TR|rt|plan|idx=2 str={} rows={}", idx_str, n_row);
        info.idx_str = if idx_str.is_empty() {
            None
        } else {
            Some(idx_str)
        };
        info.estimated_cost = 6.0 * (n_row as f64);
        info.estimated_rows = n_row;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(RTreeCursor {
            table: self.clone(),
            cells: Vec::new(),
            aux_rows: Vec::new(),
            pos: 0,
        }))
    }

    /// `rtreeBeginTransaction`
    fn begin(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.rt.lock().unwrap().in_wr_trans = true;
        Ok(())
    }

    /// `rtreeEndTransaction`
    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.rt.lock().unwrap().in_wr_trans = false;
        Ok(())
    }

    /// `rtreeRollback`
    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut rt = self.rt.lock().unwrap();
        rt.in_wr_trans = false;
        if rt.n_node_ref != 0 {
            rt.reset_nodes();
        }
        Ok(())
    }

    /// `rtreeUpdate`
    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let mut rt = self.rt.lock().unwrap();
        let res = match &op {
            VTabUpdateOperation::Delete { old_rowid } => rt.rtree_update(ctx, old_rowid, None),
            VTabUpdateOperation::Insert { values, .. } => {
                rt.rtree_update(ctx, &Value::Null, Some(values))
            }
            VTabUpdateOperation::Update {
                old_rowid, values, ..
            } => rt.rtree_update(ctx, old_rowid, Some(values)),
        };
        if res.is_err() && rt.n_node_ref != 0 {
            rt.reset_nodes();
        }
        res
    }

    /// `rtreeRename`
    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        let mut rt = self.rt.lock().unwrap();
        let q_db = quote_ident(&rt.db_name);
        for suffix in ["node", "parent", "rowid"] {
            let old_q = quote_ident(&format!("{}_{suffix}", rt.name));
            let new_q = quote_ident(&format!("{new_name}_{suffix}"));
            // `sqlite3_exec(db, zSql, 0, 0, 0)`: only the result code comes back; OP_VRename
            // then reports `sqlite3ErrStr(rc)`.
            ctx.execute_sql(
                &format!("ALTER TABLE {q_db}.{old_q} RENAME TO {new_q}"),
                &[],
            )
            .map_err(|e| SqliteError::new(e.code, e.code.as_str()))?;
        }
        rt.name = new_name.to_owned();
        rt.node_tbl = format!("{new_name}_node");
        rt.rowid_tbl = format!("{new_name}_rowid");
        rt.parent_tbl = format!("{new_name}_parent");
        rt.node_hint = ShadowHint::NONE;
        rt.rowid_hint = ShadowHint::NONE;
        rt.parent_hint = ShadowHint::NONE;
        if rt.n_aux > 0 {
            let q_rowid = quote_ident(&rt.rowid_tbl);
            rt.write_rowid_upsert_sql = format!(
                "INSERT INTO{q_db}.{q_rowid}(rowid,nodeno)VALUES(?1,?2)\
                 ON CONFLICT(rowid)DO UPDATE SET nodeno=excluded.nodeno"
            );
            let mut sql = format!("UPDATE {q_db}.{q_rowid}SET ");
            for ii in 0..rt.n_aux {
                if ii > 0 {
                    sql.push(',');
                }
                let _ = write!(sql, "a{ii}=?{}", ii + 2);
            }
            sql.push_str(" WHERE rowid=?1");
            rt.write_aux_sql = sql;
        }
        Ok(())
    }

    /// `rtreeDestroy`: "DROP TABLE node; DROP TABLE rowid; DROP TABLE parent" in this order.
    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut rt = self.rt.lock().unwrap();
        rt.reset_nodes();
        let q_db = quote_ident(&rt.db_name);
        for suffix in ["node", "rowid", "parent"] {
            let q_tbl = quote_ident(&format!("{}_{suffix}", rt.name));
            ctx.execute_sql(&format!("DROP TABLE IF EXISTS {q_db}.{q_tbl}"), &[])?;
        }
        Ok(())
    }
}

/// `struct RtreeCursor`. The host drains a cursor right after `xFilter`, so the scan runs to
/// its end inside `filter` — with C's priority queue, which decides the order of the rows —
/// and the cells are handed out from a list.
struct RTreeCursor {
    table: RTreeTable,
    cells: Vec<RtreeCell>,
    aux_rows: Vec<Vec<Value>>,
    pos: usize,
}

impl VirtualCursor for RTreeCursor {
    /// `rtreeFilter` (+ every `rtreeNext` of the scan)
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        argv: &[Value],
    ) -> Result<()> {
        self.cells.clear();
        self.aux_rows.clear();
        self.pos = 0;
        let mut rt = self.table.rt.lock().unwrap();
        rt.search_collect(ctx, idx_num, idx_str, argv, &mut self.cells)?;
        if self.table.n_aux > 0 {
            // rtreeColumn: "SELECT * FROM x_rowid WHERE rowid=?1" for the row of the cursor.
            self.aux_rows.reserve(self.cells.len());
            let n_aux = self.table.n_aux;
            for cell in &self.cells {
                let mut aux = match ctx.shadow_get_rowid(&rt.rowid_tbl, cell.rowid)? {
                    Some(mut row) => {
                        if row.len() >= 2 {
                            row.drain(..2);
                        } else {
                            row.clear();
                        }
                        row
                    }
                    None => Vec::new(),
                };
                aux.resize(n_aux, Value::Null);
                self.aux_rows.push(aux);
            }
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.cells.len()
    }

    /// `rtreeColumn`
    fn column(&self, col_idx: usize) -> Result<Value> {
        let Some(cell) = self.cells.get(self.pos) else {
            return Ok(Value::Null);
        };
        if col_idx == 0 {
            return Ok(Value::Integer(cell.rowid));
        }
        let coord_idx = col_idx - 1;
        if coord_idx < self.table.n_dim2 {
            return Ok(if self.table.coord_int {
                Value::Integer(i64::from(cell.i(coord_idx)))
            } else {
                Value::Real(f64::from(cell.f(coord_idx)))
            });
        }
        let aux_idx = coord_idx - self.table.n_dim2;
        Ok(self
            .aux_rows
            .get(self.pos)
            .and_then(|r| r.get(aux_idx))
            .cloned()
            .unwrap_or(Value::Null))
    }

    /// `rtreeRowid`
    fn rowid(&self) -> Result<i64> {
        Ok(self.cells.get(self.pos).map_or(0, |c| c.rowid))
    }
}

/// Evaluates `rtreedepth(blob)` (`sqlite/ext/rtree/rtree.c:rtreedepth`).
pub fn eval_rtreedepth(arg: &Value) -> Result<Value> {
    let Value::Blob(bytes) = arg else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "Invalid argument to rtreedepth()",
        ));
    };
    if bytes.len() < 2 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "Invalid argument to rtreedepth()",
        ));
    }
    Ok(Value::Integer(i64::from(read_u16_be(&bytes[0..2]))))
}

/// Evaluates `rtreenode(nDim, blob)` (`sqlite/ext/rtree/rtree.c:rtreenode`).
/// `fmt_g` appends its argument formatted as SQLite's `%g` does.
pub fn eval_rtreenode(
    dim_arg: &Value,
    blob_arg: &Value,
    fmt_g: &dyn Fn(&mut String, f64),
) -> Result<Value> {
    let n_dim = value_to_i64(dim_arg);
    if !(1..=5).contains(&n_dim) {
        return Ok(Value::Null);
    }
    let bytes: &[u8] = match blob_arg {
        Value::Blob(b) => b.as_slice(),
        Value::Text(s) => s.as_bytes(),
        _ => return Ok(Value::Null),
    };
    if bytes.len() < 4 {
        return Ok(Value::Null);
    }
    let n_dim_usize = n_dim as usize;
    let n_dim2 = n_dim_usize * 2;
    let bytes_per_cell = 8 + 8 * n_dim_usize;
    let n_cell = read_u16_be(&bytes[2..4]) as usize;
    if bytes.len() < 4 + n_cell * bytes_per_cell {
        return Ok(Value::Null);
    }

    let mut out = String::new();
    for i in 0..n_cell {
        if i > 0 {
            out.push(' ');
        }
        let base = 4 + i * bytes_per_cell;
        let rowid = read_i64_be(&bytes[base..base + 8]);
        let _ = write!(out, "{{{rowid}");
        for j in 0..n_dim2 {
            let c_off = base + 8 + j * 4;
            let f = f32::from_bits(u32::from_be_bytes([
                bytes[c_off],
                bytes[c_off + 1],
                bytes[c_off + 2],
                bytes[c_off + 3],
            ]));
            out.push(' ');
            fmt_g(&mut out, f64::from(f));
        }
        out.push('}');
    }
    Ok(Value::Text(out))
}

/// Runs the R-Tree integrity checker (`rtreecheck(tbl)` / `rtreecheck(db, tbl)`),
/// returning `"ok"` when consistent or a newline-separated diagnostic report.
pub fn run_rtreecheck<F>(
    mut query_fn: F,
    db_name: &str,
    table_name: &str,
    n_dim: usize,
    is_int: bool,
) -> Result<String>
where
    F: FnMut(&str, &[Value]) -> Result<Vec<Vec<Value>>>,
{
    let mut state = RTreeCheckState {
        db_name,
        table_name,
        n_dim,
        is_int,
        n_leaf: 0,
        n_nonleaf: 0,
        errors: Vec::new(),
        visited: HashMap::new(),
    };

    if n_dim < 1 {
        state.push_err("Schema corrupt or not an rtree".to_owned());
        return Ok(state.finish());
    }

    state.check_node(&mut query_fn, 0, None, 1)?;
    state.check_table_count(&mut query_fn, "_rowid", state.n_leaf)?;
    state.check_table_count(&mut query_fn, "_parent", state.n_nonleaf)?;

    Ok(state.finish())
}

struct RTreeCheckState<'a> {
    db_name: &'a str,
    table_name: &'a str,
    n_dim: usize,
    is_int: bool,
    n_leaf: i64,
    n_nonleaf: i64,
    errors: Vec<String>,
    visited: HashMap<i64, bool>,
}

impl<'a> RTreeCheckState<'a> {
    fn push_err(&mut self, msg: String) {
        if self.errors.len() < RTREE_CHECK_MAX_ERROR {
            self.errors.push(msg);
        }
    }

    fn finish(self) -> String {
        if self.errors.is_empty() {
            "ok".to_owned()
        } else {
            self.errors.join("\n")
        }
    }

    fn check_mapping<F>(
        &mut self,
        query_fn: &mut F,
        is_leaf: bool,
        key: i64,
        expected: i64,
    ) -> Result<()>
    where
        F: FnMut(&str, &[Value]) -> Result<Vec<Vec<Value>>>,
    {
        let q_db = quote_ident(self.db_name);
        let (col, suffix, label) = if is_leaf {
            ("nodeno, rowid", "rowid", "%_rowid")
        } else {
            ("parentnode, nodeno", "parent", "%_parent")
        };
        let key_col = if is_leaf { "rowid" } else { "nodeno" };
        let q_tbl = quote_ident(&format!("{}_{suffix}", self.table_name));
        let rows = query_fn(
            &format!("SELECT {col} FROM {q_db}.{q_tbl} WHERE {key_col} = ?1"),
            &[Value::Integer(key)],
        )?;
        match rows.first().and_then(|r| r.first()) {
            None => {
                self.push_err(format!(
                    "Mapping ({key} -> {expected}) missing from {label} table"
                ));
            }
            Some(v) => {
                let actual = value_to_i64(v);
                if actual != expected {
                    self.push_err(format!(
                        "Found ({key} -> {actual}) in {label} table, expected ({key} -> {expected})"
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_cell_coords(
        &mut self,
        node_id: i64,
        cell_idx: usize,
        cell_coords: &[u8],
        parent_coords: Option<&[u8]>,
    ) {
        for dim in 0..self.n_dim {
            let off1 = dim * 8;
            let off2 = off1 + 4;
            let b1 = [
                cell_coords[off1],
                cell_coords[off1 + 1],
                cell_coords[off1 + 2],
                cell_coords[off1 + 3],
            ];
            let b2 = [
                cell_coords[off2],
                cell_coords[off2 + 1],
                cell_coords[off2 + 2],
                cell_coords[off2 + 3],
            ];
            let bad_order = if self.is_int {
                i32::from_be_bytes(b1) > i32::from_be_bytes(b2)
            } else {
                let f1 = f32::from_bits(u32::from_be_bytes(b1));
                let f2 = f32::from_bits(u32::from_be_bytes(b2));
                f1.is_nan() || f2.is_nan() || f1 > f2
            };
            if bad_order {
                self.push_err(format!(
                    "Dimension {dim} of cell {cell_idx} on node {node_id} is corrupt"
                ));
            }
            if let Some(p_coords) = parent_coords {
                let pb1 = [
                    p_coords[off1],
                    p_coords[off1 + 1],
                    p_coords[off1 + 2],
                    p_coords[off1 + 3],
                ];
                let pb2 = [
                    p_coords[off2],
                    p_coords[off2 + 1],
                    p_coords[off2 + 2],
                    p_coords[off2 + 3],
                ];
                let bad_parent = if self.is_int {
                    i32::from_be_bytes(b1) < i32::from_be_bytes(pb1)
                        || i32::from_be_bytes(b2) > i32::from_be_bytes(pb2)
                } else {
                    let f1 = f32::from_bits(u32::from_be_bytes(b1));
                    let f2 = f32::from_bits(u32::from_be_bytes(b2));
                    let pf1 = f32::from_bits(u32::from_be_bytes(pb1));
                    let pf2 = f32::from_bits(u32::from_be_bytes(pb2));
                    f1 < pf1 || f2 > pf2
                };
                if bad_parent {
                    self.push_err(format!(
                        "Dimension {dim} of cell {cell_idx} on node {node_id} is corrupt relative to parent"
                    ));
                }
            }
        }
    }

    fn check_node<F>(
        &mut self,
        query_fn: &mut F,
        mut depth: usize,
        parent_coords: Option<&[u8]>,
        node_id: i64,
    ) -> Result<()>
    where
        F: FnMut(&str, &[Value]) -> Result<Vec<Vec<Value>>>,
    {
        if self.visited.insert(node_id, true).is_some() {
            self.push_err(format!("Node {node_id} referenced multiple times"));
            return Ok(());
        }
        let q_db = quote_ident(self.db_name);
        let q_node = quote_ident(&format!("{}_node", self.table_name));
        let rows = query_fn(
            &format!("SELECT data FROM {q_db}.{q_node} WHERE nodeno = ?1"),
            &[Value::Integer(node_id)],
        )?;
        let Some(val) = rows.first().and_then(|r| r.first()) else {
            self.push_err(format!("Node {node_id} missing from database"));
            return Ok(());
        };
        let bytes: &[u8] = match val {
            Value::Blob(b) => b.as_slice(),
            Value::Text(s) => s.as_bytes(),
            _ => &[],
        };
        if bytes.len() < 4 {
            self.push_err(format!(
                "Node {node_id} is too small ({} bytes)",
                bytes.len()
            ));
            return Ok(());
        }
        if parent_coords.is_none() {
            depth = read_u16_be(&bytes[0..2]) as usize;
            if depth > RTREE_MAX_DEPTH {
                self.push_err(format!("Rtree depth out of range ({depth})"));
                return Ok(());
            }
        }
        let n_cell = read_u16_be(&bytes[2..4]) as usize;
        let cell_sz = 8 + self.n_dim * 8;
        if 4 + n_cell * cell_sz > bytes.len() {
            self.push_err(format!(
                "Node {node_id} is too small for cell count of {n_cell} ({} bytes)",
                bytes.len()
            ));
            return Ok(());
        }

        for i in 0..n_cell {
            let base = 4 + i * cell_sz;
            let child_val = read_i64_be(&bytes[base..base + 8]);
            let coords_slice = &bytes[base + 8..base + cell_sz];
            self.check_cell_coords(node_id, i, coords_slice, parent_coords);
            if depth > 0 {
                self.check_mapping(query_fn, false, child_val, node_id)?;
                self.check_node(query_fn, depth - 1, Some(coords_slice), child_val)?;
                self.n_nonleaf += 1;
            } else {
                self.check_mapping(query_fn, true, child_val, node_id)?;
                self.n_leaf += 1;
            }
        }
        Ok(())
    }

    fn check_table_count<F>(&mut self, query_fn: &mut F, suffix: &str, expected: i64) -> Result<()>
    where
        F: FnMut(&str, &[Value]) -> Result<Vec<Vec<Value>>>,
    {
        let q_db = quote_ident(self.db_name);
        let q_tbl = quote_ident(&format!("{}{suffix}", self.table_name));
        let rows = query_fn(&format!("SELECT count(*) FROM {q_db}.{q_tbl}"), &[])?;
        if let Some(actual) = rows.first().and_then(|r| r.first()).and_then(Value::as_i64)
            && actual != expected
        {
            self.push_err(format!(
                "Wrong number of entries in %{suffix} table - expected {expected}, actual {actual}"
            ));
        }
        Ok(())
    }
}

#[inline]
fn read_u16_be(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

#[inline]
fn read_i64_be(b: &[u8]) -> i64 {
    i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[inline]
fn rtree_value_down(d: f64) -> f32 {
    let f = d as f32;
    if f64::from(f) > d {
        let bits = f.to_bits();
        let next = if (bits & 0x8000_0000) != 0 {
            bits.wrapping_add(1)
        } else {
            bits.wrapping_sub(1)
        };
        f32::from_bits(next)
    } else {
        f
    }
}

#[inline]
fn rtree_value_up(d: f64) -> f32 {
    let f = d as f32;
    if f64::from(f) < d {
        let bits = f.to_bits();
        let next = if (bits & 0x8000_0000) != 0 {
            bits.wrapping_sub(1)
        } else {
            bits.wrapping_add(1)
        };
        f32::from_bits(next)
    } else {
        f
    }
}

fn value_to_f64(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Integer(i) => *i as f64,
        Value::Real(f) => {
            if f.is_nan() {
                0.0
            } else {
                *f
            }
        }
        Value::Text(s) => parse_numeric_prefix_f64(s),
        Value::Blob(b) => parse_numeric_prefix_f64(&String::from_utf8_lossy(b)),
    }
}

fn value_to_i64(v: &Value) -> i64 {
    match v {
        Value::Null => 0,
        Value::Integer(i) => *i,
        Value::Real(f) => {
            if f.is_nan() {
                0
            } else if *f >= (i64::MAX as f64) {
                i64::MAX
            } else if *f <= (i64::MIN as f64) {
                i64::MIN
            } else {
                *f as i64
            }
        }
        Value::Text(s) => parse_numeric_prefix_i64(s),
        Value::Blob(b) => parse_numeric_prefix_i64(&String::from_utf8_lossy(b)),
    }
}

fn parse_numeric_prefix_f64(s: &str) -> f64 {
    sqliters_types::parse_leading_f64(s)
}

fn parse_numeric_prefix_i64(s: &str) -> i64 {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return 0;
    }
    let bytes = trimmed.as_bytes();
    let mut end = 0;
    if bytes.first().is_some_and(|c| *c == b'+' || *c == b'-') {
        end += 1;
    }
    let start_digits = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == start_digits {
        return 0;
    }
    if let Ok(i) = trimmed[..end].parse::<i64>() {
        i
    } else if trimmed.starts_with('-') {
        i64::MIN
    } else {
        i64::MAX
    }
}

enum NumericArg {
    Int(i64),
    Float(f64),
    Null,
    TextOrBlob,
}

/// `sqlite3_value_numeric_type`: text that is a well-formed number counts as that number.
fn value_numeric_type(v: &Value) -> NumericArg {
    match v {
        Value::Null => NumericArg::Null,
        Value::Integer(i) => NumericArg::Int(*i),
        Value::Real(f) => NumericArg::Float(*f),
        Value::Text(s) => match sqliters_types::parse_numeric_text(s) {
            Some(Value::Integer(i)) => NumericArg::Int(i),
            Some(Value::Real(f)) => NumericArg::Float(f),
            _ => NumericArg::TextOrBlob,
        },
        Value::Blob(_) => NumericArg::TextOrBlob,
    }
}

fn extract_first_sql_token(s: &str) -> &str {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return "";
    }
    let bytes = trimmed.as_bytes();
    match bytes[0] {
        b'"' | b'\'' | b'`' => {
            let q = bytes[0];
            let mut i = 1;
            while i < bytes.len() {
                if bytes[i] == q {
                    if i + 1 < bytes.len() && bytes[i + 1] == q {
                        i += 2;
                        continue;
                    }
                    return &trimmed[..=i];
                }
                i += 1;
            }
            trimmed
        }
        b'[' => {
            if let Some(end) = trimmed.find(']') {
                &trimmed[..=end]
            } else {
                trimmed
            }
        }
        _ => {
            let end = trimmed
                .find(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')'))
                .unwrap_or(trimmed.len());
            &trimmed[..end]
        }
    }
}

fn dequote_ident(tok: &str) -> String {
    let t = tok.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[t.len() - 1] == b'"')
            || (b[0] == b'\'' && b[t.len() - 1] == b'\'')
            || (b[0] == b'`' && b[t.len() - 1] == b'`')
        {
            let q = char::from(b[0]);
            let doubled = format!("{q}{q}");
            return t[1..t.len() - 1].replace(&doubled, &q.to_string());
        }
        if b[0] == b'[' && b[t.len() - 1] == b']' {
            return t[1..t.len() - 1].to_owned();
        }
    }
    t.to_owned()
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(n_dim: usize, coord_int: bool) -> Rtree {
        Rtree {
            db_name: "main".to_owned(),
            name: "t".to_owned(),
            node_tbl: "t_node".to_owned(),
            rowid_tbl: "t_rowid".to_owned(),
            parent_tbl: "t_parent".to_owned(),
            node_hint: ShadowHint::NONE,
            rowid_hint: ShadowHint::NONE,
            parent_hint: ShadowHint::NONE,
            coord_int,
            n_dim,
            n_dim2: n_dim * 2,
            n_aux: 0,
            bytes_per_cell: 8 + n_dim * 8,
            node_size: 4096,
            in_wr_trans: false,
            depth: 0,
            n_node_ref: 0,
            nodes: Vec::new(),
            free: Vec::new(),
            hash: Vec::new(),
            deleted: Vec::new(),
            col_names: Vec::new(),
            write_rowid_upsert_sql: String::new(),
            write_aux_sql: String::new(),
            search_pool: None,
        }
    }

    fn fcell(rowid: i64, coords: &[f32]) -> RtreeCell {
        let mut c = RtreeCell::zero();
        c.rowid = rowid;
        for (i, v) in coords.iter().enumerate() {
            c.c[i] = v.to_bits();
        }
        c
    }

    fn sorted(t: &Rtree, cells: &[RtreeCell]) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..cells.len()).collect();
        let mut spare = vec![0usize; cells.len()];
        t.sort_by_dimension(&mut idx, 0, cells, &mut spare);
        idx
    }

    /// `SortByDimension` of rtree.c is a merge sort whose merge takes the RIGHT run unless the
    /// left key is strictly smaller, so equal keys do not keep their order. The expected
    /// permutations are what the C loop yields for these inputs (a stable sort would give
    /// 0..7 for the first one); the order decides which cells go to which half of a split.
    #[test]
    fn sort_by_dimension_breaks_ties_like_the_c_merge() {
        let t = tree(1, false);
        let same: Vec<RtreeCell> = (0..7).map(|i| fcell(i, &[1.0, 2.0])).collect();
        assert_eq!(sorted(&t, &same), vec![6, 5, 4, 3, 2, 1, 0]);
        let mixed: Vec<RtreeCell> = [
            (3.0, 4.0),
            (1.0, 2.0),
            (3.0, 4.0),
            (1.0, 2.0),
            (0.0, 9.0),
            (3.0, 3.0),
            (1.0, 2.0),
            (3.0, 4.0),
        ]
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| fcell(i as i64, &[a, b]))
        .collect();
        assert_eq!(sorted(&t, &mixed), vec![4, 6, 3, 1, 5, 7, 2, 0]);
    }

    /// `cellUnion` in the amalgamation uses `MIN`/`MAX` of sqliteInt.h, which return the
    /// second operand on a tie: the union of a +0.0 box with a -0.0 box stores -0.0 (C writes
    /// the bytes 80 00 00 00 into the parent cell).
    #[test]
    fn cell_union_takes_the_second_operand_on_ties() {
        let t = tree(1, false);
        let mut a = fcell(1, &[0.0, 0.0]);
        let b = fcell(2, &[-0.0, -0.0]);
        t.cell_union(&mut a, &b);
        assert_eq!(a.c[0], 0x8000_0000);
        assert_eq!(a.c[1], 0x8000_0000);
        let mut c = fcell(1, &[-0.0, -0.0]);
        t.cell_union(&mut c, &fcell(2, &[0.0, 0.0]));
        assert_eq!(c.c[0], 0);
        assert_eq!(c.c[1], 0);
    }

    /// `cellArea` subtracts the `float` coordinates as `float` and only then widens; the
    /// overlap and margin use `double` differences (`DCOORD`).
    #[test]
    fn cell_area_subtracts_in_single_precision() {
        let t = tree(1, false);
        let c = fcell(1, &[0.1, 16_777_216.0]);
        assert_eq!(t.cell_area(&c), 16_777_216.0);
        assert!(t.cell_margin(&c) < 16_777_216.0);
        let ti = tree(1, true);
        let mut ci = RtreeCell::zero();
        ci.c[0] = i32::MIN as u32;
        ci.c[1] = i32::MAX as u32;
        assert_eq!(ti.cell_area(&ci), 4_294_967_295.0);
    }
}
