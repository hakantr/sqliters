//! R-Tree and R*-Tree spatial index virtual table modules (`rtree` and `rtree_i32`)
//! and companion SQL functions (`rtreedepth`, `rtreenode`, `rtreecheck`).
//!
//! Ported from SQLite 3.54.1 `sqlite/ext/rtree/rtree.c` with identical on-disk
//! shadow table layout (`<name>_node`, `<name>_rowid`, `<name>_parent`) and
//! big-endian binary cell representation.

use std::collections::HashMap;
use std::fmt::Write as _;

use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConflictMode, VTabConstraintOp,
    VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor,
    VirtualTable, VirtualTableModule,
};

const RTREE_MAX_DIMENSIONS: usize = 5;
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
enum CoordVal {
    F32(f32),
    I32(i32),
}

impl CoordVal {
    #[inline]
    fn to_be_bytes(self) -> [u8; 4] {
        match self {
            Self::F32(f) => f.to_bits().to_be_bytes(),
            Self::I32(i) => i.to_be_bytes(),
        }
    }

    #[inline]
    fn from_be_bytes(bytes: [u8; 4], mode: RTreeCoordMode) -> Self {
        match mode {
            RTreeCoordMode::Float32 => Self::F32(f32::from_bits(u32::from_be_bytes(bytes))),
            RTreeCoordMode::Int32 => Self::I32(i32::from_be_bytes(bytes)),
        }
    }

    #[inline]
    fn as_f64(self) -> f64 {
        match self {
            Self::F32(f) => f64::from(f),
            Self::I32(i) => f64::from(i),
        }
    }

    #[inline]
    fn to_sql_value(self) -> Value {
        match self {
            Self::F32(f) => Value::Real(f64::from(f)),
            Self::I32(i) => Value::Integer(i64::from(i)),
        }
    }

    #[inline]
    fn min_with(self, other: Self) -> Self {
        match (self, other) {
            (Self::F32(a), Self::F32(b)) => Self::F32(if a < b { a } else { b }),
            (Self::I32(a), Self::I32(b)) => Self::I32(a.min(b)),
            _ => self,
        }
    }

    #[inline]
    fn max_with(self, other: Self) -> Self {
        match (self, other) {
            (Self::F32(a), Self::F32(b)) => Self::F32(if a > b { a } else { b }),
            (Self::I32(a), Self::I32(b)) => Self::I32(a.max(b)),
            _ => self,
        }
    }

    #[inline]
    fn gt(self, other: Self) -> bool {
        match (self, other) {
            (Self::F32(a), Self::F32(b)) => a > b,
            (Self::I32(a), Self::I32(b)) => a > b,
            _ => false,
        }
    }

    #[inline]
    fn lt(self, other: Self) -> bool {
        match (self, other) {
            (Self::F32(a), Self::F32(b)) => a < b,
            (Self::I32(a), Self::I32(b)) => a < b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
struct RTreeCell {
    id: i64,
    coords: Vec<CoordVal>,
}

impl RTreeCell {
    fn area(&self, mode: RTreeCoordMode) -> f64 {
        let mut area = 1.0_f64;
        for pair in self.coords.as_chunks::<2>().0 {
            let span = match mode {
                RTreeCoordMode::Float32 => pair[1].as_f64() - pair[0].as_f64(),
                RTreeCoordMode::Int32 => pair[1].as_f64() - pair[0].as_f64(),
            };
            area *= span;
        }
        area
    }

    fn margin(&self) -> f64 {
        let mut margin = 0.0_f64;
        for pair in self.coords.as_chunks::<2>().0 {
            margin += pair[1].as_f64() - pair[0].as_f64();
        }
        margin
    }

    fn union_in_place(&mut self, other: &Self) {
        for (dst_pair, src_pair) in self
            .coords
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(other.coords.as_chunks::<2>().0.iter())
        {
            dst_pair[0] = dst_pair[0].min_with(src_pair[0]);
            dst_pair[1] = dst_pair[1].max_with(src_pair[1]);
        }
    }

    fn contains(&self, other: &Self) -> bool {
        for (a, b) in self
            .coords
            .as_chunks::<2>()
            .0
            .iter()
            .zip(other.coords.as_chunks::<2>().0.iter())
        {
            if b[0].lt(a[0]) || b[1].gt(a[1]) {
                return false;
            }
        }
        true
    }

    fn overlap(&self, other: &Self) -> f64 {
        let mut prod = 1.0_f64;
        for (a, b) in self
            .coords
            .as_chunks::<2>()
            .0
            .iter()
            .zip(other.coords.as_chunks::<2>().0.iter())
        {
            let lo = a[0].as_f64().max(b[0].as_f64());
            let hi = a[1].as_f64().min(b[1].as_f64());
            if hi < lo {
                return 0.0;
            }
            prod *= hi - lo;
        }
        prod
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
    mode: RTreeCoordMode,
    n_dim: usize,
    n_dim2: usize,
    n_aux: usize,
    bytes_per_cell: usize,
    node_size: usize,
    col_names: Vec<String>,
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
            let q_node = quote_ident(&format!("{table_name}_node"));
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
                    format!("undersize RTree blobs in \"{table_name}_node\""),
                ));
            }
            sz as usize
        };

        if is_create {
            let q_rowid = quote_ident(&format!("{table_name}_rowid"));
            let q_node = quote_ident(&format!("{table_name}_node"));
            let q_parent = quote_ident(&format!("{table_name}_parent"));

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
            ctx.execute_sql(
                &format!("INSERT INTO {q_db}.{q_node} VALUES(1, ?1)"),
                &[Value::Blob(vec![0u8; node_size])],
            )?;
        }

        Ok(Self {
            db_name,
            table_name,
            mode,
            n_dim,
            n_dim2,
            n_aux,
            bytes_per_cell,
            node_size,
            col_names,
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

    fn q_shadow(&self, suffix: &str) -> String {
        format!(
            "{}.{}",
            quote_ident(&self.db_name),
            quote_ident(&format!("{}_{}", self.table_name, suffix))
        )
    }

    fn read_node_blob(&self, ctx: &mut dyn VTabHostContext, nodeno: i64) -> Result<Vec<u8>> {
        if nodeno <= 0 {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "corrupt rtree node id",
            ));
        }
        let tbl = self.q_shadow("node");
        let rows = ctx.query_sql(
            &format!("SELECT data FROM {tbl} WHERE nodeno = ?1"),
            &[Value::Integer(nodeno)],
        )?;
        let Some(Value::Blob(data)) = rows.first().and_then(|r| r.first()) else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("missing or invalid RTree node {nodeno}"),
            ));
        };
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
        if nodeno == 1 {
            let depth = read_u16_be(&data[0..2]) as usize;
            if depth > RTREE_MAX_DEPTH {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    "corrupt RTree root depth",
                ));
            }
        }
        Ok(data.clone())
    }

    fn write_node_blob(
        &self,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
        data: &[u8],
    ) -> Result<()> {
        let tbl = self.q_shadow("node");
        ctx.execute_sql(
            &format!("INSERT OR REPLACE INTO {tbl} VALUES(?1, ?2)"),
            &[Value::Integer(nodeno), Value::Blob(data.to_vec())],
        )?;
        Ok(())
    }

    fn alloc_new_node(&self, ctx: &mut dyn VTabHostContext, data: &[u8]) -> Result<i64> {
        let tbl = self.q_shadow("node");
        let rows = ctx.query_sql(
            &format!("SELECT coalesce(max(nodeno), 1) + 1 FROM {tbl}"),
            &[],
        )?;
        let next_id = rows
            .first()
            .and_then(|r| r.first())
            .and_then(Value::as_i64)
            .unwrap_or(2)
            .max(2);
        self.write_node_blob(ctx, next_id, data)?;
        Ok(next_id)
    }

    fn delete_node_blob(&self, ctx: &mut dyn VTabHostContext, nodeno: i64) -> Result<()> {
        let tbl = self.q_shadow("node");
        ctx.execute_sql(
            &format!("DELETE FROM {tbl} WHERE nodeno = ?1"),
            &[Value::Integer(nodeno)],
        )?;
        Ok(())
    }

    fn read_rowid_nodeno(&self, ctx: &mut dyn VTabHostContext, rowid: i64) -> Result<Option<i64>> {
        let tbl = self.q_shadow("rowid");
        let rows = ctx.query_sql(
            &format!("SELECT nodeno FROM {tbl} WHERE rowid = ?1"),
            &[Value::Integer(rowid)],
        )?;
        Ok(rows.first().and_then(|r| r.first()).and_then(Value::as_i64))
    }

    fn write_rowid_mapping(
        &self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        nodeno: i64,
    ) -> Result<()> {
        let tbl = self.q_shadow("rowid");
        if self.n_aux == 0 {
            ctx.execute_sql(
                &format!("INSERT OR REPLACE INTO {tbl} VALUES(?1, ?2)"),
                &[Value::Integer(rowid), Value::Integer(nodeno)],
            )?;
        } else {
            ctx.execute_sql(
                &format!(
                    "INSERT INTO {tbl}(rowid, nodeno) VALUES(?1, ?2) \
                     ON CONFLICT(rowid) DO UPDATE SET nodeno = excluded.nodeno"
                ),
                &[Value::Integer(rowid), Value::Integer(nodeno)],
            )?;
        }
        Ok(())
    }

    fn write_aux_values(
        &self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        aux_vals: &[Value],
    ) -> Result<()> {
        if self.n_aux == 0 {
            return Ok(());
        }
        let tbl = self.q_shadow("rowid");
        let mut sql = format!("UPDATE {tbl} SET ");
        let mut params = Vec::with_capacity(self.n_aux + 1);
        params.push(Value::Integer(rowid));
        for i in 0..self.n_aux {
            if i > 0 {
                sql.push_str(", ");
            }
            let _ = write!(sql, "a{i} = ?{}", i + 2);
            params.push(aux_vals.get(i).cloned().unwrap_or(Value::Null));
        }
        sql.push_str(" WHERE rowid = ?1");
        ctx.execute_sql(&sql, &params)?;
        Ok(())
    }

    fn delete_rowid_mapping(&self, ctx: &mut dyn VTabHostContext, rowid: i64) -> Result<()> {
        let tbl = self.q_shadow("rowid");
        ctx.execute_sql(
            &format!("DELETE FROM {tbl} WHERE rowid = ?1"),
            &[Value::Integer(rowid)],
        )?;
        Ok(())
    }

    fn read_parent_nodeno(
        &self,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
    ) -> Result<Option<i64>> {
        let tbl = self.q_shadow("parent");
        let rows = ctx.query_sql(
            &format!("SELECT parentnode FROM {tbl} WHERE nodeno = ?1"),
            &[Value::Integer(nodeno)],
        )?;
        Ok(rows.first().and_then(|r| r.first()).and_then(Value::as_i64))
    }

    fn write_parent_mapping(
        &self,
        ctx: &mut dyn VTabHostContext,
        nodeno: i64,
        parent: i64,
    ) -> Result<()> {
        let tbl = self.q_shadow("parent");
        ctx.execute_sql(
            &format!("INSERT OR REPLACE INTO {tbl} VALUES(?1, ?2)"),
            &[Value::Integer(nodeno), Value::Integer(parent)],
        )?;
        Ok(())
    }

    fn delete_parent_mapping(&self, ctx: &mut dyn VTabHostContext, nodeno: i64) -> Result<()> {
        let tbl = self.q_shadow("parent");
        ctx.execute_sql(
            &format!("DELETE FROM {tbl} WHERE nodeno = ?1"),
            &[Value::Integer(nodeno)],
        )?;
        Ok(())
    }

    fn update_child_mapping(
        &self,
        ctx: &mut dyn VTabHostContext,
        child_id: i64,
        node_id: i64,
        height: usize,
    ) -> Result<()> {
        if height == 0 {
            self.write_rowid_mapping(ctx, child_id, node_id)
        } else {
            self.write_parent_mapping(ctx, child_id, node_id)
        }
    }

    fn decode_node_cells(&self, data: &[u8]) -> Vec<RTreeCell> {
        let n_cell = read_u16_be(&data[2..4]) as usize;
        let mut cells = Vec::with_capacity(n_cell);
        for i in 0..n_cell {
            let base = 4 + i * self.bytes_per_cell;
            let id = read_i64_be(&data[base..base + 8]);
            let mut coords = Vec::with_capacity(self.n_dim2);
            for j in 0..self.n_dim2 {
                let c_off = base + 8 + j * 4;
                let bytes = [
                    data[c_off],
                    data[c_off + 1],
                    data[c_off + 2],
                    data[c_off + 3],
                ];
                coords.push(CoordVal::from_be_bytes(bytes, self.mode));
            }
            cells.push(RTreeCell { id, coords });
        }
        cells
    }

    fn encode_node_cells(&self, depth: u16, cells: &[RTreeCell]) -> Vec<u8> {
        let mut buf = vec![0u8; self.node_size];
        buf[0..2].copy_from_slice(&depth.to_be_bytes());
        buf[2..4].copy_from_slice(&(cells.len() as u16).to_be_bytes());
        for (i, cell) in cells.iter().enumerate() {
            let base = 4 + i * self.bytes_per_cell;
            buf[base..base + 8].copy_from_slice(&cell.id.to_be_bytes());
            for (j, c) in cell.coords.iter().enumerate() {
                let c_off = base + 8 + j * 4;
                buf[c_off..c_off + 4].copy_from_slice(&c.to_be_bytes());
            }
        }
        buf
    }

    fn compute_bbox(&self, cells: &[RTreeCell], node_id: i64) -> RTreeCell {
        let mut bbox = cells[0].clone();
        for cell in &cells[1..] {
            bbox.union_in_place(cell);
        }
        bbox.id = node_id;
        bbox
    }

    fn choose_subtree_path(
        &self,
        ctx: &mut dyn VTabHostContext,
        cell: &RTreeCell,
        target_height: usize,
    ) -> Result<(usize, Vec<i64>)> {
        let root_blob = self.read_node_blob(ctx, 1)?;
        let root_depth = read_u16_be(&root_blob[0..2]) as usize;
        if target_height > root_depth {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "corrupt RTree subtree height",
            ));
        }
        let steps = root_depth - target_height;
        let mut path = Vec::with_capacity(steps + 1);
        path.push(1);
        let mut cur_blob = root_blob;
        for step in 0..steps {
            let cells = self.decode_node_cells(&cur_blob);
            if cells.is_empty() {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    "corrupt empty internal RTree node",
                ));
            }
            let mut best_id = cells[0].id;
            let mut found_containing = false;
            let mut min_area = 0.0_f64;
            for c in &cells {
                if c.contains(cell) {
                    let area = c.area(self.mode);
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
                    let area = c.area(self.mode);
                    let mut u = c.clone();
                    u.union_in_place(cell);
                    let growth = u.area(self.mode) - area;
                    if idx == 0 || growth < min_growth || (growth == min_growth && area < min_area)
                    {
                        min_growth = growth;
                        min_area = area;
                        best_id = c.id;
                    }
                }
            }
            if path.contains(&best_id) {
                return Err(SqliteError::new(ErrorCode::Corrupt, "cycle in RTree"));
            }
            path.push(best_id);
            if step + 1 < steps {
                cur_blob = self.read_node_blob(ctx, best_id)?;
            }
        }
        Ok((root_depth, path))
    }

    fn split_cells_rstar(&self, all_cells: &[RTreeCell]) -> (Vec<RTreeCell>, Vec<RTreeCell>) {
        let n_cell = all_cells.len();
        let min_c = self.min_cells().min(n_cell / 2).max(1);
        let max_left = (n_cell - min_c).max(min_c);

        let mut best_dim = 0usize;
        let mut best_split = min_c;
        let mut best_margin = 0.0_f64;
        let mut sorted_per_dim: Vec<Vec<usize>> = Vec::with_capacity(self.n_dim);

        for dim in 0..self.n_dim {
            let mut idxs: Vec<usize> = (0..n_cell).collect();
            idxs.sort_by(|&a, &b| {
                let a0 = all_cells[a].coords[dim * 2].as_f64();
                let a1 = all_cells[a].coords[dim * 2 + 1].as_f64();
                let b0 = all_cells[b].coords[dim * 2].as_f64();
                let b1 = all_cells[b].coords[dim * 2 + 1].as_f64();
                a0.total_cmp(&b0).then_with(|| a1.total_cmp(&b1))
            });

            let mut margin_sum = 0.0_f64;
            let mut dim_best_left = min_c;
            let mut dim_best_overlap = 0.0_f64;
            let mut dim_best_area = 0.0_f64;

            for n_left in min_c..=max_left {
                let mut left_box = all_cells[idxs[0]].clone();
                for &k in &idxs[1..n_left] {
                    left_box.union_in_place(&all_cells[k]);
                }
                let mut right_box = all_cells[idxs[n_left]].clone();
                for &k in &idxs[n_left + 1..] {
                    right_box.union_in_place(&all_cells[k]);
                }

                margin_sum += left_box.margin() + right_box.margin();
                let overlap = left_box.overlap(&right_box);
                let area = left_box.area(self.mode) + right_box.area(self.mode);
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
                best_dim = dim;
                best_margin = margin_sum;
                best_split = dim_best_left;
            }
            sorted_per_dim.push(idxs);
        }

        let chosen = &sorted_per_dim[best_dim];
        let left_cells = chosen[..best_split]
            .iter()
            .map(|&i| all_cells[i].clone())
            .collect();
        let right_cells = chosen[best_split..]
            .iter()
            .map(|&i| all_cells[i].clone())
            .collect();
        (left_cells, right_cells)
    }

    fn adjust_ancestors(
        &self,
        ctx: &mut dyn VTabHostContext,
        ancestors: &[i64],
        mut child_id: i64,
        mut child_bbox: RTreeCell,
        root_depth: usize,
    ) -> Result<()> {
        for &parent_id in ancestors.iter().rev() {
            let parent_blob = self.read_node_blob(ctx, parent_id)?;
            let depth_field = if parent_id == 1 { root_depth as u16 } else { 0 };
            let mut p_cells = self.decode_node_cells(&parent_blob);
            let Some(pos) = p_cells.iter().position(|c| c.id == child_id) else {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    "missing child entry in parent RTree node",
                ));
            };
            p_cells[pos] = child_bbox;
            let encoded = self.encode_node_cells(depth_field, &p_cells);
            self.write_node_blob(ctx, parent_id, &encoded)?;
            child_id = parent_id;
            child_bbox = self.compute_bbox(&p_cells, parent_id);
        }
        Ok(())
    }

    fn insert_cell_at_height(
        &self,
        ctx: &mut dyn VTabHostContext,
        cell: RTreeCell,
        target_height: usize,
    ) -> Result<()> {
        let (root_depth, path) = self.choose_subtree_path(ctx, &cell, target_height)?;
        self.insert_into_path(ctx, &path, cell, target_height, root_depth)
    }

    fn insert_into_path(
        &self,
        ctx: &mut dyn VTabHostContext,
        path: &[i64],
        cell: RTreeCell,
        height: usize,
        root_depth: usize,
    ) -> Result<()> {
        let node_id = *path.last().unwrap_or(&1);
        let ancestors = &path[..path.len().saturating_sub(1)];
        let node_blob = self.read_node_blob(ctx, node_id)?;
        let mut cells = self.decode_node_cells(&node_blob);

        if cells.len() < self.max_cells() {
            cells.push(cell.clone());
            let depth_field = if node_id == 1 { root_depth as u16 } else { 0 };
            let encoded = self.encode_node_cells(depth_field, &cells);
            self.write_node_blob(ctx, node_id, &encoded)?;
            self.update_child_mapping(ctx, cell.id, node_id, height)?;
            if !ancestors.is_empty() {
                let bbox = self.compute_bbox(&cells, node_id);
                self.adjust_ancestors(ctx, ancestors, node_id, bbox, root_depth)?;
            }
            return Ok(());
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
            let left_blob = self.encode_node_cells(0, &left_cells);
            let right_blob = self.encode_node_cells(0, &right_cells);
            let left_id = self.alloc_new_node(ctx, &left_blob)?;
            let right_id = self.alloc_new_node(ctx, &right_blob)?;

            for c in &left_cells {
                self.update_child_mapping(ctx, c.id, left_id, height)?;
            }
            for c in &right_cells {
                self.update_child_mapping(ctx, c.id, right_id, height)?;
            }

            let left_bbox = self.compute_bbox(&left_cells, left_id);
            let right_bbox = self.compute_bbox(&right_cells, right_id);
            let new_depth = (root_depth + 1) as u16;
            let root_cells = vec![left_bbox, right_bbox];
            let root_encoded = self.encode_node_cells(new_depth, &root_cells);
            self.write_node_blob(ctx, 1, &root_encoded)?;
            self.write_parent_mapping(ctx, left_id, 1)?;
            self.write_parent_mapping(ctx, right_id, 1)?;
            Ok(())
        } else {
            let left_encoded = self.encode_node_cells(0, &left_cells);
            self.write_node_blob(ctx, node_id, &left_encoded)?;
            for c in &left_cells {
                self.update_child_mapping(ctx, c.id, node_id, height)?;
            }

            let right_encoded = self.encode_node_cells(0, &right_cells);
            let right_id = self.alloc_new_node(ctx, &right_encoded)?;
            for c in &right_cells {
                self.update_child_mapping(ctx, c.id, right_id, height)?;
            }

            let left_bbox = self.compute_bbox(&left_cells, node_id);
            self.adjust_ancestors(ctx, ancestors, node_id, left_bbox, root_depth)?;

            let right_bbox = self.compute_bbox(&right_cells, right_id);
            self.insert_into_path(ctx, ancestors, right_bbox, height + 1, root_depth)
        }
    }

    fn build_leaf_path(&self, ctx: &mut dyn VTabHostContext, leaf_id: i64) -> Result<Vec<i64>> {
        let mut rev_path = vec![leaf_id];
        let mut cur = leaf_id;
        while cur != 1 {
            let Some(parent) = self.read_parent_nodeno(ctx, cur)? else {
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

    fn delete_rowid_entry(&self, ctx: &mut dyn VTabHostContext, rowid: i64) -> Result<()> {
        let root_blob = self.read_node_blob(ctx, 1)?;
        let mut root_depth = read_u16_be(&root_blob[0..2]) as usize;

        let Some(leaf_id) = self.read_rowid_nodeno(ctx, rowid)? else {
            return Ok(());
        };
        let path = self.build_leaf_path(ctx, leaf_id)?;
        let mut orphaned: Vec<(usize, RTreeCell)> = Vec::new();

        self.remove_entry_along_path(ctx, &path, rowid, 0, &mut root_depth, &mut orphaned)?;
        self.delete_rowid_mapping(ctx, rowid)?;

        // Shrink root height if root is internal and has only 1 child left
        loop {
            if root_depth == 0 {
                break;
            }
            let r_blob = self.read_node_blob(ctx, 1)?;
            let r_cells = self.decode_node_cells(&r_blob);
            if r_cells.len() != 1 {
                break;
            }
            let sole_child_id = r_cells[0].id;
            let child_blob = self.read_node_blob(ctx, sole_child_id)?;
            let child_cells = self.decode_node_cells(&child_blob);
            self.delete_node_blob(ctx, sole_child_id)?;
            self.delete_parent_mapping(ctx, sole_child_id)?;
            root_depth -= 1;
            let new_root = self.encode_node_cells(root_depth as u16, &child_cells);
            self.write_node_blob(ctx, 1, &new_root)?;
            for c in &child_cells {
                self.update_child_mapping(ctx, c.id, 1, root_depth)?;
            }
        }

        // Reinsert orphaned entries (from higher subtrees down to leaves, matching rtree.c pDeleted LIFO order)
        for (h, cell) in orphaned.into_iter().rev() {
            self.insert_cell_at_height(ctx, cell, h)?;
        }

        Ok(())
    }

    fn remove_entry_along_path(
        &self,
        ctx: &mut dyn VTabHostContext,
        path: &[i64],
        target_id: i64,
        height: usize,
        root_depth: &mut usize,
        orphaned: &mut Vec<(usize, RTreeCell)>,
    ) -> Result<()> {
        let node_id = *path.last().unwrap_or(&1);
        let ancestors = &path[..path.len().saturating_sub(1)];
        let blob = self.read_node_blob(ctx, node_id)?;
        let mut cells = self.decode_node_cells(&blob);
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
            let encoded = self.encode_node_cells(*root_depth as u16, &cells);
            self.write_node_blob(ctx, 1, &encoded)?;
            return Ok(());
        }

        if cells.len() < self.min_cells() {
            for rem in cells {
                orphaned.push((height, rem));
            }
            self.delete_node_blob(ctx, node_id)?;
            self.delete_parent_mapping(ctx, node_id)?;
            self.remove_entry_along_path(ctx, ancestors, node_id, height + 1, root_depth, orphaned)
        } else {
            let encoded = self.encode_node_cells(0, &cells);
            self.write_node_blob(ctx, node_id, &encoded)?;
            let bbox = self.compute_bbox(&cells, node_id);
            self.adjust_ancestors(ctx, ancestors, node_id, bbox, *root_depth)
        }
    }

    fn allocate_new_rowid(&self, ctx: &mut dyn VTabHostContext) -> Result<i64> {
        let tbl = self.q_shadow("rowid");
        let rows = ctx.query_sql(
            &format!("SELECT coalesce(max(rowid), 0) + 1 FROM {tbl}"),
            &[],
        )?;
        Ok(rows
            .first()
            .and_then(|r| r.first())
            .and_then(Value::as_i64)
            .unwrap_or(1))
    }

    fn parse_cell_coords(&self, values: &[Value]) -> Result<Vec<CoordVal>> {
        let mut coords = Vec::with_capacity(self.n_dim2);
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
                    coords.push(CoordVal::F32(lo));
                    coords.push(CoordVal::F32(hi));
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
                    coords.push(CoordVal::I32(lo));
                    coords.push(CoordVal::I32(hi));
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
            rows: Vec::new(),
            pos: 0,
        }))
    }

    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        match op {
            VTabUpdateOperation::Delete { old_rowid } => {
                self.delete_rowid_entry(ctx, value_to_i64(&old_rowid))?;
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
                    if self.read_rowid_nodeno(ctx, id)?.is_some() {
                        if ctx.vtab_on_conflict() == VTabConflictMode::Replace {
                            self.delete_rowid_entry(ctx, id)?;
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
                    self.allocate_new_rowid(ctx)?
                };

                let cell = RTreeCell {
                    id: final_id,
                    coords,
                };
                self.insert_cell_at_height(ctx, cell, 0)?;
                if self.n_aux > 0 {
                    let aux_slice = if values.len() > 1 + self.n_dim2 {
                        &values[1 + self.n_dim2..]
                    } else {
                        &[]
                    };
                    self.write_aux_values(ctx, final_id, aux_slice)?;
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

                if target_id != old_id && self.read_rowid_nodeno(ctx, target_id)?.is_some() {
                    if ctx.vtab_on_conflict() == VTabConflictMode::Replace {
                        self.delete_rowid_entry(ctx, target_id)?;
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

                self.delete_rowid_entry(ctx, old_id)?;
                let cell = RTreeCell {
                    id: target_id,
                    coords,
                };
                self.insert_cell_at_height(ctx, cell, 0)?;
                if self.n_aux > 0 {
                    let aux_slice = if values.len() > 1 + self.n_dim2 {
                        &values[1 + self.n_dim2..]
                    } else {
                        &[]
                    };
                    self.write_aux_values(ctx, target_id, aux_slice)?;
                }
                Ok(Some(target_id))
            }
        }
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
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
        Ok(())
    }

    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let q_db = quote_ident(&self.db_name);
        for suffix in ["node", "parent", "rowid"] {
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

#[derive(Debug, Clone)]
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

    fn matches_nonleaf(&self, cell: &RTreeCell) -> bool {
        let pair_base = self.coord_idx & !1;
        let lo = cell.coords[pair_base].as_f64();
        let hi = cell.coords[pair_base + 1].as_f64();
        match self.op {
            SpatialOp::AlwaysTrue => true,
            SpatialOp::AlwaysFalse => false,
            SpatialOp::Eq => self.val >= lo && self.val <= hi,
            SpatialOp::Le | SpatialOp::Lt => self.val >= lo,
            SpatialOp::Ge | SpatialOp::Gt => self.val <= hi,
        }
    }

    fn matches_leaf(&self, cell: &RTreeCell) -> bool {
        let x = cell.coords[self.coord_idx].as_f64();
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

#[derive(Debug)]
struct RTreeResultRow {
    rowid: i64,
    cols: Vec<Value>,
}

struct RTreeCursor {
    table: RTreeTable,
    rows: Vec<RTreeResultRow>,
    pos: usize,
}

impl RTreeCursor {
    fn materialize_row(
        &self,
        ctx: &mut dyn VTabHostContext,
        cell: &RTreeCell,
    ) -> Result<RTreeResultRow> {
        let mut cols = Vec::with_capacity(1 + self.table.n_dim2 + self.table.n_aux);
        cols.push(Value::Integer(cell.id));
        for c in &cell.coords {
            cols.push(c.to_sql_value());
        }
        if self.table.n_aux > 0 {
            let tbl = self.table.q_shadow("rowid");
            let aux_rows = ctx.query_sql(
                &format!("SELECT * FROM {tbl} WHERE rowid = ?1"),
                &[Value::Integer(cell.id)],
            )?;
            if let Some(r) = aux_rows.first() {
                for i in 0..self.table.n_aux {
                    cols.push(r.get(2 + i).cloned().unwrap_or(Value::Null));
                }
            } else {
                cols.resize(1 + self.table.n_dim2 + self.table.n_aux, Value::Null);
            }
        }
        Ok(RTreeResultRow {
            rowid: cell.id,
            cols,
        })
    }

    fn walk_subtree(
        &self,
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
        let blob = self.table.read_node_blob(ctx, node_id)?;
        let cells = self.table.decode_node_cells(&blob);
        if level == 0 {
            for cell in cells {
                if constraints.iter().all(|c| c.matches_leaf(&cell)) {
                    out.push(cell);
                }
            }
        } else {
            for cell in cells {
                if constraints.iter().all(|c| c.matches_nonleaf(&cell)) {
                    self.walk_subtree(ctx, cell.id, level - 1, constraints, visited, out)?;
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
        self.rows.clear();
        self.pos = 0;

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
            let Some(nodeno) = self.table.read_rowid_nodeno(ctx, rowid)? else {
                return Ok(());
            };
            let blob = self.table.read_node_blob(ctx, nodeno)?;
            let cells = self.table.decode_node_cells(&blob);
            let Some(cell) = cells.into_iter().find(|c| c.id == rowid) else {
                return Err(SqliteError::new(
                    ErrorCode::Corrupt,
                    "missing cell in leaf node referenced by _rowid",
                ));
            };
            self.rows.push(self.materialize_row(ctx, &cell)?);
            return Ok(());
        }

        let mut constraints = Vec::new();
        if let Some(s) = idx_str {
            let bytes = s.as_bytes();
            for (arg_idx, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
                if let Some(arg) = argv.get(arg_idx) {
                    let op_ch = char::from(pair[0]);
                    let coord_idx = pair[1].saturating_sub(b'0') as usize;
                    if coord_idx < self.table.n_dim2 {
                        constraints.push(SpatialConstraint::from_arg(op_ch, coord_idx, arg));
                    }
                }
            }
        }
        if constraints.iter().any(|c| c.op == SpatialOp::AlwaysFalse) {
            return Ok(());
        }

        let root_blob = self.table.read_node_blob(ctx, 1)?;
        let root_depth = read_u16_be(&root_blob[0..2]) as usize;
        let mut visited = Vec::with_capacity(root_depth + 2);
        let mut matched_cells = Vec::new();
        self.walk_subtree(
            ctx,
            1,
            root_depth,
            &constraints,
            &mut visited,
            &mut matched_cells,
        )?;
        self.rows.reserve(matched_cells.len());
        for cell in &matched_cells {
            self.rows.push(self.materialize_row(ctx, cell)?);
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        Ok(self
            .rows
            .get(self.pos)
            .and_then(|r| r.cols.get(col_idx))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.rows.get(self.pos).map_or(0, |r| r.rowid))
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

fn parse_numeric_prefix_f64(s: &str) -> f64 {
    let trimmed = s.trim_start();
    if trimmed.is_empty() {
        return 0.0;
    }
    let bytes = trimmed.as_bytes();
    let mut end = 0;
    if bytes.first().is_some_and(|c| *c == b'+' || *c == b'-') {
        end += 1;
    }
    let mut saw_digit = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        saw_digit = true;
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            saw_digit = true;
            end += 1;
        }
    }
    if !saw_digit {
        return 0.0;
    }
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let exp_start = end;
        end += 1;
        if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
            end += 1;
        }
        let mut exp_digits = false;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            exp_digits = true;
            end += 1;
        }
        if !exp_digits {
            end = exp_start;
        }
    }
    trimmed[..end].parse::<f64>().unwrap_or(0.0)
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
