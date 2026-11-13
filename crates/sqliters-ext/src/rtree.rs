//! R-Tree and R*-Tree spatial index virtual table modules (`rtree` and `rtree_i32`)
//! and companion SQL functions (`rtreedepth`, `rtreenode`, `rtreecheck`).
//!
//! Ported from SQLite 3.54.1 `sqlite/ext/rtree/rtree.c` with identical on-disk
//! shadow table layout (`<name>_node`, `<name>_rowid`, `<name>_parent`) and
//! big-endian binary cell representation.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::Write as _;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Arc, Mutex};

use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConflictMode, VTabConstraintOp,
    VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor,
    VirtualTable, VirtualTableModule,
};

const RTREE_MAX_DIMENSIONS: usize = 5;
const RTREE_MAX_COORDS: usize = RTREE_MAX_DIMENSIONS * 2;
const RTREE_MAX_AUX_COLUMN: usize = 100;
const RTREE_MAXCELLS: usize = 51;
const RTREE_MAX_DEPTH: usize = 40;
const RTREE_DEFAULT_ROWEST: i64 = 1_048_576;
const RTREE_MIN_ROWEST: i64 = 100;
const RTREE_CHECK_MAX_ERROR: usize = 100;

/// Coordinate representation for an R-Tree module (`f32` vs `i32`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RTreeCoordMode {
    Float32,
    Int32,
}

#[derive(Debug, Clone, Copy)]
struct RTreeCell {
    id: i64,
    coords: [u32; RTREE_MAX_COORDS],
}

impl RTreeCell {
    #[inline]
    fn zero() -> Self {
        Self {
            id: 0,
            coords: [0; RTREE_MAX_COORDS],
        }
    }

    #[inline]
    fn coord_f64(&self, idx: usize, mode: RTreeCoordMode) -> f64 {
        let bits = self.coords[idx];
        match mode {
            RTreeCoordMode::Float32 => f64::from(f32::from_bits(bits)),
            RTreeCoordMode::Int32 => f64::from(bits as i32),
        }
    }

    #[inline]
    fn coord_sql_value(&self, idx: usize, mode: RTreeCoordMode) -> Value {
        let bits = self.coords[idx];
        match mode {
            RTreeCoordMode::Float32 => Value::Real(f64::from(f32::from_bits(bits))),
            RTreeCoordMode::Int32 => Value::Integer(i64::from(bits as i32)),
        }
    }

    #[inline]
    fn area(&self, n_dim: usize, mode: RTreeCoordMode) -> f64 {
        let mut area = 1.0_f64;
        match mode {
            RTreeCoordMode::Float32 => {
                for d in 0..n_dim {
                    let lo = f64::from(f32::from_bits(self.coords[d * 2]));
                    let hi = f64::from(f32::from_bits(self.coords[d * 2 + 1]));
                    area *= hi - lo;
                }
            }
            RTreeCoordMode::Int32 => {
                for d in 0..n_dim {
                    let lo = f64::from(self.coords[d * 2] as i32);
                    let hi = f64::from(self.coords[d * 2 + 1] as i32);
                    area *= hi - lo;
                }
            }
        }
        area
    }

    #[inline]
    fn margin(&self, n_dim: usize, mode: RTreeCoordMode) -> f64 {
        let mut margin = 0.0_f64;
        match mode {
            RTreeCoordMode::Float32 => {
                for d in 0..n_dim {
                    let lo = f64::from(f32::from_bits(self.coords[d * 2]));
                    let hi = f64::from(f32::from_bits(self.coords[d * 2 + 1]));
                    margin += hi - lo;
                }
            }
            RTreeCoordMode::Int32 => {
                for d in 0..n_dim {
                    let lo = f64::from(self.coords[d * 2] as i32);
                    let hi = f64::from(self.coords[d * 2 + 1] as i32);
                    margin += hi - lo;
                }
            }
        }
        margin
    }

    #[inline]
    fn union_in_place(&mut self, other: &Self, n_dim: usize, mode: RTreeCoordMode) {
        match mode {
            RTreeCoordMode::Float32 => {
                for d in 0..n_dim {
                    let i0 = d * 2;
                    let i1 = i0 + 1;
                    let a0 = f32::from_bits(self.coords[i0]);
                    let b0 = f32::from_bits(other.coords[i0]);
                    if a0 >= b0 {
                        self.coords[i0] = other.coords[i0];
                    }
                    let a1 = f32::from_bits(self.coords[i1]);
                    let b1 = f32::from_bits(other.coords[i1]);
                    if a1 <= b1 {
                        self.coords[i1] = other.coords[i1];
                    }
                }
            }
            RTreeCoordMode::Int32 => {
                for d in 0..n_dim {
                    let i0 = d * 2;
                    let i1 = i0 + 1;
                    let a0 = self.coords[i0] as i32;
                    let b0 = other.coords[i0] as i32;
                    self.coords[i0] = a0.min(b0) as u32;
                    let a1 = self.coords[i1] as i32;
                    let b1 = other.coords[i1] as i32;
                    self.coords[i1] = a1.max(b1) as u32;
                }
            }
        }
    }

    #[inline]
    fn contains(&self, other: &Self, n_dim: usize, mode: RTreeCoordMode) -> bool {
        match mode {
            RTreeCoordMode::Float32 => {
                for d in 0..n_dim {
                    let i0 = d * 2;
                    let i1 = i0 + 1;
                    let a0 = f32::from_bits(self.coords[i0]);
                    let a1 = f32::from_bits(self.coords[i1]);
                    let b0 = f32::from_bits(other.coords[i0]);
                    let b1 = f32::from_bits(other.coords[i1]);
                    if b0 < a0 || b1 > a1 {
                        return false;
                    }
                }
            }
            RTreeCoordMode::Int32 => {
                for d in 0..n_dim {
                    let i0 = d * 2;
                    let i1 = i0 + 1;
                    let a0 = self.coords[i0] as i32;
                    let a1 = self.coords[i1] as i32;
                    let b0 = other.coords[i0] as i32;
                    let b1 = other.coords[i1] as i32;
                    if b0 < a0 || b1 > a1 {
                        return false;
                    }
                }
            }
        }
        true
    }

    #[inline]
    fn overlap(&self, other: &Self, n_dim: usize, mode: RTreeCoordMode) -> f64 {
        let mut prod = 1.0_f64;
        match mode {
            RTreeCoordMode::Float32 => {
                for d in 0..n_dim {
                    let i0 = d * 2;
                    let i1 = i0 + 1;
                    let lo = f64::from(f32::from_bits(self.coords[i0]))
                        .max(f64::from(f32::from_bits(other.coords[i0])));
                    let hi = f64::from(f32::from_bits(self.coords[i1]))
                        .min(f64::from(f32::from_bits(other.coords[i1])));
                    if hi < lo {
                        return 0.0;
                    }
                    prod *= hi - lo;
                }
            }
            RTreeCoordMode::Int32 => {
                for d in 0..n_dim {
                    let i0 = d * 2;
                    let i1 = i0 + 1;
                    let lo =
                        f64::from(self.coords[i0] as i32).max(f64::from(other.coords[i0] as i32));
                    let hi =
                        f64::from(self.coords[i1] as i32).min(f64::from(other.coords[i1] as i32));
                    if hi < lo {
                        return 0.0;
                    }
                    prod *= hi - lo;
                }
            }
        }
        prod
    }
}

/// Hasher for the node-number keyed cache maps (`nodes`, `parents`). Node numbers are small
/// integers allocated by the tree itself, so one multiply-and-fold replaces SipHash there; the
/// rowid map keeps the default keyed hasher because its keys are chosen by the user.
#[derive(Debug, Default, Clone, Copy)]
struct NodeNoHasher(u64);

impl Hasher for NodeNoHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
        self.0 = h;
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        let h = (self.0 ^ v).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = h ^ (h >> 32);
    }

    #[inline]
    fn write_i64(&mut self, v: i64) {
        self.write_u64(v as u64);
    }
}

type NodeNoMap<V> = HashMap<i64, V, BuildHasherDefault<NodeNoHasher>>;

#[derive(Debug, Clone)]
struct CachedNode {
    depth: u16,
    cells: Vec<RTreeCell>,
    dirty: bool,
    /// Coordinates of `compute_bbox(&cells, _)` when known. Only the in-place mutators keep it
    /// up to date; every other writer builds a fresh node with `None`.
    bbox: Option<[u32; RTREE_MAX_COORDS]>,
}

#[derive(Debug, Clone)]
struct CachedRowid {
    nodeno: Option<i64>,
    aux: Option<Vec<Value>>,
    dirty: bool,
}

#[derive(Debug, Clone, Copy)]
struct CachedParent {
    parent: Option<i64>,
    dirty: bool,
}

#[derive(Debug)]
enum RTreeUndo {
    Node(i64, Option<Option<CachedNode>>),
    /// A cell was appended in place to a cached node (undo: pop it, restore depth/dirty).
    NodePush {
        nodeno: i64,
        prev_depth: u16,
        prev_dirty: bool,
    },
    /// One cell of a cached node was overwritten in place (undo: put it back, restore depth/dirty).
    NodeSetCell {
        nodeno: i64,
        pos: usize,
        prev_cell: RTreeCell,
        prev_depth: u16,
        prev_dirty: bool,
    },
    Rowid(i64, Option<CachedRowid>),
    /// The node number of an existing rowid entry was changed in place (aux values untouched).
    RowidNodeno {
        rowid: i64,
        prev_nodeno: Option<i64>,
        prev_dirty: bool,
    },
    Parent(i64, Option<CachedParent>),
    MaxNodeno(Option<i64>),
    MaxRowid(Option<i64>, bool),
    HasDirty(bool),
}

#[derive(Debug, Clone, Copy)]
struct RTreeSavepoint {
    idx: usize,
    undo_pos: usize,
    flushed: bool,
}

#[derive(Debug, Default)]
struct RTreeCache {
    nodes: NodeNoMap<Option<CachedNode>>,
    rowids: HashMap<i64, CachedRowid>,
    parents: NodeNoMap<CachedParent>,
    max_nodeno: Option<i64>,
    max_rowid: Option<i64>,
    deleted_max_rowid: bool,
    root1_blob: Option<Vec<u8>>,
    in_tx: bool,
    has_dirty: bool,
    recording_undo: bool,
    undo_log: Vec<RTreeUndo>,
    savepoints: Vec<RTreeSavepoint>,
    /// Reusable buffer for the root-to-target node path of an insert (no cached state).
    path_scratch: Vec<i64>,
}

impl RTreeCache {
    fn clear_data(&mut self) {
        self.nodes.clear();
        self.rowids.clear();
        self.parents.clear();
        self.max_nodeno = None;
        self.max_rowid = None;
        self.deleted_max_rowid = false;
        self.root1_blob = None;
        self.has_dirty = false;
        self.undo_log.clear();
    }

    #[inline]
    fn should_record_undo(&self) -> bool {
        self.recording_undo || !self.savepoints.is_empty()
    }

    #[inline]
    fn mark_dirty(&mut self) {
        if !self.has_dirty {
            if self.should_record_undo() {
                self.undo_log.push(RTreeUndo::HasDirty(false));
            }
            self.has_dirty = true;
        }
    }

    fn record_node_before(&mut self, nodeno: i64) {
        if self.should_record_undo() {
            let prev = self.nodes.get(&nodeno).cloned();
            self.undo_log.push(RTreeUndo::Node(nodeno, prev));
        }
    }

    fn record_rowid_before(&mut self, rowid: i64) {
        if self.should_record_undo() {
            let prev = self.rowids.get(&rowid).cloned();
            self.undo_log.push(RTreeUndo::Rowid(rowid, prev));
        }
    }

    fn record_parent_before(&mut self, nodeno: i64) {
        if self.should_record_undo() {
            let prev = self.parents.get(&nodeno).copied();
            self.undo_log.push(RTreeUndo::Parent(nodeno, prev));
        }
    }

    fn record_max_nodeno_before(&mut self) {
        if self.should_record_undo() {
            self.undo_log.push(RTreeUndo::MaxNodeno(self.max_nodeno));
        }
    }

    fn record_max_rowid_before(&mut self) {
        if self.should_record_undo() {
            self.undo_log
                .push(RTreeUndo::MaxRowid(self.max_rowid, self.deleted_max_rowid));
        }
    }

    fn rollback_undo_to(&mut self, target_len: usize) {
        while self.undo_log.len() > target_len {
            let Some(item) = self.undo_log.pop() else {
                break;
            };
            match item {
                RTreeUndo::Node(nodeno, prev) => match prev {
                    Some(slot) => {
                        self.nodes.insert(nodeno, slot);
                    }
                    None => {
                        self.nodes.remove(&nodeno);
                    }
                },
                RTreeUndo::NodePush {
                    nodeno,
                    prev_depth,
                    prev_dirty,
                } => {
                    if let Some(Some(node)) = self.nodes.get_mut(&nodeno) {
                        node.cells.pop();
                        node.depth = prev_depth;
                        node.dirty = prev_dirty;
                        node.bbox = None;
                    }
                }
                RTreeUndo::NodeSetCell {
                    nodeno,
                    pos,
                    prev_cell,
                    prev_depth,
                    prev_dirty,
                } => {
                    if let Some(Some(node)) = self.nodes.get_mut(&nodeno) {
                        if let Some(slot) = node.cells.get_mut(pos) {
                            *slot = prev_cell;
                        }
                        node.depth = prev_depth;
                        node.dirty = prev_dirty;
                        node.bbox = None;
                    }
                }
                RTreeUndo::Rowid(rowid, prev) => match prev {
                    Some(entry) => {
                        self.rowids.insert(rowid, entry);
                    }
                    None => {
                        self.rowids.remove(&rowid);
                    }
                },
                RTreeUndo::RowidNodeno {
                    rowid,
                    prev_nodeno,
                    prev_dirty,
                } => {
                    if let Some(entry) = self.rowids.get_mut(&rowid) {
                        entry.nodeno = prev_nodeno;
                        entry.dirty = prev_dirty;
                    }
                }
                RTreeUndo::Parent(nodeno, prev) => match prev {
                    Some(entry) => {
                        self.parents.insert(nodeno, entry);
                    }
                    None => {
                        self.parents.remove(&nodeno);
                    }
                },
                RTreeUndo::MaxNodeno(prev) => {
                    self.max_nodeno = prev;
                }
                RTreeUndo::MaxRowid(prev_max, prev_del) => {
                    self.max_rowid = prev_max;
                    self.deleted_max_rowid = prev_del;
                }
                RTreeUndo::HasDirty(prev) => {
                    self.has_dirty = prev;
                }
            }
        }
    }
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

#[derive(Debug, Clone)]
struct RTreeTable {
    db_name: String,
    table_name: String,
    node_shadow: String,
    rowid_shadow: String,
    parent_shadow: String,
    mode: RTreeCoordMode,
    n_dim: usize,
    n_dim2: usize,
    n_aux: usize,
    bytes_per_cell: usize,
    node_size: usize,
    col_names: Vec<String>,
    cache: Arc<Mutex<RTreeCache>>,
}

impl RTreeTable {
    fn init(
        ctx: &mut dyn VTabHostContext,
        args: &[String],
        mode: RTreeCoordMode,
        is_create: bool,
    ) -> Result<Self> {
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
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "Auxiliary rtree columns must be last",
                ));
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
                match mode {
                    RTreeCoordMode::Float32 => {
                        let _ = write!(declare_sql, ",{coord_tok} REAL");
                    }
                    RTreeCoordMode::Int32 => {
                        let _ = write!(declare_sql, ",{coord_tok} INT");
                    }
                }
            }
        }
        declare_sql.push(')');

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

        let node_shadow = format!("{table_name}_node");
        let rowid_shadow = format!("{table_name}_rowid");
        let parent_shadow = format!("{table_name}_parent");

        let node_size = if is_create {
            let page_rows = ctx.query_sql(&format!("PRAGMA {q_db}.page_size"), &[])?;
            let page_size = page_rows
                .first()
                .and_then(|r| r.first())
                .and_then(Value::as_i64)
                .unwrap_or(4096)
                .max(512) as usize;
            let max_by_cells = 4 + bytes_per_cell * RTREE_MAXCELLS;
            (page_size - 64).min(max_by_cells)
        } else {
            let q_node = quote_ident(&node_shadow);
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
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    format!("undersize RTree blobs in \"{node_shadow}\""),
                ));
            }
            sz as usize
        };

        let mut init_cache = RTreeCache::default();
        if is_create {
            let q_rowid = quote_ident(&rowid_shadow);
            let q_node = quote_ident(&node_shadow);
            let q_parent = quote_ident(&parent_shadow);

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
            let root_zero = vec![0u8; node_size];
            ctx.execute_sql(
                &format!("INSERT INTO {q_db}.{q_node} VALUES(1, ?1)"),
                &[Value::Blob(root_zero.clone())],
            )?;
            init_cache.root1_blob = Some(root_zero);
            init_cache.nodes.insert(
                1,
                Some(CachedNode {
                    depth: 0,
                    cells: Vec::new(),
                    dirty: false,
                    bbox: None,
                }),
            );
            init_cache.max_nodeno = Some(1);
            init_cache.max_rowid = Some(0);
        }

        Ok(Self {
            db_name,
            table_name,
            node_shadow,
            rowid_shadow,
            parent_shadow,
            mode,
            n_dim,
            n_dim2,
            n_aux,
            bytes_per_cell,
            node_size,
            col_names,
            cache: Arc::new(Mutex::new(init_cache)),
        })
    }

    #[inline]
    fn max_cells(&self) -> usize {
        (self.node_size - 4) / self.bytes_per_cell
    }

    #[inline]
    fn min_cells(&self) -> usize {
        (self.max_cells() / 3).max(1)
    }

    fn validate_node_blob(&self, nodeno: i64, data: &[u8]) -> Result<(u16, usize)> {
        if data.len() != self.node_size {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("corrupt RTree node {nodeno} size"),
            ));
        }
        let n_cell = read_u16_be(&data[2..4]) as usize;
        if n_cell > self.max_cells() {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("corrupt RTree node {nodeno} cell count"),
            ));
        }
        let depth = read_u16_be(&data[0..2]);
        if nodeno == 1 && (depth as usize) > RTREE_MAX_DEPTH {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "corrupt RTree root depth",
            ));
        }
        Ok((depth, n_cell))
    }

    fn decode_node_cells(&self, data: &[u8]) -> Vec<RTreeCell> {
        let n_cell = read_u16_be(&data[2..4]) as usize;
        let mut cells = Vec::with_capacity(n_cell);
        let bpc = self.bytes_per_cell;
        let nd2 = self.n_dim2;
        for i in 0..n_cell {
            let base = 4 + i * bpc;
            let id = read_i64_be(&data[base..base + 8]);
            let mut coords = [0u32; RTREE_MAX_COORDS];
            for (j, slot) in coords.iter_mut().enumerate().take(nd2) {
                let c_off = base + 8 + j * 4;
                *slot = u32::from_be_bytes([
                    data[c_off],
                    data[c_off + 1],
                    data[c_off + 2],
                    data[c_off + 3],
                ]);
            }
            cells.push(RTreeCell { id, coords });
        }
        cells
    }

    fn encode_node_cells(&self, depth: u16, cells: &[RTreeCell]) -> Vec<u8> {
        let mut buf = vec![0u8; self.node_size];
        buf[0..2].copy_from_slice(&depth.to_be_bytes());
        buf[2..4].copy_from_slice(&(cells.len() as u16).to_be_bytes());
        let bpc = self.bytes_per_cell;
        let nd2 = self.n_dim2;
        for (i, cell) in cells.iter().enumerate() {
            let base = 4 + i * bpc;
            buf[base..base + 8].copy_from_slice(&cell.id.to_be_bytes());
            for j in 0..nd2 {
                let c_off = base + 8 + j * 4;
                buf[c_off..c_off + 4].copy_from_slice(&cell.coords[j].to_be_bytes());
            }
        }
        buf
    }

    fn ensure_coherent(&self, cache: &mut RTreeCache, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if cache.in_tx && cache.has_dirty {
            return Ok(());
        }
        if let Some(ref root1) = cache.root1_blob
            && cache.nodes.get(&1).is_some_and(Option::is_some)
            && ctx.shadow_rowid_blob_eq(&self.node_shadow, 1, 1, root1)? == Some(true)
        {
            return Ok(());
        }
        let row = ctx.shadow_get_rowid(&self.node_shadow, 1)?;
        let Some(Value::Blob(data)) = row.as_ref().and_then(|r| r.get(1)) else {
            cache.clear_data();
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "missing or invalid RTree node 1",
            ));
        };
        let (depth, _) = match self.validate_node_blob(1, data) {
            Ok(v) => v,
            Err(e) => {
                cache.clear_data();
                return Err(e);
            }
        };
        if cache.root1_blob.as_deref() == Some(data.as_slice()) {
            cache.nodes.entry(1).or_insert_with(|| {
                let cells = self.decode_node_cells(data);
                Some(CachedNode {
                    depth,
                    cells,
                    dirty: false,
                    bbox: None,
                })
            });
            return Ok(());
        }
        cache.clear_data();
        let cells = self.decode_node_cells(data);
        cache.root1_blob = Some(data.clone());
        cache.nodes.insert(
            1,
            Some(CachedNode {
                depth,
                cells,
                dirty: false,
                bbox: None,
            }),
        );
        Ok(())
    }

    /// Returns the cached node `nodeno`, reading and decoding it from the node shadow table on a
    /// cache miss. One map probe on a hit.
    fn cached_node_mut<'a>(
        &self,
        nodes: &'a mut NodeNoMap<Option<CachedNode>>,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
    ) -> Result<&'a mut CachedNode> {
        if nodeno <= 0 {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "corrupt rtree node id",
            ));
        }
        let slot = match nodes.entry(nodeno) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(v) => {
                let row = ctx.shadow_get_rowid(&self.node_shadow, nodeno)?;
                let Some(Value::Blob(data)) = row.as_ref().and_then(|r| r.get(1)) else {
                    return Err(SqliteError::new(
                        ErrorCode::Corrupt,
                        format!("missing or invalid RTree node {nodeno}"),
                    ));
                };
                let (depth, _) = self.validate_node_blob(nodeno, data)?;
                let cells = self.decode_node_cells(data);
                v.insert(Some(CachedNode {
                    depth,
                    cells,
                    dirty: false,
                    bbox: None,
                }))
            }
        };
        match slot {
            Some(node) => Ok(node),
            None => Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("missing or invalid RTree node {nodeno}"),
            )),
        }
    }

    #[inline]
    fn load_node_into_cache(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
    ) -> Result<()> {
        self.cached_node_mut(&mut cache.nodes, ctx, nodeno)
            .map(|_| ())
    }

    fn read_node_decoded(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
    ) -> Result<(u16, Vec<RTreeCell>)> {
        self.load_node_into_cache(cache, ctx, nodeno)?;
        let node = cache
            .nodes
            .get(&nodeno)
            .and_then(|s| s.as_ref())
            .ok_or_else(|| {
                SqliteError::new(
                    ErrorCode::Corrupt,
                    format!("missing or invalid RTree node {nodeno}"),
                )
            })?;
        Ok((node.depth, node.cells.clone()))
    }

    fn put_node(&self, cache: &mut RTreeCache, nodeno: i64, depth: u16, cells: Vec<RTreeCell>) {
        cache.mark_dirty();
        cache.record_node_before(nodeno);
        if let Some(cur_max) = cache.max_nodeno
            && nodeno > cur_max
        {
            cache.record_max_nodeno_before();
            cache.max_nodeno = Some(nodeno);
        }
        cache.nodes.insert(
            nodeno,
            Some(CachedNode {
                depth,
                cells,
                dirty: true,
                bbox: None,
            }),
        );
    }

    /// In-place equivalent of `put_node(nodeno, depth, old_cells + [cell])` for a node that is
    /// already cached: same dirty/undo/max-nodeno bookkeeping, without copying the cell array.
    /// Returns the node's bounding box (id = `nodeno`) when `want_bbox` is set.
    fn push_cell_in_place(
        &self,
        cache: &mut RTreeCache,
        nodeno: i64,
        depth: u16,
        cell: RTreeCell,
        want_bbox: bool,
    ) -> Result<Option<RTreeCell>> {
        let record = cache.should_record_undo();
        let Some(Some(node)) = cache.nodes.get_mut(&nodeno) else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("missing or invalid RTree node {nodeno}"),
            ));
        };
        if !cache.has_dirty {
            if record {
                cache.undo_log.push(RTreeUndo::HasDirty(false));
            }
            cache.has_dirty = true;
        }
        if record {
            cache.undo_log.push(RTreeUndo::NodePush {
                nodeno,
                prev_depth: node.depth,
                prev_dirty: node.dirty,
            });
            if let Some(cur_max) = cache.max_nodeno
                && nodeno > cur_max
            {
                cache.undo_log.push(RTreeUndo::MaxNodeno(cache.max_nodeno));
            }
        }
        if let Some(cur_max) = cache.max_nodeno
            && nodeno > cur_max
        {
            cache.max_nodeno = Some(nodeno);
        }
        let had_cells = !node.cells.is_empty();
        node.cells.push(cell);
        node.depth = depth;
        node.dirty = true;
        // `compute_bbox` folds the cells left to right, so appending one cell continues the fold
        // from the cached box and gives exactly the bits a full recomputation would.
        match node.bbox {
            Some(prev) if had_cells => {
                let mut bbox = RTreeCell {
                    id: nodeno,
                    coords: prev,
                };
                bbox.union_in_place(&cell, self.n_dim, self.mode);
                node.bbox = Some(bbox.coords);
                Ok(want_bbox.then_some(bbox))
            }
            _ if want_bbox => {
                let bbox = self.compute_bbox(&node.cells, nodeno);
                node.bbox = Some(bbox.coords);
                Ok(Some(bbox))
            }
            _ => {
                node.bbox = None;
                Ok(None)
            }
        }
    }

    /// In-place equivalent of replacing the entry for `child_id` in cached node `nodeno` with
    /// `child_bbox` followed by `put_node(nodeno, depth, cells)`. Returns the node's new bounding
    /// box (id = `nodeno`). Nothing is modified when the child entry is missing.
    fn set_child_cell_in_place(
        &self,
        cache: &mut RTreeCache,
        nodeno: i64,
        depth: u16,
        child_id: i64,
        child_bbox: RTreeCell,
    ) -> Result<RTreeCell> {
        let record = cache.should_record_undo();
        let Some(Some(node)) = cache.nodes.get_mut(&nodeno) else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("missing or invalid RTree node {nodeno}"),
            ));
        };
        let Some(pos) = node.cells.iter().position(|c| c.id == child_id) else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "missing child entry in parent RTree node",
            ));
        };
        if !cache.has_dirty {
            if record {
                cache.undo_log.push(RTreeUndo::HasDirty(false));
            }
            cache.has_dirty = true;
        }
        if record {
            cache.undo_log.push(RTreeUndo::NodeSetCell {
                nodeno,
                pos,
                prev_cell: node.cells[pos],
                prev_depth: node.depth,
                prev_dirty: node.dirty,
            });
            if let Some(cur_max) = cache.max_nodeno
                && nodeno > cur_max
            {
                cache.undo_log.push(RTreeUndo::MaxNodeno(cache.max_nodeno));
            }
        }
        if let Some(cur_max) = cache.max_nodeno
            && nodeno > cur_max
        {
            cache.max_nodeno = Some(nodeno);
        }
        let unchanged =
            node.cells[pos].id == child_bbox.id && node.cells[pos].coords == child_bbox.coords;
        node.cells[pos] = child_bbox;
        node.depth = depth;
        node.dirty = true;
        match node.bbox {
            Some(coords) if unchanged => Ok(RTreeCell { id: nodeno, coords }),
            _ => {
                let bbox = self.compute_bbox(&node.cells, nodeno);
                node.bbox = Some(bbox.coords);
                Ok(bbox)
            }
        }
    }

    fn alloc_new_node(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        cells: Vec<RTreeCell>,
    ) -> Result<i64> {
        let cur_max = match cache.max_nodeno {
            Some(m) => m,
            None => {
                let mut m = ctx.shadow_max_rowid(&self.node_shadow)?.unwrap_or(1);
                for (&id, slot) in &cache.nodes {
                    if slot.is_some() && id > m {
                        m = id;
                    }
                }
                cache.record_max_nodeno_before();
                cache.max_nodeno = Some(m);
                m
            }
        };
        let next_id = (cur_max + 1).max(2);
        cache.record_max_nodeno_before();
        cache.max_nodeno = Some(next_id);
        self.put_node(cache, next_id, 0, cells);
        Ok(next_id)
    }

    fn delete_node(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
    ) -> Result<()> {
        cache.mark_dirty();
        cache.record_node_before(nodeno);
        cache.nodes.insert(nodeno, None);
        if cache.max_nodeno == Some(nodeno) {
            let mut new_max = 1_i64;
            for probe in (1..nodeno).rev() {
                match cache.nodes.get(&probe) {
                    Some(Some(_)) => {
                        new_max = probe;
                        break;
                    }
                    Some(None) => continue,
                    None => {
                        if ctx.shadow_get_rowid(&self.node_shadow, probe)?.is_some() {
                            new_max = probe;
                            break;
                        }
                    }
                }
            }
            cache.record_max_nodeno_before();
            cache.max_nodeno = Some(new_max);
        }
        Ok(())
    }

    fn read_rowid_nodeno(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<i64>> {
        // A cached `max_rowid` bounds every live rowid (shadow table and cache), so a larger
        // rowid cannot be mapped anywhere.
        if let Some(cur_max) = cache.max_rowid
            && rowid > cur_max
        {
            return Ok(None);
        }
        if let Some(entry) = cache.rowids.get(&rowid) {
            return Ok(entry.nodeno);
        }
        let Some(row) = ctx.shadow_get_rowid(&self.rowid_shadow, rowid)? else {
            return Ok(None);
        };
        let nodeno = row.get(1).and_then(Value::as_i64);
        let aux = if self.n_aux > 0 {
            Some(
                (0..self.n_aux)
                    .map(|i| row.get(2 + i).cloned().unwrap_or(Value::Null))
                    .collect(),
            )
        } else {
            None
        };
        cache.rowids.insert(
            rowid,
            CachedRowid {
                nodeno,
                aux,
                dirty: false,
            },
        );
        Ok(nodeno)
    }

    fn read_rowid_aux(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Vec<Value>> {
        if let Some(entry) = cache.rowids.get(&rowid)
            && let Some(ref aux) = entry.aux
        {
            return Ok(aux.clone());
        }
        let row = ctx.shadow_get_rowid(&self.rowid_shadow, rowid)?;
        let mut aux = Vec::with_capacity(self.n_aux);
        if let Some(ref r) = row {
            for i in 0..self.n_aux {
                aux.push(r.get(2 + i).cloned().unwrap_or(Value::Null));
            }
            let nodeno = cache
                .rowids
                .get(&rowid)
                .and_then(|e| e.nodeno)
                .or_else(|| r.get(1).and_then(Value::as_i64));
            let was_dirty = cache.rowids.get(&rowid).is_some_and(|e| e.dirty);
            cache.rowids.insert(
                rowid,
                CachedRowid {
                    nodeno,
                    aux: Some(aux.clone()),
                    dirty: was_dirty,
                },
            );
        } else {
            aux.resize(self.n_aux, Value::Null);
        }
        Ok(aux)
    }

    fn write_rowid_mapping(&self, cache: &mut RTreeCache, rowid: i64, nodeno: i64) {
        cache.mark_dirty();
        let record = cache.should_record_undo();
        match cache.rowids.entry(rowid) {
            Entry::Occupied(mut e) => {
                let entry = e.get_mut();
                if record {
                    cache.undo_log.push(RTreeUndo::RowidNodeno {
                        rowid,
                        prev_nodeno: entry.nodeno,
                        prev_dirty: entry.dirty,
                    });
                }
                entry.nodeno = Some(nodeno);
                entry.dirty = true;
            }
            Entry::Vacant(v) => {
                if record {
                    cache.undo_log.push(RTreeUndo::Rowid(rowid, None));
                }
                v.insert(CachedRowid {
                    nodeno: Some(nodeno),
                    aux: None,
                    dirty: true,
                });
            }
        }
        if let Some(cur_max) = cache.max_rowid
            && rowid > cur_max
        {
            cache.record_max_rowid_before();
            cache.max_rowid = Some(rowid);
            cache.deleted_max_rowid = false;
        }
    }

    fn write_aux_values(&self, cache: &mut RTreeCache, rowid: i64, aux_vals: &[Value]) {
        if self.n_aux == 0 {
            return;
        }
        cache.mark_dirty();
        cache.record_rowid_before(rowid);
        let mut aux = Vec::with_capacity(self.n_aux);
        for i in 0..self.n_aux {
            aux.push(aux_vals.get(i).cloned().unwrap_or(Value::Null));
        }
        let prev_nodeno = cache.rowids.get(&rowid).and_then(|e| e.nodeno);
        cache.rowids.insert(
            rowid,
            CachedRowid {
                nodeno: prev_nodeno,
                aux: Some(aux),
                dirty: true,
            },
        );
    }

    fn delete_rowid_mapping(&self, cache: &mut RTreeCache, rowid: i64) {
        cache.mark_dirty();
        cache.record_rowid_before(rowid);
        cache.rowids.insert(
            rowid,
            CachedRowid {
                nodeno: None,
                aux: None,
                dirty: true,
            },
        );
        if cache.max_rowid == Some(rowid) {
            cache.record_max_rowid_before();
            cache.deleted_max_rowid = true;
        }
    }

    fn read_parent_nodeno(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
    ) -> Result<Option<i64>> {
        if let Some(entry) = cache.parents.get(&nodeno) {
            return Ok(entry.parent);
        }
        let row = ctx.shadow_get_rowid(&self.parent_shadow, nodeno)?;
        let parent = row.as_ref().and_then(|r| r.get(1)).and_then(Value::as_i64);
        cache.parents.insert(
            nodeno,
            CachedParent {
                parent,
                dirty: false,
            },
        );
        Ok(parent)
    }

    fn write_parent_mapping(&self, cache: &mut RTreeCache, nodeno: i64, parent: i64) {
        cache.mark_dirty();
        let record = cache.should_record_undo();
        let new_entry = CachedParent {
            parent: Some(parent),
            dirty: true,
        };
        match cache.parents.entry(nodeno) {
            Entry::Occupied(mut e) => {
                if record {
                    cache
                        .undo_log
                        .push(RTreeUndo::Parent(nodeno, Some(*e.get())));
                }
                *e.get_mut() = new_entry;
            }
            Entry::Vacant(v) => {
                if record {
                    cache.undo_log.push(RTreeUndo::Parent(nodeno, None));
                }
                v.insert(new_entry);
            }
        }
    }

    fn delete_parent_mapping(&self, cache: &mut RTreeCache, nodeno: i64) {
        cache.mark_dirty();
        cache.record_parent_before(nodeno);
        cache.parents.insert(
            nodeno,
            CachedParent {
                parent: None,
                dirty: true,
            },
        );
    }

    fn update_child_mapping(
        &self,
        cache: &mut RTreeCache,
        child_id: i64,
        node_id: i64,
        height: usize,
    ) {
        if height == 0 {
            self.write_rowid_mapping(cache, child_id, node_id);
        } else {
            self.write_parent_mapping(cache, child_id, node_id);
        }
    }

    fn flush_dirty(&self, cache: &mut RTreeCache, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if !cache.has_dirty {
            return Ok(());
        }

        let mut dirty_node_ids: Vec<i64> = cache
            .nodes
            .iter()
            .filter_map(|(&id, slot)| match slot {
                Some(n) if n.dirty => Some(id),
                None => Some(id),
                _ => None,
            })
            .collect();
        dirty_node_ids.sort_unstable();

        let mut new_root1: Option<Vec<u8>> = None;
        for nodeno in dirty_node_ids {
            match cache.nodes.get(&nodeno) {
                Some(Some(node)) => {
                    let encoded = self.encode_node_cells(node.depth, &node.cells);
                    if nodeno == 1 {
                        new_root1 = Some(encoded.clone());
                    }
                    ctx.shadow_put_rowid(
                        &self.node_shadow,
                        nodeno,
                        &[Value::Integer(nodeno), Value::Blob(encoded)],
                        true,
                    )?;
                }
                Some(None) => {
                    ctx.shadow_delete_rowid(&self.node_shadow, nodeno)?;
                }
                None => {}
            }
        }
        cache.nodes.retain(|_, slot| {
            if let Some(n) = slot {
                n.dirty = false;
                true
            } else {
                false
            }
        });
        if let Some(r1) = new_root1 {
            cache.root1_blob = Some(r1);
        }

        let mut dirty_rowids: Vec<i64> = cache
            .rowids
            .iter()
            .filter_map(|(&id, entry)| if entry.dirty { Some(id) } else { None })
            .collect();
        dirty_rowids.sort_unstable();

        if self.n_aux == 0 {
            for rowid in dirty_rowids {
                if let Some(entry) = cache.rowids.get(&rowid) {
                    match entry.nodeno {
                        Some(nodeno) => {
                            ctx.shadow_put_rowid(
                                &self.rowid_shadow,
                                rowid,
                                &[Value::Integer(rowid), Value::Integer(nodeno)],
                                true,
                            )?;
                        }
                        None => {
                            ctx.shadow_delete_rowid(&self.rowid_shadow, rowid)?;
                        }
                    }
                }
            }
        } else {
            let mut row_buf = Vec::with_capacity(2 + self.n_aux);
            for rowid in dirty_rowids {
                let (nodeno_opt, aux_opt) = match cache.rowids.get(&rowid) {
                    Some(e) => (e.nodeno, e.aux.clone()),
                    None => continue,
                };
                match nodeno_opt {
                    Some(nodeno) => {
                        let aux_vals = match aux_opt {
                            Some(a) => a,
                            None => self.read_rowid_aux(cache, ctx, rowid)?,
                        };
                        row_buf.clear();
                        row_buf.push(Value::Integer(rowid));
                        row_buf.push(Value::Integer(nodeno));
                        for i in 0..self.n_aux {
                            row_buf.push(aux_vals.get(i).cloned().unwrap_or(Value::Null));
                        }
                        ctx.shadow_put_rowid(&self.rowid_shadow, rowid, &row_buf, true)?;
                    }
                    None => {
                        ctx.shadow_delete_rowid(&self.rowid_shadow, rowid)?;
                    }
                }
            }
        }
        cache.rowids.retain(|_, entry| {
            if entry.nodeno.is_some() {
                entry.dirty = false;
                true
            } else {
                false
            }
        });

        let mut dirty_parents: Vec<i64> = cache
            .parents
            .iter()
            .filter_map(|(&id, entry)| if entry.dirty { Some(id) } else { None })
            .collect();
        dirty_parents.sort_unstable();

        for nodeno in dirty_parents {
            if let Some(entry) = cache.parents.get(&nodeno) {
                match entry.parent {
                    Some(parent) => {
                        ctx.shadow_put_rowid(
                            &self.parent_shadow,
                            nodeno,
                            &[Value::Integer(nodeno), Value::Integer(parent)],
                            true,
                        )?;
                    }
                    None => {
                        ctx.shadow_delete_rowid(&self.parent_shadow, nodeno)?;
                    }
                }
            }
        }
        cache.parents.retain(|_, entry| {
            if entry.parent.is_some() {
                entry.dirty = false;
                true
            } else {
                false
            }
        });

        cache.has_dirty = false;
        if cache.deleted_max_rowid {
            cache.max_rowid = None;
            cache.deleted_max_rowid = false;
        }
        for sp in &mut cache.savepoints {
            sp.flushed = true;
        }
        cache.undo_log.clear();
        Ok(())
    }

    #[inline]
    fn compute_bbox(&self, cells: &[RTreeCell], node_id: i64) -> RTreeCell {
        let mut bbox = cells[0];
        for cell in &cells[1..] {
            bbox.union_in_place(cell, self.n_dim, self.mode);
        }
        bbox.id = node_id;
        bbox
    }

    fn choose_subtree_path(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        cell: &RTreeCell,
        target_height: usize,
        path: &mut Vec<i64>,
    ) -> Result<usize> {
        path.clear();
        let root_depth = self.cached_node_mut(&mut cache.nodes, ctx, 1)?.depth as usize;
        if target_height > root_depth {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "corrupt RTree subtree height",
            ));
        }
        let steps = root_depth - target_height;
        path.reserve(steps + 1);
        path.push(1);
        let mut cur_id = 1_i64;
        let n_dim = self.n_dim;
        let mode = self.mode;
        for step in 0..steps {
            let best_id = {
                let cells = &self.cached_node_mut(&mut cache.nodes, ctx, cur_id)?.cells;
                if cells.is_empty() {
                    return Err(SqliteError::new(
                        ErrorCode::Corrupt,
                        "corrupt empty internal RTree node",
                    ));
                }
                let mut best_id = cells[0].id;
                let mut found_containing = false;
                let mut min_area = 0.0_f64;
                for c in cells {
                    if c.contains(cell, n_dim, mode) {
                        let area = c.area(n_dim, mode);
                        if !found_containing || area < min_area {
                            best_id = c.id;
                            min_area = area;
                            found_containing = true;
                        }
                    }
                }
                if !found_containing {
                    let mut min_growth = 0.0_f64;
                    for (idx, c) in cells.iter().enumerate() {
                        let area = c.area(n_dim, mode);
                        let mut u = *c;
                        u.union_in_place(cell, n_dim, mode);
                        let growth = u.area(n_dim, mode) - area;
                        if idx == 0
                            || growth < min_growth
                            || (growth == min_growth && area < min_area)
                        {
                            min_growth = growth;
                            min_area = area;
                            best_id = c.id;
                        }
                    }
                }
                best_id
            };
            if path.contains(&best_id) {
                return Err(SqliteError::new(ErrorCode::Corrupt, "cycle in RTree"));
            }
            path.push(best_id);
            if step + 1 < steps {
                cur_id = best_id;
            }
        }
        Ok(root_depth)
    }

    fn split_cells_rstar(&self, all_cells: &[RTreeCell]) -> (Vec<RTreeCell>, Vec<RTreeCell>) {
        let n_cell = all_cells.len();
        let min_c = self.min_cells().min(n_cell / 2).max(1);
        let max_left = (n_cell - min_c).max(min_c);
        let n_dim = self.n_dim;
        let mode = self.mode;

        if n_cell <= 64 {
            let mut best_split = min_c;
            let mut best_margin = 0.0_f64;
            let mut best_idxs = [0usize; 64];
            let mut cur_idxs = [0usize; 64];
            let mut suffix_boxes = [RTreeCell::zero(); 64];
            let mut keyed = [(0_i64, 0_i64, 0_usize); 64];

            for dim in 0..n_dim {
                sort_cell_indexes_by_dim(all_cells, dim * 2, mode, &mut keyed, &mut cur_idxs);
                let idxs = &cur_idxs[..n_cell];

                suffix_boxes[n_cell - 1] = all_cells[idxs[n_cell - 1]];
                for k in (min_c..n_cell - 1).rev() {
                    let mut b = all_cells[idxs[k]];
                    b.union_in_place(&suffix_boxes[k + 1], n_dim, mode);
                    suffix_boxes[k] = b;
                }

                let mut left_box = all_cells[idxs[0]];
                for &k in &idxs[1..min_c] {
                    left_box.union_in_place(&all_cells[k], n_dim, mode);
                }

                let mut margin_sum = 0.0_f64;
                let mut dim_best_left = min_c;
                let mut dim_best_overlap = 0.0_f64;
                let mut dim_best_area = 0.0_f64;

                for n_left in min_c..=max_left {
                    if n_left > min_c {
                        left_box.union_in_place(&all_cells[idxs[n_left - 1]], n_dim, mode);
                    }
                    let right_box = &suffix_boxes[n_left];
                    margin_sum += left_box.margin(n_dim, mode) + right_box.margin(n_dim, mode);
                    let overlap = left_box.overlap(right_box, n_dim, mode);
                    let area = left_box.area(n_dim, mode) + right_box.area(n_dim, mode);
                    if n_left == min_c
                        || overlap < dim_best_overlap
                        || (overlap == dim_best_overlap && area < dim_best_area)
                    {
                        dim_best_left = n_left;
                        dim_best_overlap = overlap;
                        dim_best_area = area;
                    }
                }

                if dim == 0 || margin_sum < best_margin {
                    best_margin = margin_sum;
                    best_split = dim_best_left;
                    best_idxs[..n_cell].copy_from_slice(idxs);
                }
            }

            let left_cells = best_idxs[..best_split]
                .iter()
                .map(|&i| all_cells[i])
                .collect();
            let right_cells = best_idxs[best_split..n_cell]
                .iter()
                .map(|&i| all_cells[i])
                .collect();
            return (left_cells, right_cells);
        }

        let mut best_split = min_c;
        let mut best_margin = 0.0_f64;
        let mut best_idxs: Vec<usize> = Vec::new();
        for dim in 0..n_dim {
            let mut idxs: Vec<usize> = (0..n_cell).collect();
            let d0 = dim * 2;
            let d1 = d0 + 1;
            idxs.sort_by(|&a, &b| {
                let a0 = all_cells[a].coord_f64(d0, mode);
                let a1 = all_cells[a].coord_f64(d1, mode);
                let b0 = all_cells[b].coord_f64(d0, mode);
                let b1 = all_cells[b].coord_f64(d1, mode);
                a0.total_cmp(&b0).then_with(|| a1.total_cmp(&b1))
            });
            let mut suffix_boxes = vec![RTreeCell::zero(); n_cell];
            suffix_boxes[n_cell - 1] = all_cells[idxs[n_cell - 1]];
            for k in (min_c..n_cell - 1).rev() {
                let mut b = all_cells[idxs[k]];
                b.union_in_place(&suffix_boxes[k + 1], n_dim, mode);
                suffix_boxes[k] = b;
            }
            let mut left_box = all_cells[idxs[0]];
            for &k in &idxs[1..min_c] {
                left_box.union_in_place(&all_cells[k], n_dim, mode);
            }
            let mut margin_sum = 0.0_f64;
            let mut dim_best_left = min_c;
            let mut dim_best_overlap = 0.0_f64;
            let mut dim_best_area = 0.0_f64;
            for n_left in min_c..=max_left {
                if n_left > min_c {
                    left_box.union_in_place(&all_cells[idxs[n_left - 1]], n_dim, mode);
                }
                let right_box = &suffix_boxes[n_left];
                margin_sum += left_box.margin(n_dim, mode) + right_box.margin(n_dim, mode);
                let overlap = left_box.overlap(right_box, n_dim, mode);
                let area = left_box.area(n_dim, mode) + right_box.area(n_dim, mode);
                if n_left == min_c
                    || overlap < dim_best_overlap
                    || (overlap == dim_best_overlap && area < dim_best_area)
                {
                    dim_best_left = n_left;
                    dim_best_overlap = overlap;
                    dim_best_area = area;
                }
            }
            if dim == 0 || margin_sum < best_margin {
                best_margin = margin_sum;
                best_split = dim_best_left;
                best_idxs = idxs;
            }
        }
        let left_cells = best_idxs[..best_split]
            .iter()
            .map(|&i| all_cells[i])
            .collect();
        let right_cells = best_idxs[best_split..]
            .iter()
            .map(|&i| all_cells[i])
            .collect();
        (left_cells, right_cells)
    }

    fn adjust_ancestors(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        ancestors: &[i64],
        mut child_id: i64,
        mut child_bbox: RTreeCell,
        root_depth: usize,
    ) -> Result<()> {
        for &parent_id in ancestors.iter().rev() {
            self.load_node_into_cache(cache, ctx, parent_id)?;
            let depth_field = if parent_id == 1 { root_depth as u16 } else { 0 };
            let next_bbox =
                self.set_child_cell_in_place(cache, parent_id, depth_field, child_id, child_bbox)?;
            child_id = parent_id;
            child_bbox = next_bbox;
        }
        Ok(())
    }

    fn insert_cell_at_height(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        cell: RTreeCell,
        target_height: usize,
    ) -> Result<()> {
        let mut path = std::mem::take(&mut cache.path_scratch);
        let res = self
            .choose_subtree_path(cache, ctx, &cell, target_height, &mut path)
            .and_then(|root_depth| {
                self.insert_into_path(cache, ctx, &path, cell, target_height, root_depth)
            });
        cache.path_scratch = path;
        res
    }

    fn insert_into_path(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        path: &[i64],
        cell: RTreeCell,
        height: usize,
        root_depth: usize,
    ) -> Result<()> {
        let node_id = *path.last().unwrap_or(&1);
        let ancestors = &path[..path.len().saturating_sub(1)];
        let n_cells = self
            .cached_node_mut(&mut cache.nodes, ctx, node_id)?
            .cells
            .len();

        if n_cells < self.max_cells() {
            let depth_field = if node_id == 1 { root_depth as u16 } else { 0 };
            let bbox_opt =
                self.push_cell_in_place(cache, node_id, depth_field, cell, !ancestors.is_empty())?;
            self.update_child_mapping(cache, cell.id, node_id, height);
            if let Some(bbox) = bbox_opt {
                self.adjust_ancestors(cache, ctx, ancestors, node_id, bbox, root_depth)?;
            }
            return Ok(());
        }

        let mut cells = Vec::with_capacity(n_cells + 1);
        if let Some(Some(node)) = cache.nodes.get(&node_id) {
            cells.extend_from_slice(&node.cells);
        }
        cells.push(cell);
        let (left_cells, right_cells) = self.split_cells_rstar(&cells);

        if node_id == 1 {
            if root_depth + 1 > RTREE_MAX_DEPTH {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    "RTree depth limit exceeded",
                ));
            }
            let left_bbox_no_id = self.compute_bbox(&left_cells, 0);
            let right_bbox_no_id = self.compute_bbox(&right_cells, 0);

            let left_id = self.alloc_new_node(cache, ctx, left_cells.clone())?;
            let right_id = self.alloc_new_node(cache, ctx, right_cells.clone())?;

            for c in &left_cells {
                self.update_child_mapping(cache, c.id, left_id, height);
            }
            for c in &right_cells {
                self.update_child_mapping(cache, c.id, right_id, height);
            }

            let mut left_bbox = left_bbox_no_id;
            left_bbox.id = left_id;
            let mut right_bbox = right_bbox_no_id;
            right_bbox.id = right_id;
            let new_depth = (root_depth + 1) as u16;
            let root_cells = vec![left_bbox, right_bbox];
            self.put_node(cache, 1, new_depth, root_cells);
            self.write_parent_mapping(cache, left_id, 1);
            self.write_parent_mapping(cache, right_id, 1);
            Ok(())
        } else {
            let left_bbox = self.compute_bbox(&left_cells, node_id);
            for c in &left_cells {
                self.update_child_mapping(cache, c.id, node_id, height);
            }
            self.put_node(cache, node_id, 0, left_cells);

            let right_id = self.alloc_new_node(cache, ctx, right_cells.clone())?;
            for c in &right_cells {
                self.update_child_mapping(cache, c.id, right_id, height);
            }
            let right_bbox = self.compute_bbox(&right_cells, right_id);

            self.adjust_ancestors(cache, ctx, ancestors, node_id, left_bbox, root_depth)?;
            self.insert_into_path(cache, ctx, ancestors, right_bbox, height + 1, root_depth)
        }
    }

    fn build_leaf_path(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        leaf_id: i64,
    ) -> Result<Vec<i64>> {
        let mut rev_path = vec![leaf_id];
        let mut cur = leaf_id;
        while cur != 1 {
            let Some(parent) = self.read_parent_nodeno(cache, ctx, cur)? else {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    format!("missing parent mapping for RTree node {cur}"),
                ));
            };
            if rev_path.contains(&parent) || rev_path.len() > RTREE_MAX_DEPTH + 2 {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    "corrupt parent chain in RTree",
                ));
            }
            rev_path.push(parent);
            cur = parent;
        }
        rev_path.reverse();
        Ok(rev_path)
    }

    fn delete_rowid_entry(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<()> {
        self.load_node_into_cache(cache, ctx, 1)?;
        let mut root_depth = cache
            .nodes
            .get(&1)
            .and_then(|s| s.as_ref())
            .map(|n| n.depth as usize)
            .ok_or_else(|| {
                SqliteError::new(ErrorCode::Corrupt, "missing or invalid RTree node 1")
            })?;

        let Some(leaf_id) = self.read_rowid_nodeno(cache, ctx, rowid)? else {
            return Ok(());
        };
        let path = self.build_leaf_path(cache, ctx, leaf_id)?;
        let mut orphaned: Vec<(usize, RTreeCell)> = Vec::new();

        self.remove_entry_along_path(cache, ctx, &path, rowid, 0, &mut root_depth, &mut orphaned)?;
        self.delete_rowid_mapping(cache, rowid);

        // Shrink root height if root is internal and has only 1 child left
        loop {
            if root_depth == 0 {
                break;
            }
            let (_, r_cells) = self.read_node_decoded(cache, ctx, 1)?;
            if r_cells.len() != 1 {
                break;
            }
            let sole_child_id = r_cells[0].id;
            let (_, child_cells) = self.read_node_decoded(cache, ctx, sole_child_id)?;
            self.delete_node(cache, ctx, sole_child_id)?;
            self.delete_parent_mapping(cache, sole_child_id);
            root_depth -= 1;
            for c in &child_cells {
                self.update_child_mapping(cache, c.id, 1, root_depth);
            }
            self.put_node(cache, 1, root_depth as u16, child_cells);
        }

        // Reinsert orphaned entries (from higher subtrees down to leaves, matching rtree.c pDeleted LIFO order)
        for (h, cell) in orphaned.into_iter().rev() {
            self.insert_cell_at_height(cache, ctx, cell, h)?;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn remove_entry_along_path(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        path: &[i64],
        target_id: i64,
        height: usize,
        root_depth: &mut usize,
        orphaned: &mut Vec<(usize, RTreeCell)>,
    ) -> Result<()> {
        let node_id = *path.last().unwrap_or(&1);
        let ancestors = &path[..path.len().saturating_sub(1)];
        let (_, mut cells) = self.read_node_decoded(cache, ctx, node_id)?;
        let Some(pos) = cells.iter().position(|c| c.id == target_id) else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("missing entry {target_id} in RTree node {node_id}"),
            ));
        };
        cells.remove(pos);

        if node_id == 1 {
            if cells.is_empty() && *root_depth > 0 {
                *root_depth = 0;
            }
            self.put_node(cache, 1, *root_depth as u16, cells);
            return Ok(());
        }

        if cells.len() < self.min_cells() {
            for rem in cells {
                orphaned.push((height, rem));
            }
            self.delete_node(cache, ctx, node_id)?;
            self.delete_parent_mapping(cache, node_id);
            self.remove_entry_along_path(
                cache,
                ctx,
                ancestors,
                node_id,
                height + 1,
                root_depth,
                orphaned,
            )
        } else {
            let bbox = self.compute_bbox(&cells, node_id);
            self.put_node(cache, node_id, 0, cells);
            self.adjust_ancestors(cache, ctx, ancestors, node_id, bbox, *root_depth)
        }
    }

    fn allocate_new_rowid(
        &self,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<i64> {
        if cache.deleted_max_rowid {
            self.flush_dirty(cache, ctx)?;
        }
        let cur_max = match cache.max_rowid {
            Some(m) => m,
            None => {
                let mut m = ctx.shadow_max_rowid(&self.rowid_shadow)?.unwrap_or(0);
                for (&id, entry) in &cache.rowids {
                    if entry.nodeno.is_some() && id > m {
                        m = id;
                    }
                }
                cache.record_max_rowid_before();
                cache.max_rowid = Some(m);
                m
            }
        };
        if cur_max == i64::MAX {
            // C allocates through `INSERT INTO %_rowid VALUES(NULL, NULL)`, i.e. OP_NewRowid:
            // with the largest rowid in use, up to 100 random positive candidates are tried and
            // the insert fails with SQLITE_FULL when all of them exist.
            for _ in 0..100 {
                let probe = (rtree_random_i64() & (i64::MAX >> 1)) + 1;
                if self.read_rowid_nodeno(cache, ctx, probe)?.is_none() {
                    return Ok(probe);
                }
            }
            return Err(SqliteError::new(
                ErrorCode::Full,
                "database or disk is full",
            ));
        }
        let next_id = cur_max + 1;
        cache.record_max_rowid_before();
        cache.max_rowid = Some(next_id);
        cache.deleted_max_rowid = false;
        Ok(next_id)
    }

    fn parse_cell_coords(&self, values: &[Value]) -> Result<[u32; RTREE_MAX_COORDS]> {
        let mut coords = [0u32; RTREE_MAX_COORDS];
        for dim in 0..self.n_dim {
            let idx_min = 1 + dim * 2;
            let idx_max = idx_min + 1;
            let v_min = values.get(idx_min).unwrap_or(&Value::Null);
            let v_max = values.get(idx_max).unwrap_or(&Value::Null);
            match self.mode {
                RTreeCoordMode::Float32 => {
                    let lo = rtree_value_down(value_to_f64(v_min));
                    let hi = rtree_value_up(value_to_f64(v_max));
                    if lo > hi {
                        let c1 = &self.col_names[idx_min];
                        let c2 = &self.col_names[idx_max];
                        return Err(SqliteError::new(
                            ErrorCode::Constraint,
                            format!("rtree constraint failed: {}.({c1}<={c2})", self.table_name),
                        ));
                    }
                    coords[dim * 2] = lo.to_bits();
                    coords[dim * 2 + 1] = hi.to_bits();
                }
                RTreeCoordMode::Int32 => {
                    let lo = value_to_i32_clamped(v_min);
                    let hi = value_to_i32_clamped(v_max);
                    if lo > hi {
                        let c1 = &self.col_names[idx_min];
                        let c2 = &self.col_names[idx_max];
                        return Err(SqliteError::new(
                            ErrorCode::Constraint,
                            format!("rtree constraint failed: {}.({c1}<={c2})", self.table_name),
                        ));
                    }
                    coords[dim * 2] = lo as u32;
                    coords[dim * 2 + 1] = hi as u32;
                }
            }
        }
        Ok(coords)
    }
}

impl VirtualTable for RTreeTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        // Strategy 1: Direct point lookup by rowid / id column (`i_column == -1` or `i_column == 0`)
        for (i, c) in info.constraints.iter().enumerate() {
            if !c.usable {
                continue;
            }
            if (c.i_column == -1 || c.i_column == 0) && c.op == VTabConstraintOp::Eq {
                info.constraint_usage[i].argv_index = 1;
                info.constraint_usage[i].omit = false;
                info.idx_num = 1;
                info.idx_flags = 1;
                info.estimated_cost = 1.0;
                info.estimated_rows = 1;
                return Ok(());
            }
        }

        // Strategy 2: Spatial tree walk with coordinate constraints
        let mut idx_str = String::new();
        let mut argv_idx = 0usize;
        for (i, c) in info.constraints.iter().enumerate() {
            if !c.usable || argv_idx >= 32 {
                continue;
            }
            if c.i_column >= 1 && (c.i_column as usize) <= self.n_dim2 {
                let op_ch = match c.op {
                    VTabConstraintOp::Eq => Some('A'),
                    VTabConstraintOp::Le => Some('B'),
                    VTabConstraintOp::Lt => Some('C'),
                    VTabConstraintOp::Ge => Some('D'),
                    VTabConstraintOp::Gt => Some('E'),
                    _ => None,
                };
                if let Some(ch) = op_ch {
                    argv_idx += 1;
                    let coord_idx = (c.i_column - 1) as u8;
                    idx_str.push(ch);
                    idx_str.push(char::from(b'0' + coord_idx));
                    info.constraint_usage[i].argv_index = argv_idx;
                    info.constraint_usage[i].omit = false;
                }
            }
        }

        info.idx_num = 2;
        if !idx_str.is_empty() {
            info.idx_str = Some(idx_str);
        }
        let n_est = RTREE_DEFAULT_ROWEST >> (argv_idx.min(30) as u32);
        let n_rows = n_est.max(RTREE_MIN_ROWEST);
        info.estimated_rows = n_rows;
        info.estimated_cost = 6.0 * (n_rows as f64);
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

    fn begin(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        self.ensure_coherent(&mut cache, ctx)?;
        cache.in_tx = true;
        cache.savepoints.clear();
        cache.undo_log.clear();
        Ok(())
    }

    fn sync(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        self.flush_dirty(&mut cache, ctx)
    }

    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        cache.in_tx = false;
        cache.savepoints.clear();
        cache.undo_log.clear();
        Ok(())
    }

    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        cache.clear_data();
        cache.in_tx = false;
        cache.savepoints.clear();
        Ok(())
    }

    fn savepoint(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(pos) = cache.savepoints.iter().position(|s| s.idx == idx) {
            cache.savepoints.truncate(pos);
        }
        let undo_pos = cache.undo_log.len();
        cache.savepoints.push(RTreeSavepoint {
            idx,
            undo_pos,
            flushed: false,
        });
        Ok(())
    }

    fn release(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(pos) = cache.savepoints.iter().position(|s| s.idx == idx) {
            cache.savepoints.truncate(pos);
        }
        if cache.savepoints.is_empty() && !cache.recording_undo {
            cache.undo_log.clear();
        }
        Ok(())
    }

    fn rollback_to(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        if let Some(pos) = cache.savepoints.iter().position(|s| s.idx == idx) {
            let sp = cache.savepoints[pos];
            cache.savepoints.truncate(pos + 1);
            if sp.flushed {
                cache.clear_data();
                if let Some(last) = cache.savepoints.last_mut() {
                    last.undo_pos = 0;
                    last.flushed = false;
                }
            } else {
                cache.rollback_undo_to(sp.undo_pos);
            }
        } else {
            cache.clear_data();
        }
        Ok(())
    }

    fn flush_before_shadow_access(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut cache = self.cache.lock().unwrap();
        self.flush_dirty(&mut cache, ctx)
    }

    fn invalidate_shadow_cache(&mut self) {
        let mut cache = self.cache.lock().unwrap();
        cache.clear_data();
    }

    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let mut cache = self.cache.lock().unwrap();
        self.ensure_coherent(&mut cache, ctx)?;
        let flush_after = !cache.in_tx;
        let undo_mark = cache.undo_log.len();
        let prev_rec = cache.recording_undo;
        cache.recording_undo = true;

        let res: Result<Option<i64>> = (|| match op {
            VTabUpdateOperation::Delete { old_rowid } => {
                self.delete_rowid_entry(&mut cache, ctx, value_to_i64(&old_rowid))?;
                Ok(None)
            }
            VTabUpdateOperation::Insert { new_rowid, values } => {
                let coords = self.parse_cell_coords(&values)?;
                let explicit_id = match new_rowid {
                    Some(v) if !v.is_null() => Some(value_to_i64(&v)),
                    _ => match values.first() {
                        Some(Value::Null) | None => None,
                        Some(v) => Some(value_to_i64(v)),
                    },
                };

                let final_id = if let Some(id) = explicit_id {
                    if self.read_rowid_nodeno(&mut cache, ctx, id)?.is_some() {
                        if ctx.vtab_on_conflict() == VTabConflictMode::Replace {
                            self.delete_rowid_entry(&mut cache, ctx, id)?;
                        } else {
                            return Err(SqliteError::new(
                                ErrorCode::Constraint,
                                format!(
                                    "UNIQUE constraint failed: {}.{}",
                                    self.table_name, self.col_names[0]
                                ),
                            ));
                        }
                    }
                    id
                } else {
                    self.allocate_new_rowid(&mut cache, ctx)?
                };

                let cell = RTreeCell {
                    id: final_id,
                    coords,
                };
                self.insert_cell_at_height(&mut cache, ctx, cell, 0)?;
                if self.n_aux > 0 {
                    let aux_slice = if values.len() > 1 + self.n_dim2 {
                        &values[1 + self.n_dim2..]
                    } else {
                        &[]
                    };
                    self.write_aux_values(&mut cache, final_id, aux_slice);
                }
                Ok(Some(final_id))
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                let old_id = value_to_i64(&old_rowid);
                let coords = self.parse_cell_coords(&values)?;
                let target_id = if !values.first().is_none_or(Value::is_null) {
                    value_to_i64(&values[0])
                } else if !new_rowid.is_null() {
                    value_to_i64(&new_rowid)
                } else {
                    old_id
                };

                if target_id != old_id
                    && self
                        .read_rowid_nodeno(&mut cache, ctx, target_id)?
                        .is_some()
                {
                    if ctx.vtab_on_conflict() == VTabConflictMode::Replace {
                        self.delete_rowid_entry(&mut cache, ctx, target_id)?;
                    } else {
                        return Err(SqliteError::new(
                            ErrorCode::Constraint,
                            format!(
                                "UNIQUE constraint failed: {}.{}",
                                self.table_name, self.col_names[0]
                            ),
                        ));
                    }
                }

                self.delete_rowid_entry(&mut cache, ctx, old_id)?;
                let cell = RTreeCell {
                    id: target_id,
                    coords,
                };
                self.insert_cell_at_height(&mut cache, ctx, cell, 0)?;
                if self.n_aux > 0 {
                    let aux_slice = if values.len() > 1 + self.n_dim2 {
                        &values[1 + self.n_dim2..]
                    } else {
                        &[]
                    };
                    self.write_aux_values(&mut cache, target_id, aux_slice);
                }
                Ok(Some(target_id))
            }
        })();

        cache.recording_undo = prev_rec;
        match res {
            Ok(val) => {
                if cache.savepoints.is_empty() && !cache.recording_undo {
                    cache.undo_log.clear();
                }
                if flush_after {
                    self.flush_dirty(&mut cache, ctx)?;
                }
                Ok(val)
            }
            Err(e) => {
                cache.rollback_undo_to(undo_mark);
                Err(e)
            }
        }
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        {
            let mut cache = self.cache.lock().unwrap();
            self.flush_dirty(&mut cache, ctx)?;
            cache.clear_data();
        }
        let q_db = quote_ident(&self.db_name);
        for suffix in ["node", "parent", "rowid"] {
            let old_q = quote_ident(&format!("{}_{suffix}", self.table_name));
            let new_q = quote_ident(&format!("{new_name}_{suffix}"));
            ctx.execute_sql(
                &format!("ALTER TABLE {q_db}.{old_q} RENAME TO {new_q}"),
                &[],
            )?;
        }
        self.table_name = new_name.to_owned();
        self.node_shadow = format!("{new_name}_node");
        self.rowid_shadow = format!("{new_name}_rowid");
        self.parent_shadow = format!("{new_name}_parent");
        Ok(())
    }

    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        {
            let mut cache = self.cache.lock().unwrap();
            cache.clear_data();
        }
        let q_db = quote_ident(&self.db_name);
        // `rtreeDestroy`: "DROP TABLE node; DROP TABLE rowid; DROP TABLE parent" in this order.
        for suffix in ["node", "rowid", "parent"] {
            let q_tbl = quote_ident(&format!("{}_{suffix}", self.table_name));
            ctx.execute_sql(&format!("DROP TABLE IF EXISTS {q_db}.{q_tbl}"), &[])?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpatialOp {
    AlwaysTrue,
    AlwaysFalse,
    Eq,
    Le,
    Lt,
    Ge,
    Gt,
}

#[derive(Debug, Clone, Copy)]
struct SpatialConstraint {
    coord_idx: usize,
    op: SpatialOp,
    val: f64,
}

impl SpatialConstraint {
    fn from_arg(op_ch: char, coord_idx: usize, arg: &Value) -> Self {
        let base_op = match op_ch {
            'A' => SpatialOp::Eq,
            'B' => SpatialOp::Le,
            'C' => SpatialOp::Lt,
            'D' => SpatialOp::Ge,
            'E' => SpatialOp::Gt,
            _ => SpatialOp::AlwaysTrue,
        };
        match classify_numeric_arg(arg) {
            NumericArg::Int(i) => {
                let mut op = base_op;
                if !(-((1_i64) << 48)..=((1_i64) << 48)).contains(&i) {
                    if op == SpatialOp::Lt {
                        op = SpatialOp::Le;
                    } else if op == SpatialOp::Gt {
                        op = SpatialOp::Ge;
                    }
                }
                Self {
                    coord_idx,
                    op,
                    val: i as f64,
                }
            }
            NumericArg::Float(f) => Self {
                coord_idx,
                op: base_op,
                val: f,
            },
            NumericArg::Null => Self {
                coord_idx,
                op: SpatialOp::AlwaysFalse,
                val: 0.0,
            },
            NumericArg::TextOrBlob => {
                // In SQLite comparison order, numbers (INT/REAL) are always < TEXT/BLOB
                let op = match base_op {
                    SpatialOp::Lt | SpatialOp::Le => SpatialOp::AlwaysTrue,
                    _ => SpatialOp::AlwaysFalse,
                };
                Self {
                    coord_idx,
                    op,
                    val: 0.0,
                }
            }
        }
    }

    #[inline]
    fn matches_nonleaf(&self, cell: &RTreeCell, mode: RTreeCoordMode) -> bool {
        let pair_base = self.coord_idx & !1;
        let lo = cell.coord_f64(pair_base, mode);
        let hi = cell.coord_f64(pair_base + 1, mode);
        match self.op {
            SpatialOp::AlwaysTrue => true,
            SpatialOp::AlwaysFalse => false,
            SpatialOp::Eq => self.val >= lo && self.val <= hi,
            SpatialOp::Le | SpatialOp::Lt => self.val >= lo,
            SpatialOp::Ge | SpatialOp::Gt => self.val <= hi,
        }
    }

    #[inline]
    fn matches_leaf(&self, cell: &RTreeCell, mode: RTreeCoordMode) -> bool {
        let x = cell.coord_f64(self.coord_idx, mode);
        match self.op {
            SpatialOp::AlwaysTrue => true,
            SpatialOp::AlwaysFalse => false,
            SpatialOp::Eq => x == self.val,
            SpatialOp::Le => x <= self.val,
            SpatialOp::Lt => x < self.val,
            SpatialOp::Ge => x >= self.val,
            SpatialOp::Gt => x > self.val,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BBox2D {
    q_min_x: f64,
    q_max_x: f64,
    q_min_y: f64,
    q_max_y: f64,
}

fn try_extract_2d_bbox(constraints: &[SpatialConstraint]) -> Option<BBox2D> {
    let mut q_min_x = f64::NEG_INFINITY;
    let mut q_max_x = f64::INFINITY;
    let mut q_min_y = f64::NEG_INFINITY;
    let mut q_max_y = f64::INFINITY;
    for c in constraints {
        if c.val.is_nan() {
            return None;
        }
        match (c.coord_idx, c.op) {
            (_, SpatialOp::AlwaysTrue) => {}
            (0, SpatialOp::Le) => {
                if c.val < q_max_x {
                    q_max_x = c.val;
                }
            }
            (1, SpatialOp::Ge) => {
                if c.val > q_min_x {
                    q_min_x = c.val;
                }
            }
            (2, SpatialOp::Le) => {
                if c.val < q_max_y {
                    q_max_y = c.val;
                }
            }
            (3, SpatialOp::Ge) => {
                if c.val > q_min_y {
                    q_min_y = c.val;
                }
            }
            _ => return None,
        }
    }
    Some(BBox2D {
        q_min_x,
        q_max_x,
        q_min_y,
        q_max_y,
    })
}

struct RTreeCursor {
    table: RTreeTable,
    cells: Vec<RTreeCell>,
    aux_rows: Vec<Vec<Value>>,
    pos: usize,
}

impl RTreeCursor {
    #[allow(clippy::too_many_arguments)]
    fn walk_subtree_2d_f32(
        table: &RTreeTable,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        node_id: i64,
        level: usize,
        q: BBox2D,
        visited: &mut Vec<i64>,
        out: &mut Vec<RTreeCell>,
    ) -> Result<()> {
        if visited.contains(&node_id) {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "cycle detected during RTree scan",
            ));
        }
        visited.push(node_id);
        let node = table.cached_node_mut(&mut cache.nodes, ctx, node_id)?;
        if level == 0 {
            for cell in &node.cells {
                let min_x = f64::from(f32::from_bits(cell.coords[0]));
                let max_x = f64::from(f32::from_bits(cell.coords[1]));
                let min_y = f64::from(f32::from_bits(cell.coords[2]));
                let max_y = f64::from(f32::from_bits(cell.coords[3]));
                // All four tests are evaluated (no short-circuit) so the loop has a single
                // data-dependent branch per cell.
                if (max_x >= q.q_min_x)
                    & (min_x <= q.q_max_x)
                    & (max_y >= q.q_min_y)
                    & (min_y <= q.q_max_y)
                {
                    out.push(*cell);
                }
            }
        } else {
            let mut stack_children = [0_i64; 64];
            let mut n_stack = 0usize;
            let mut heap_children: Option<Vec<i64>> = None;
            for cell in &node.cells {
                let min_x = f64::from(f32::from_bits(cell.coords[0]));
                let max_x = f64::from(f32::from_bits(cell.coords[1]));
                let min_y = f64::from(f32::from_bits(cell.coords[2]));
                let max_y = f64::from(f32::from_bits(cell.coords[3]));
                // All four tests are evaluated (no short-circuit) so the loop has a single
                // data-dependent branch per cell.
                if (max_x >= q.q_min_x)
                    & (min_x <= q.q_max_x)
                    & (max_y >= q.q_min_y)
                    & (min_y <= q.q_max_y)
                {
                    if let Some(ref mut v) = heap_children {
                        v.push(cell.id);
                    } else if n_stack < 64 {
                        stack_children[n_stack] = cell.id;
                        n_stack += 1;
                    } else {
                        let mut v = Vec::with_capacity(node.cells.len());
                        v.extend_from_slice(&stack_children[..n_stack]);
                        v.push(cell.id);
                        heap_children = Some(v);
                    }
                }
            }
            if let Some(children) = heap_children {
                for cid in children {
                    Self::walk_subtree_2d_f32(table, cache, ctx, cid, level - 1, q, visited, out)?;
                }
            } else {
                for &cid in &stack_children[..n_stack] {
                    Self::walk_subtree_2d_f32(table, cache, ctx, cid, level - 1, q, visited, out)?;
                }
            }
        }
        visited.pop();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_subtree_2d_i32(
        table: &RTreeTable,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        node_id: i64,
        level: usize,
        q: BBox2D,
        visited: &mut Vec<i64>,
        out: &mut Vec<RTreeCell>,
    ) -> Result<()> {
        if visited.contains(&node_id) {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "cycle detected during RTree scan",
            ));
        }
        visited.push(node_id);
        let node = table.cached_node_mut(&mut cache.nodes, ctx, node_id)?;
        if level == 0 {
            for cell in &node.cells {
                let min_x = f64::from(cell.coords[0] as i32);
                let max_x = f64::from(cell.coords[1] as i32);
                let min_y = f64::from(cell.coords[2] as i32);
                let max_y = f64::from(cell.coords[3] as i32);
                // All four tests are evaluated (no short-circuit) so the loop has a single
                // data-dependent branch per cell.
                if (max_x >= q.q_min_x)
                    & (min_x <= q.q_max_x)
                    & (max_y >= q.q_min_y)
                    & (min_y <= q.q_max_y)
                {
                    out.push(*cell);
                }
            }
        } else {
            let mut stack_children = [0_i64; 64];
            let mut n_stack = 0usize;
            let mut heap_children: Option<Vec<i64>> = None;
            for cell in &node.cells {
                let min_x = f64::from(cell.coords[0] as i32);
                let max_x = f64::from(cell.coords[1] as i32);
                let min_y = f64::from(cell.coords[2] as i32);
                let max_y = f64::from(cell.coords[3] as i32);
                // All four tests are evaluated (no short-circuit) so the loop has a single
                // data-dependent branch per cell.
                if (max_x >= q.q_min_x)
                    & (min_x <= q.q_max_x)
                    & (max_y >= q.q_min_y)
                    & (min_y <= q.q_max_y)
                {
                    if let Some(ref mut v) = heap_children {
                        v.push(cell.id);
                    } else if n_stack < 64 {
                        stack_children[n_stack] = cell.id;
                        n_stack += 1;
                    } else {
                        let mut v = Vec::with_capacity(node.cells.len());
                        v.extend_from_slice(&stack_children[..n_stack]);
                        v.push(cell.id);
                        heap_children = Some(v);
                    }
                }
            }
            if let Some(children) = heap_children {
                for cid in children {
                    Self::walk_subtree_2d_i32(table, cache, ctx, cid, level - 1, q, visited, out)?;
                }
            } else {
                for &cid in &stack_children[..n_stack] {
                    Self::walk_subtree_2d_i32(table, cache, ctx, cid, level - 1, q, visited, out)?;
                }
            }
        }
        visited.pop();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_subtree_general(
        table: &RTreeTable,
        cache: &mut RTreeCache,
        ctx: &mut dyn VTabHostContext,
        node_id: i64,
        level: usize,
        constraints: &[SpatialConstraint],
        visited: &mut Vec<i64>,
        out: &mut Vec<RTreeCell>,
    ) -> Result<()> {
        if visited.contains(&node_id) {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "cycle detected during RTree scan",
            ));
        }
        visited.push(node_id);
        let node = table.cached_node_mut(&mut cache.nodes, ctx, node_id)?;
        let mode = table.mode;
        if level == 0 {
            for cell in &node.cells {
                if constraints.iter().all(|c| c.matches_leaf(cell, mode)) {
                    out.push(*cell);
                }
            }
        } else {
            let mut stack_children = [0_i64; 64];
            let mut n_stack = 0usize;
            let mut heap_children: Option<Vec<i64>> = None;
            for cell in &node.cells {
                if constraints.iter().all(|c| c.matches_nonleaf(cell, mode)) {
                    if let Some(ref mut v) = heap_children {
                        v.push(cell.id);
                    } else if n_stack < 64 {
                        stack_children[n_stack] = cell.id;
                        n_stack += 1;
                    } else {
                        let mut v = Vec::with_capacity(node.cells.len());
                        v.extend_from_slice(&stack_children[..n_stack]);
                        v.push(cell.id);
                        heap_children = Some(v);
                    }
                }
            }
            if let Some(children) = heap_children {
                for cid in children {
                    Self::walk_subtree_general(
                        table,
                        cache,
                        ctx,
                        cid,
                        level - 1,
                        constraints,
                        visited,
                        out,
                    )?;
                }
            } else {
                for &cid in &stack_children[..n_stack] {
                    Self::walk_subtree_general(
                        table,
                        cache,
                        ctx,
                        cid,
                        level - 1,
                        constraints,
                        visited,
                        out,
                    )?;
                }
            }
        }
        visited.pop();
        Ok(())
    }
}

impl VirtualCursor for RTreeCursor {
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

        let mut cache = self.table.cache.lock().unwrap();
        self.table.ensure_coherent(&mut cache, ctx)?;

        if idx_num == 1 {
            let Some(arg) = argv.first() else {
                return Ok(());
            };
            let lookup_id = match classify_numeric_arg(arg) {
                NumericArg::Int(i) => Some(i),
                NumericArg::Float(f) => {
                    let i = f as i64;
                    if (i as f64) == f { Some(i) } else { None }
                }
                _ => None,
            };
            let Some(rowid) = lookup_id else {
                return Ok(());
            };
            let Some(nodeno) = self.table.read_rowid_nodeno(&mut cache, ctx, rowid)? else {
                return Ok(());
            };
            self.table.load_node_into_cache(&mut cache, ctx, nodeno)?;
            let found_cell = cache
                .nodes
                .get(&nodeno)
                .and_then(|s| s.as_ref())
                .and_then(|n| n.cells.iter().find(|c| c.id == rowid).copied());
            let Some(cell) = found_cell else {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    "missing cell in leaf node referenced by _rowid",
                ));
            };
            self.cells.push(cell);
            if self.table.n_aux > 0 {
                let aux = self.table.read_rowid_aux(&mut cache, ctx, cell.id)?;
                self.aux_rows.push(aux);
            }
            return Ok(());
        }

        let mut constraints = [SpatialConstraint {
            coord_idx: 0,
            op: SpatialOp::AlwaysTrue,
            val: 0.0,
        }; 32];
        let mut n_constraints = 0usize;
        if let Some(s) = idx_str {
            let bytes = s.as_bytes();
            for (arg_idx, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
                if n_constraints >= 32 {
                    break;
                }
                if let Some(arg) = argv.get(arg_idx) {
                    let op_ch = char::from(pair[0]);
                    let coord_idx = pair[1].saturating_sub(b'0') as usize;
                    if coord_idx < self.table.n_dim2 {
                        let sc = SpatialConstraint::from_arg(op_ch, coord_idx, arg);
                        if sc.op == SpatialOp::AlwaysFalse {
                            return Ok(());
                        }
                        constraints[n_constraints] = sc;
                        n_constraints += 1;
                    }
                }
            }
        }
        let active_constraints = &constraints[..n_constraints];

        let root_depth = cache
            .nodes
            .get(&1)
            .and_then(|s| s.as_ref())
            .map(|n| n.depth as usize)
            .ok_or_else(|| {
                SqliteError::new(ErrorCode::Corrupt, "missing or invalid RTree node 1")
            })?;
        let mut visited = Vec::with_capacity(root_depth + 2);
        if self.cells.capacity() == 0 {
            // A fresh cursor: skip the first few growth steps of the result buffer.
            self.cells.reserve(64);
        }

        if self.table.n_dim == 2
            && let Some(bbox2d) = try_extract_2d_bbox(active_constraints)
        {
            match self.table.mode {
                RTreeCoordMode::Float32 => Self::walk_subtree_2d_f32(
                    &self.table,
                    &mut cache,
                    ctx,
                    1,
                    root_depth,
                    bbox2d,
                    &mut visited,
                    &mut self.cells,
                )?,
                RTreeCoordMode::Int32 => Self::walk_subtree_2d_i32(
                    &self.table,
                    &mut cache,
                    ctx,
                    1,
                    root_depth,
                    bbox2d,
                    &mut visited,
                    &mut self.cells,
                )?,
            }
        } else {
            Self::walk_subtree_general(
                &self.table,
                &mut cache,
                ctx,
                1,
                root_depth,
                active_constraints,
                &mut visited,
                &mut self.cells,
            )?;
        }

        if self.table.n_aux > 0 {
            self.aux_rows.reserve(self.cells.len());
            for cell in &self.cells {
                let aux = self.table.read_rowid_aux(&mut cache, ctx, cell.id)?;
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

    fn column(&self, col_idx: usize) -> Result<Value> {
        let Some(cell) = self.cells.get(self.pos) else {
            return Ok(Value::Null);
        };
        if col_idx == 0 {
            return Ok(Value::Integer(cell.id));
        }
        let coord_idx = col_idx - 1;
        if coord_idx < self.table.n_dim2 {
            return Ok(cell.coord_sql_value(coord_idx, self.table.mode));
        }
        let aux_idx = coord_idx - self.table.n_dim2;
        Ok(self
            .aux_rows
            .get(self.pos)
            .and_then(|r| r.get(aux_idx))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.cells.get(self.pos).map_or(0, |c| c.id))
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
pub fn eval_rtreenode(dim_arg: &Value, blob_arg: &Value) -> Result<Value> {
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
            format_c_printf_g(&mut out, f64::from(f));
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

    if !(1..=RTREE_MAX_DIMENSIONS).contains(&n_dim) {
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

/// Writes into `idxs[..cells.len()]` the cell indexes ordered by `(coord d0, coord d0 + 1)`
/// under [`f64::total_cmp`], equal keys in index order: the permutation a stable sort of the
/// indexes with that comparator produces. `keyed` is scratch space; `cells.len()` must not
/// exceed either buffer.
fn sort_cell_indexes_by_dim(
    cells: &[RTreeCell],
    d0: usize,
    mode: RTreeCoordMode,
    keyed: &mut [(i64, i64, usize)],
    idxs: &mut [usize],
) {
    let keyed = &mut keyed[..cells.len()];
    for (i, (slot, cell)) in keyed.iter_mut().zip(cells).enumerate() {
        *slot = (
            f64_total_order_key(cell.coord_f64(d0, mode)),
            f64_total_order_key(cell.coord_f64(d0 + 1, mode)),
            i,
        );
    }
    // The index makes every key distinct, so an unstable sort gives the stable order.
    keyed.sort_unstable();
    for (slot, k) in idxs.iter_mut().zip(keyed.iter()) {
        *slot = k.2;
    }
}

/// Maps an `f64` to an integer whose natural order is exactly [`f64::total_cmp`].
#[inline]
fn f64_total_order_key(v: f64) -> i64 {
    let bits = v.to_bits() as i64;
    bits ^ ((((bits >> 63) as u64) >> 1) as i64)
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

fn value_to_i32_clamped(v: &Value) -> i32 {
    let i = value_to_i64(v);
    i.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// A pseudo-random 64-bit value for rowid probing (`sqlite3_randomness` in C; the values are
/// not meant to be reproducible).
fn rtree_random_i64() -> i64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STATE: AtomicU64 = AtomicU64::new(0);
    let mut x = STATE.load(Ordering::Relaxed);
    if x == 0 {
        x = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
            | 1;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    STATE.store(x, Ordering::Relaxed);
    x.wrapping_mul(0x2545_f491_4f6c_dd1d) as i64
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

fn classify_numeric_arg(v: &Value) -> NumericArg {
    match v {
        Value::Null => NumericArg::Null,
        Value::Integer(i) => NumericArg::Int(*i),
        Value::Real(f) => NumericArg::Float(*f),
        Value::Text(s) => {
            let t = s.trim();
            if t.is_empty() {
                return NumericArg::TextOrBlob;
            }
            if let Ok(i) = t.parse::<i64>() {
                return NumericArg::Int(i);
            }
            if let Ok(f) = t.parse::<f64>()
                && f.is_finite()
            {
                return NumericArg::Float(f);
            }
            NumericArg::TextOrBlob
        }
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

fn format_c_printf_g(out: &mut String, val: f64) {
    if val == 0.0 {
        if val.is_sign_negative() {
            out.push_str("-0");
        } else {
            out.push('0');
        }
        return;
    }
    if !val.is_finite() {
        if val.is_nan() {
            out.push_str("nan");
        } else if val.is_sign_negative() {
            out.push_str("-inf");
        } else {
            out.push_str("inf");
        }
        return;
    }
    let sci = format!("{val:.5e}");
    let Some((mant, exp_str)) = sci.split_once('e') else {
        let _ = write!(out, "{val}");
        return;
    };
    let exp = exp_str.parse::<i32>().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let decimals = (5 - exp).max(0) as usize;
        let fixed = format!("{val:.decimals$}");
        let trimmed = if fixed.contains('.') {
            fixed.trim_end_matches('0').trim_end_matches('.')
        } else {
            fixed.as_str()
        };
        out.push_str(trimmed);
    } else {
        let mant_trimmed = mant.trim_end_matches('0').trim_end_matches('.');
        let sign = if exp < 0 { '-' } else { '+' };
        let abs_exp = exp.unsigned_abs();
        let _ = write!(out, "{mant_trimmed}e{sign}{abs_exp:02}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn total_order_key_orders_like_total_cmp() {
        let mut vals: Vec<f64> = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            1.5,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            -f64::NAN,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::MAX,
            f64::MIN,
            f64::from(f32::MAX),
            f64::from(f32::MIN_POSITIVE),
            f64::from_bits(0x7ff0_0000_0000_0001),
            f64::from_bits(0xfff0_0000_0000_0001),
            f64::from_bits(0x7ff8_0000_0000_0123),
            f64::from_bits(0xfff8_0000_0000_0123),
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..400 {
            vals.push(f64::from_bits(next(&mut seed)));
            vals.push(f64::from(f32::from_bits(next(&mut seed) as u32)));
            vals.push(f64::from(next(&mut seed) as i32));
        }
        for a in &vals {
            for b in &vals {
                assert_eq!(
                    f64_total_order_key(*a).cmp(&f64_total_order_key(*b)),
                    a.total_cmp(b),
                    "{a:?} ({:#x}) vs {b:?} ({:#x})",
                    a.to_bits(),
                    b.to_bits()
                );
            }
        }
    }

    /// The R*-split's keyed sort must give the permutation of the stable comparator sort it
    /// replaced, for both coordinate modes, with duplicate keys, signed zeros, infinities and NaNs.
    #[test]
    fn split_sort_matches_stable_comparator_sort() {
        let specials_f32: [u32; 10] = [
            0.0_f32.to_bits(),
            (-0.0_f32).to_bits(),
            1.0_f32.to_bits(),
            (-1.0_f32).to_bits(),
            f32::INFINITY.to_bits(),
            f32::NEG_INFINITY.to_bits(),
            f32::NAN.to_bits(),
            (-f32::NAN).to_bits(),
            0x7f80_0001,
            f32::MAX.to_bits(),
        ];
        let mut seed = 0x0123_4567_89AB_CDEF_u64;
        for round in 0..3000 {
            let mode = if round % 2 == 0 {
                RTreeCoordMode::Float32
            } else {
                RTreeCoordMode::Int32
            };
            let n_cell = 1 + (next(&mut seed) % 64) as usize;
            let n_dim = 1 + (next(&mut seed) % RTREE_MAX_DIMENSIONS as u64) as usize;
            let distinct = 1 + next(&mut seed) % 40;
            let cells: Vec<RTreeCell> = (0..n_cell)
                .map(|i| {
                    let mut cell = RTreeCell::zero();
                    cell.id = i as i64;
                    for slot in cell.coords.iter_mut().take(n_dim * 2) {
                        let r = next(&mut seed);
                        *slot = match (mode, r % 8) {
                            (RTreeCoordMode::Float32, 0) => specials_f32[((r >> 8) % 10) as usize],
                            (RTreeCoordMode::Float32, 1) => (r >> 8) as u32,
                            (RTreeCoordMode::Float32, _) => {
                                (((r >> 8) % distinct) as f32 - 7.5).to_bits()
                            }
                            (RTreeCoordMode::Int32, 0) => (r >> 8) as u32,
                            (RTreeCoordMode::Int32, _) => (((r >> 8) % distinct) as i32 - 7) as u32,
                        };
                    }
                    cell
                })
                .collect();
            for dim in 0..n_dim {
                let d0 = dim * 2;
                let d1 = d0 + 1;
                let mut want: Vec<usize> = (0..n_cell).collect();
                want.sort_by(|&a, &b| {
                    let a0 = cells[a].coord_f64(d0, mode);
                    let a1 = cells[a].coord_f64(d1, mode);
                    let b0 = cells[b].coord_f64(d0, mode);
                    let b1 = cells[b].coord_f64(d1, mode);
                    a0.total_cmp(&b0).then_with(|| a1.total_cmp(&b1))
                });
                let mut keyed = [(0_i64, 0_i64, 0_usize); 64];
                let mut got = [0usize; 64];
                sort_cell_indexes_by_dim(&cells, d0, mode, &mut keyed, &mut got);
                assert_eq!(
                    &got[..n_cell],
                    &want[..],
                    "round {round} dim {dim} {mode:?}"
                );
            }
        }
    }

    #[test]
    fn node_number_hasher_separates_small_keys() {
        use std::hash::BuildHasher;
        let build = BuildHasherDefault::<NodeNoHasher>::default();
        let mut low_bits = std::collections::HashSet::new();
        let mut top_bits = std::collections::HashSet::new();
        for nodeno in 1_i64..=4096 {
            let h = build.hash_one(nodeno);
            low_bits.insert(h & 0xfff);
            top_bits.insert(h >> 57);
        }
        // Bucket index (low bits) and control tag (top 7 bits) both vary across node numbers.
        assert!(low_bits.len() > 2400, "{}", low_bits.len());
        assert_eq!(top_bits.len(), 128);
    }
}
