//! Schema catalog (`sqlite_schema` / `sqlite_master` on Page 1), ported from `sqlite/src/build.c`
//! and `sqlite/src/prepare.c`.
#![forbid(unsafe_code)]

use sqliters_btree::Btree;
use sqliters_syntax::FkAction;
use sqliters_syntax::{
    ColumnConstraint, ColumnDef, ColumnRef, CommonTableExpr, ConflictAction, CreateIndexStmt,
    CreateTableStmt, Expr, FromClause, FunctionArgs, IndexedColumn, JoinClause, JoinConstraint,
    LimitClause, OrderByTerm, QualifiedName, ResultColumn, SelectBody, SelectCore, SelectStmt,
    Stmt, TableConstraint, TableOrSubquery, parse_one,
};
use sqliters_types::{
    Collation, ErrorCode, ExecutionServices, KeySortOrder, Pgno, Result, SqliteError, TypeAffinity,
    Value, decode_record_with_encoding, encode_record_with_encoding,
};
use sqliters_vdbe::P4;

#[derive(Debug, Clone)]
pub struct CheckSchema {
    pub label: String,
    pub expr: Expr,
}

#[derive(Debug, Clone)]
pub struct ColumnSchema {
    pub name: String,
    pub type_name: String,
    pub affinity: TypeAffinity,
    pub collation: Collation,
    pub not_null: Option<ConflictAction>,
    pub default_expr: Option<Expr>,
    pub generated: Option<(Expr, bool)>,
    pub is_primary_key: bool,
    /// `true` when the column has an inline `UNIQUE` column constraint (`COLFLAG_UNIQUE` in `sqliteInt.h`).
    pub is_unique: bool,
    /// 1-based position within PRIMARY KEY (`0` if not part of PK), matching `PRAGMA table_info`.
    pub pk_order: usize,
}

impl ColumnSchema {
    /// Extracts a constant `P4` fallback (with column affinity + `STRICT` `REAL` coercion) for
    /// `OP_Column` when reading rows written prior to `ALTER TABLE ... ADD COLUMN`.
    pub fn default_p4_for_table(&self, table_strict: bool) -> P4 {
        let raw_val = match &self.default_expr {
            Some(Expr::Integer(i)) => Some(Value::Integer(*i)),
            Some(Expr::Bool(b)) => Some(Value::Integer(i64::from(*b))),
            Some(Expr::Float(f)) => Some(Value::real(*f)),
            Some(Expr::String(s)) => Some(Value::Text(s.clone())),
            Some(Expr::Blob(b)) => Some(Value::Blob(b.clone())),
            Some(Expr::Unary {
                op: sqliters_syntax::UnaryOp::Plus,
                expr,
            }) => match expr.as_ref() {
                Expr::Integer(i) => Some(Value::Integer(*i)),
                Expr::Float(f) => Some(Value::real(*f)),
                _ => None,
            },
            Some(Expr::Unary {
                op: sqliters_syntax::UnaryOp::Negate,
                expr,
            }) => match expr.as_ref() {
                Expr::Integer(i) => Some(Value::Integer(i.wrapping_neg())),
                Expr::Float(f) => Some(Value::real(-*f)),
                _ => None,
            },
            _ => None,
        };
        let Some(v) = raw_val else {
            return P4::None;
        };
        let mut coerced = self.affinity.apply(v);
        if table_strict
            && self.type_name.trim().eq_ignore_ascii_case("REAL")
            && let Value::Integer(iv) = coerced
        {
            coerced = Value::real(iv as f64);
        }
        match coerced {
            Value::Null => P4::None,
            Value::Integer(i) => P4::Int64(i),
            Value::Real(r) => P4::Real(r),
            Value::Text(s) => P4::Str(s),
            Value::Blob(b) => P4::Blob(b),
        }
    }

    pub fn default_p4(&self) -> P4 {
        self.default_p4_for_table(false)
    }
}

#[derive(Debug, Clone)]
pub struct ForeignKeySchema {
    pub id: usize,
    pub from_cols: Vec<usize>,
    pub from_col_names: Vec<String>,
    pub ref_table: String,
    pub ref_cols: Option<Vec<String>>,
    pub on_delete: FkAction,
    pub on_update: FkAction,
    pub deferred: bool,
}

/// Physical primary key identifying a single row in either a normal rowid Table B+Tree
/// or an index-organized `WITHOUT ROWID` Index B-Tree.
#[derive(Debug, Clone, PartialEq)]
pub enum TableRowKey {
    Rowid(i64),
    WithoutRowidPk(Vec<Value>),
}

impl TableRowKey {
    #[inline]
    pub fn rowid_or_zero(&self) -> i64 {
        match self {
            Self::Rowid(r) => *r,
            Self::WithoutRowidPk(_) => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TableSchema {
    pub schema_rowid: i64,
    pub name: String,
    pub root_pgno: Pgno,
    pub columns: Vec<ColumnSchema>,
    /// Index of the `INTEGER PRIMARY KEY` column that aliases the B+Tree `rowid`, if any (`iPKey` in `sqliteInt.h`).
    pub rowid_alias_col: Option<usize>,
    pub rowid_pk_conflict: ConflictAction,
    pub autoincrement: bool,
    pub without_rowid: bool,
    /// Deduplicated `PRIMARY KEY` columns in PK key order for `WITHOUT ROWID` tables (`pPk->aiColumn[0..nKeyCol]`).
    pub without_rowid_pk_cols: Vec<IndexedColSchema>,
    /// Non-VIRTUAL non-PK column indices in table declaration order stored after `without_rowid_pk_cols` in `root_pgno`.
    pub without_rowid_extra_cols: Vec<usize>,
    pub without_rowid_pk_conflict: ConflictAction,
    pub without_rowid_pk_autoindex_name: String,
    pub strict: bool,
    pub checks: Vec<CheckSchema>,
    pub foreign_keys: Vec<ForeignKeySchema>,
    pub raw_sql: String,
}

impl TableSchema {
    /// Built-in descriptor for `sqlite_schema` / `sqlite_master` stored on root page 1.
    pub fn sqlite_schema_table() -> Self {
        let mk_col = |name: &str, ty: &str, aff: TypeAffinity| ColumnSchema {
            name: name.to_owned(),
            type_name: ty.to_owned(),
            affinity: aff,
            collation: Collation::Binary,
            not_null: None,
            default_expr: None,
            generated: None,
            is_primary_key: false,
            is_unique: false,
            pk_order: 0,
        };
        Self {
            schema_rowid: 0,
            name: "sqlite_schema".to_owned(),
            root_pgno: 1,
            columns: vec![
                mk_col("type", "TEXT", TypeAffinity::Text),
                mk_col("name", "TEXT", TypeAffinity::Text),
                mk_col("tbl_name", "TEXT", TypeAffinity::Text),
                mk_col("rootpage", "INT", TypeAffinity::Integer),
                mk_col("sql", "TEXT", TypeAffinity::Text),
            ],
            rowid_alias_col: None,
            rowid_pk_conflict: ConflictAction::Abort,
            autoincrement: false,
            without_rowid: false,
            without_rowid_pk_cols: Vec::new(),
            without_rowid_extra_cols: Vec::new(),
            without_rowid_pk_conflict: ConflictAction::Abort,
            without_rowid_pk_autoindex_name: String::new(),
            strict: false,
            checks: Vec::new(),
            foreign_keys: Vec::new(),
            raw_sql: "CREATE TABLE sqlite_schema(type text,name text,tbl_name text,rootpage int,sql text)".to_owned(),
        }
    }

    pub fn find_column(&self, name: &str) -> Option<(usize, &ColumnSchema)> {
        self.columns
            .iter()
            .enumerate()
            .find(|(_, c)| c.name.eq_ignore_ascii_case(name))
    }

    /// Returns `true` if `name` refers to the implicit B+Tree rowid (`rowid`, `_rowid_`, or `oid`)
    /// and is NOT shadowed by an explicit user column of the same name.
    pub fn is_implicit_rowid_name(&self, name: &str) -> bool {
        if self.without_rowid || self.find_column(name).is_some() {
            return false;
        }
        name.eq_ignore_ascii_case("rowid")
            || name.eq_ignore_ascii_case("_rowid_")
            || name.eq_ignore_ascii_case("oid")
    }

    /// Key sort orders of the primary Index B-Tree (`nKeyField == without_rowid_pk_cols.len()`) for `WITHOUT ROWID`.
    pub fn without_rowid_pk_orders(&self) -> Vec<KeySortOrder> {
        self.without_rowid_pk_cols
            .iter()
            .map(|ic| ic.sort_order)
            .collect()
    }

    /// Extracts the `nPk` primary key values from a full logical table row (`columns.len()`).
    pub fn extract_without_rowid_pk(&self, logical_vals: &[Value]) -> Vec<Value> {
        self.without_rowid_pk_cols
            .iter()
            .map(|ic| logical_vals[ic.col_idx.expect("PK col has col_idx")].clone())
            .collect()
    }

    /// Maps a 0-based logical table column index (`col_idx`) to its physical record slot in
    /// `root_pgno` (`[without_rowid_pk_cols..., without_rowid_extra_cols...]` on `WITHOUT ROWID`
    /// tables without virtual columns; `col_idx` on rowid tables without virtual columns).
    pub fn physical_record_slot(&self, col_idx: usize) -> usize {
        if !self.without_rowid {
            return col_idx;
        }
        if let Some(pk_pos) = self
            .without_rowid_pk_cols
            .iter()
            .position(|ic| ic.col_idx == Some(col_idx))
        {
            return pk_pos;
        }
        if let Some(extra_pos) = self
            .without_rowid_extra_cols
            .iter()
            .position(|&ci| ci == col_idx)
        {
            return self.without_rowid_pk_cols.len() + extra_pos;
        }
        col_idx
    }

    /// Synthesizes an [`IndexSchema`] descriptor representing the `WITHOUT ROWID` primary B-Tree
    /// (`root_pgno`) for query planning (`IndexSeek`) and `PRAGMA index_list`.
    pub fn without_rowid_pk_as_index(&self) -> Option<IndexSchema> {
        if !self.without_rowid {
            return None;
        }
        Some(IndexSchema {
            schema_rowid: 0,
            name: self.without_rowid_pk_autoindex_name.clone(),
            table_name: self.name.clone(),
            root_pgno: self.root_pgno,
            unique: true,
            conflict: self.without_rowid_pk_conflict,
            columns: self.without_rowid_pk_cols.clone(),
            where_clause: None,
            is_autoindex: true,
            origin: "pk",
            raw_sql: None,
            table_without_rowid: true,
            trailing_pk_cols: Vec::new(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct IndexedColSchema {
    pub col_idx: Option<usize>,
    pub expr: Option<Expr>,
    pub name: String,
    pub sort_order: KeySortOrder,
}

#[derive(Debug, Clone)]
pub struct IndexSchema {
    pub schema_rowid: i64,
    pub name: String,
    pub table_name: String,
    pub root_pgno: Pgno,
    pub unique: bool,
    pub conflict: ConflictAction,
    pub columns: Vec<IndexedColSchema>,
    pub where_clause: Option<Expr>,
    pub is_autoindex: bool,
    /// `"pk"` (PRIMARY KEY), `"u"` (UNIQUE), or `"c"` (CREATE INDEX), matching `PRAGMA index_list`.
    pub origin: &'static str,
    pub raw_sql: Option<String>,
    pub table_without_rowid: bool,
    /// Non-redundant PK columns appended after `columns` on secondary indexes of `WITHOUT ROWID` tables.
    pub trailing_pk_cols: Vec<IndexedColSchema>,
}

impl IndexSchema {
    /// Returns the full `KeySortOrder` slice for the on-disk Index B-Tree record:
    /// - Rowid table index: `[indexed_col_0, ..., indexed_col_{k-1}, trailing_rowid]`
    /// - `WITHOUT ROWID` index: `[indexed_col_0, ..., indexed_col_{k-1}, trailing_pk_cols...]`
    pub fn key_sort_orders(&self) -> Vec<KeySortOrder> {
        let mut orders: Vec<KeySortOrder> = self.columns.iter().map(|c| c.sort_order).collect();
        if self.table_without_rowid {
            orders.extend(self.trailing_pk_cols.iter().map(|c| c.sort_order));
        } else {
            // Trailing table rowid tie-breaker is always ASC BINARY
            orders.push(KeySortOrder::default());
        }
        orders
    }

    /// Reconstructs the `nPk` primary key tuple of `tbl` from a decoded index record `hit`
    /// (`[columns..., trailing_pk_cols...]`, or `[pk_cols..., extra_cols...]` when `self.root_pgno == tbl.root_pgno`).
    pub fn extract_without_rowid_pk_from_hit(
        &self,
        tbl: &TableSchema,
        hit: &[Value],
    ) -> Vec<Value> {
        if self.origin == "pk" && self.root_pgno == tbl.root_pgno {
            let n = tbl.without_rowid_pk_cols.len().min(hit.len());
            return hit[..n].to_vec();
        }
        let mut pk = Vec::with_capacity(tbl.without_rowid_pk_cols.len());
        for pk_col in &tbl.without_rowid_pk_cols {
            if let Some(slot) = self.without_rowid_pk_slot_in_index(pk_col) {
                pk.push(hit.get(slot).cloned().unwrap_or(Value::Null));
            } else {
                pk.push(Value::Null);
            }
        }
        pk
    }

    /// Locates the physical record slot (`[columns..., trailing_pk_cols...]`) in this secondary
    /// index that stores `pk_col` of its `WITHOUT ROWID` table.
    pub fn without_rowid_pk_slot_in_index(&self, pk_col: &IndexedColSchema) -> Option<usize> {
        if let Some(pos) = self.columns.iter().position(|ic| {
            ic.col_idx == pk_col.col_idx && ic.sort_order.collation == pk_col.sort_order.collation
        }) {
            return Some(pos);
        }
        self.trailing_pk_cols
            .iter()
            .position(|tc| {
                tc.col_idx == pk_col.col_idx
                    && tc.sort_order.collation == pk_col.sort_order.collation
            })
            .map(|tpos| self.columns.len() + tpos)
    }
}

/// Pending inline autoindex (`sqlite_autoindex_<table>_<n>`) derived from `CREATE TABLE` constraints.
#[derive(Debug, Clone)]
pub struct PendingAutoIndex {
    pub name: String,
    pub unique: bool,
    pub conflict: ConflictAction,
    pub origin: &'static str,
    pub columns: Vec<IndexedColSchema>,
    pub trailing_pk_cols: Vec<IndexedColSchema>,
}

/// Parsed view entry from `sqlite_schema` / `sqlite_temp_schema` (`type = 'view'`, `rootpage = 0`).
#[derive(Debug, Clone)]
pub struct ViewSchema {
    pub schema_rowid: i64,
    pub name: String,
    pub columns: Option<Vec<String>>,
    pub select: SelectStmt,
    pub raw_sql: String,
}

/// Complete in-memory catalog of tables, indexes, and views loaded from `sqlite_schema` (Page 1).
#[derive(Debug, Clone, Default)]
pub struct Schema {
    pub tables: Vec<TableSchema>,
    pub indexes: Vec<IndexSchema>,
    pub views: Vec<ViewSchema>,
    pub schema_cookie: u32,
    pub loaded: bool,
    pub had_pages: bool,
}

impl Schema {
    pub fn is_schema_table_name(name: &str) -> bool {
        name.eq_ignore_ascii_case("sqlite_schema")
            || name.eq_ignore_ascii_case("sqlite_master")
            || name.eq_ignore_ascii_case("sqlite_temp_schema")
            || name.eq_ignore_ascii_case("sqlite_temp_master")
    }

    pub fn is_temp_schema_table_name(name: &str) -> bool {
        name.eq_ignore_ascii_case("sqlite_temp_schema")
            || name.eq_ignore_ascii_case("sqlite_temp_master")
    }

    pub fn find_table(&self, name: &str) -> Option<TableSchema> {
        if Self::is_temp_schema_table_name(name) {
            let mut tbl = TableSchema::sqlite_schema_table();
            tbl.name = "sqlite_temp_schema".to_owned();
            tbl.root_pgno = 0;
            return Some(tbl);
        }
        if Self::is_schema_table_name(name) {
            return Some(TableSchema::sqlite_schema_table());
        }
        self.tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .cloned()
    }

    pub fn find_user_table(&self, name: &str) -> Option<&TableSchema> {
        self.tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    pub fn find_user_table_mut(&mut self, name: &str) -> Option<&mut TableSchema> {
        self.tables
            .iter_mut()
            .find(|t| t.name.eq_ignore_ascii_case(name))
    }

    pub fn find_view(&self, name: &str) -> Option<&ViewSchema> {
        self.views
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(name))
    }

    pub fn find_view_mut(&mut self, name: &str) -> Option<&mut ViewSchema> {
        self.views
            .iter_mut()
            .find(|v| v.name.eq_ignore_ascii_case(name))
    }

    pub fn find_index(&self, name: &str) -> Option<&IndexSchema> {
        self.indexes
            .iter()
            .find(|idx| idx.name.eq_ignore_ascii_case(name))
    }

    pub fn find_index_including_without_rowid_pk(&self, name: &str) -> Option<IndexSchema> {
        if let Some(idx) = self.find_index(name) {
            return Some(idx.clone());
        }
        self.tables
            .iter()
            .filter_map(TableSchema::without_rowid_pk_as_index)
            .find(|idx| idx.name.eq_ignore_ascii_case(name))
    }

    pub fn indexes_for_table(&self, table_name: &str) -> Vec<IndexSchema> {
        self.indexes
            .iter()
            .filter(|idx| idx.table_name.eq_ignore_ascii_case(table_name))
            .cloned()
            .collect()
    }

    /// Returns all active B-Tree root page numbers (`[1, tables..., indexes...]`) for `PRAGMA integrity_check`.
    pub fn all_btree_roots(&self) -> Vec<Pgno> {
        let mut roots = vec![1];
        for t in &self.tables {
            if t.root_pgno > 1 {
                roots.push(t.root_pgno);
            }
        }
        for idx in &self.indexes {
            if idx.root_pgno > 1 {
                roots.push(idx.root_pgno);
            }
        }
        roots
    }

    /// Ensures `self` matches the on-disk `schema_cookie` and page presence inside an active read/write transaction.
    pub fn ensure_fresh_in_tx(&mut self, btree: &mut Btree) -> Result<()> {
        let has_pages = btree.pager().page_count() > 0;
        let cur_cookie = btree.pager().header().schema_cookie;
        if !self.loaded || self.had_pages != has_pages || self.schema_cookie != cur_cookie {
            *self = Self::load(btree)?;
        }
        Ok(())
    }

    /// Ensures `self` matches the on-disk `schema_cookie` and page presence inside an active
    /// read/write transaction, validating permanent-view `SchemaFixer` constraints against `db_schema_name`.
    pub fn ensure_fresh_in_tx_for_schema(
        &mut self,
        btree: &mut Btree,
        db_schema_name: &str,
    ) -> Result<()> {
        let has_pages = btree.pager().page_count() > 0;
        let cur_cookie = btree.pager().header().schema_cookie;
        if !self.loaded || self.had_pages != has_pages || self.schema_cookie != cur_cookie {
            *self = Self::load_for_schema(btree, Some(db_schema_name))?;
        }
        Ok(())
    }

    /// Loads the schema catalog from Page 1 (`sqlite_schema`).
    pub fn load(btree: &mut Btree) -> Result<Self> {
        Self::load_for_schema(btree, None)
    }

    /// Loads the schema catalog from Page 1 (`sqlite_schema`), validating views against
    /// `db_schema_name` (`sqlite3InitOne` / `sqlite3FixInit` parity).
    pub fn load_for_schema(btree: &mut Btree, db_schema_name: Option<&str>) -> Result<Self> {
        let need_read_tx = btree.transaction_state() == sqliters_pager::TransactionState::Open;
        if need_read_tx {
            btree.begin_read()?;
        }

        let res = if btree.pager().page_count() == 0 {
            Ok(Self {
                tables: Vec::new(),
                indexes: Vec::new(),
                views: Vec::new(),
                schema_cookie: btree.pager().header().schema_cookie,
                loaded: true,
                had_pages: false,
            })
        } else {
            Self::load_inner(btree, db_schema_name)
        };

        if need_read_tx {
            let _ = btree.pager_mut().end_read_if_unused();
        }
        res
    }

    fn load_inner(btree: &mut Btree, db_schema_name: Option<&str>) -> Result<Self> {
        let enc = btree.encoding();
        let writable_schema = btree.writable_schema();
        let raw_rows = scan_table_incremental(btree, 1)?;
        let mut tables = Vec::new();
        let mut views = Vec::new();
        let mut pending_auto_by_table: Vec<(String, Vec<PendingAutoIndex>)> = Vec::new();
        let mut raw_index_rows: Vec<(i64, String, String, Pgno, Option<String>)> = Vec::new();

        for (rowid, payload) in raw_rows {
            let cols = decode_record_with_encoding(&payload, enc)?;
            if cols.len() < 5 {
                continue;
            }
            let obj_type = cols[0].as_str().unwrap_or("");
            let name = cols[1].as_str().unwrap_or("").to_owned();
            let tbl_name = cols[2].as_str().unwrap_or("").to_owned();
            let rootpage = cols[3].as_i64().unwrap_or(0) as Pgno;
            let sql_opt = cols[4].as_str().map(str::to_owned);

            match obj_type {
                "table" => {
                    if let Some(sql_text) = sql_opt {
                        let (Some(Stmt::CreateTable(ct)), _) =
                            parse_one(&sql_text).map_err(|_| {
                                SqliteError::corrupt("database disk image is malformed")
                            })?
                        else {
                            return Err(SqliteError::corrupt("database disk image is malformed"));
                        };
                        let (mut tbl, autos) = build_table_schema_from_ast(&ct, rootpage)?;
                        tbl.schema_rowid = rowid;
                        tbl.name = name.clone();
                        tbl.raw_sql = sql_text;
                        pending_auto_by_table.push((name, autos));
                        tables.push(tbl);
                    }
                }
                "index" => {
                    raw_index_rows.push((rowid, name, tbl_name, rootpage, sql_opt));
                }
                "view" => {
                    if let Some(sql_text) = sql_opt {
                        let parsed = parse_one(&sql_text);
                        let cv = match parsed {
                            Ok((Some(Stmt::CreateView(cv)), _)) => cv,
                            _ => {
                                if writable_schema {
                                    continue;
                                }
                                return Err(SqliteError::corrupt(
                                    "database disk image is malformed",
                                ));
                            }
                        };
                        if let Err(e) = validate_view_no_parameters(&cv.query) {
                            if writable_schema {
                                continue;
                            }
                            return Err(SqliteError::corrupt(format!(
                                "malformed database schema ({name}) - {}",
                                e.message
                            )));
                        }
                        let mut fixed_select = *cv.query;
                        if let Some(sname) = db_schema_name
                            && !sname.eq_ignore_ascii_case("temp")
                        {
                            match fix_permanent_view_select(&name, sname, &fixed_select) {
                                Ok(sel) => fixed_select = sel,
                                Err(e) => {
                                    if writable_schema {
                                        continue;
                                    }
                                    return Err(SqliteError::corrupt(format!(
                                        "malformed database schema ({name}) - {}",
                                        e.message
                                    )));
                                }
                            }
                        }
                        views.push(ViewSchema {
                            schema_rowid: rowid,
                            name,
                            columns: cv.columns,
                            select: fixed_select,
                            raw_sql: sql_text,
                        });
                    }
                }
                _ => {
                    // Triggers ignored until Phase 4
                }
            }
        }

        let mut indexes = Vec::new();
        for (rowid, idx_name, tbl_name, rootpage, sql_opt) in raw_index_rows {
            let Some(tbl) = tables
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(&tbl_name))
            else {
                continue;
            };

            if let Some(sql_text) = sql_opt {
                let (Some(Stmt::CreateIndex(ci)), _) = parse_one(&sql_text)
                    .map_err(|_| SqliteError::corrupt("database disk image is malformed"))?
                else {
                    return Err(SqliteError::corrupt("database disk image is malformed"));
                };
                let mut idx = build_index_schema_from_ast(&ci, tbl, rootpage)?;
                idx.schema_rowid = rowid;
                idx.name = idx_name;
                idx.raw_sql = Some(sql_text);
                indexes.push(idx);
            } else {
                // SQLite internal autoindex (`sql IS NULL`): match against inline constraints of `tbl_name`
                if let Some((_, autos)) = pending_auto_by_table
                    .iter()
                    .find(|(tn, _)| tn.eq_ignore_ascii_case(&tbl_name))
                    && let Some(auto) = autos
                        .iter()
                        .find(|a| a.name.eq_ignore_ascii_case(&idx_name))
                {
                    indexes.push(IndexSchema {
                        schema_rowid: rowid,
                        name: idx_name,
                        table_name: tbl.name.clone(),
                        root_pgno: rootpage,
                        unique: auto.unique,
                        conflict: auto.conflict,
                        columns: auto.columns.clone(),
                        where_clause: None,
                        is_autoindex: true,
                        origin: auto.origin,
                        raw_sql: None,
                        table_without_rowid: tbl.without_rowid,
                        trailing_pk_cols: auto.trailing_pk_cols.clone(),
                    });
                }
            }
        }

        Ok(Self {
            tables,
            indexes,
            views,
            schema_cookie: btree.pager().header().schema_cookie,
            loaded: true,
            had_pages: true,
        })
    }
}

/// Builds a [`TableSchema`] and any inline [`PendingAutoIndex`] definitions from a parsed [`CreateTableStmt`],
/// matching C SQLite's exact `iPKey` rowid-alias and `sqlite_autoindex_<table>_<N>` rules (`sqlite/src/build.c`).
pub fn build_table_schema_from_ast(
    ct: &CreateTableStmt,
    root_pgno: Pgno,
) -> Result<(TableSchema, Vec<PendingAutoIndex>)> {
    let tbl_name = ct.name.name.as_str();
    let mut columns = Vec::with_capacity(ct.columns.len());
    let mut checks = Vec::new();
    let mut foreign_keys = Vec::new();
    let mut rowid_alias_col: Option<usize> = None;
    let mut rowid_pk_conflict = ConflictAction::Abort;
    let mut autoincrement = false;
    let mut has_pk = false;
    let mut inline_autos: Vec<PendingAutoIndex> = Vec::new();

    for (col_idx, cdef) in ct.columns.iter().enumerate() {
        // Check duplicate column name
        if columns
            .iter()
            .any(|existing: &ColumnSchema| existing.name.eq_ignore_ascii_case(&cdef.name))
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("duplicate column name: {}", cdef.name),
            ));
        }

        let affinity = resolve_column_affinity(tbl_name, &cdef.name, &cdef.type_name, ct.strict)?;
        let mut collation = Collation::Binary;
        let mut not_null = None;
        let mut default_expr = None;
        let mut generated = None;
        let mut is_pk = false;
        let mut pk_desc = false;
        let mut pk_conflict = ConflictAction::Abort;
        let mut pk_autoinc = false;
        let mut unique_constraints = Vec::new();

        for cons in &cdef.constraints {
            match cons {
                ColumnConstraint::PrimaryKey {
                    desc,
                    conflict,
                    autoincrement: autoinc,
                } => {
                    if has_pk {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("table \"{tbl_name}\" has more than one primary key"),
                        ));
                    }
                    has_pk = true;
                    is_pk = true;
                    pk_desc = *desc;
                    pk_conflict = *conflict;
                    pk_autoinc = *autoinc;
                }
                ColumnConstraint::NotNull { conflict } => {
                    not_null = Some(*conflict);
                }
                ColumnConstraint::Null => {}
                ColumnConstraint::Unique { conflict } => {
                    unique_constraints.push(*conflict);
                }
                ColumnConstraint::Default(expr) => {
                    default_expr = Some(expr.clone());
                }
                ColumnConstraint::Check {
                    name: cname,
                    expr_sql,
                    expr,
                } => {
                    checks.push(CheckSchema {
                        label: cname.clone().unwrap_or_else(|| expr_sql.clone()),
                        expr: expr.clone(),
                    });
                }
                ColumnConstraint::Generated { expr, stored } => {
                    generated = Some((expr.clone(), *stored));
                }
                ColumnConstraint::Collate(cname) => {
                    collation = Collation::from_name(cname).ok_or_else(|| {
                        SqliteError::new(
                            ErrorCode::Error,
                            format!("no such collation sequence: {cname}"),
                        )
                    })?;
                }
                ColumnConstraint::ForeignKey {
                    ref_table,
                    ref_columns,
                    on_delete,
                    on_update,
                    deferred,
                } => {
                    if ref_columns.len() > 1 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "number of columns in foreign key does not match the number of columns in the referenced table",
                        ));
                    }
                    let fk_id = foreign_keys.len();
                    foreign_keys.push(ForeignKeySchema {
                        id: fk_id,
                        from_cols: vec![col_idx],
                        from_col_names: vec![cdef.name.clone()],
                        ref_table: ref_table.clone(),
                        ref_cols: if ref_columns.is_empty() {
                            None
                        } else {
                            Some(ref_columns.clone())
                        },
                        on_delete: *on_delete,
                        on_update: *on_update,
                        deferred: *deferred,
                    });
                }
            }
        }

        let pk_order = usize::from(is_pk);
        columns.push(ColumnSchema {
            name: cdef.name.clone(),
            type_name: cdef.type_name.clone(),
            affinity,
            collation,
            not_null,
            default_expr,
            generated,
            is_primary_key: is_pk,
            is_unique: !unique_constraints.is_empty(),
            pk_order,
        });

        if is_pk {
            // In SQLite, `col INTEGER PRIMARY KEY` (exact declared type `INTEGER` and not `WITHOUT ROWID` and not `DESC`)
            // aliases the B+Tree rowid (`pTab->iPKey = col_idx`) and does NOT create an autoindex!
            if !ct.without_rowid
                && !pk_desc
                && cdef.type_name.trim().eq_ignore_ascii_case("INTEGER")
            {
                rowid_alias_col = Some(col_idx);
                rowid_pk_conflict = pk_conflict;
                autoincrement = pk_autoinc;
            } else {
                if pk_autoinc {
                    let msg = if ct.without_rowid {
                        "AUTOINCREMENT not allowed on WITHOUT ROWID tables"
                    } else {
                        "AUTOINCREMENT is only allowed on an INTEGER PRIMARY KEY"
                    };
                    return Err(SqliteError::new(ErrorCode::Error, msg));
                }
                push_inline_autoindex(
                    tbl_name,
                    &mut inline_autos,
                    pk_conflict,
                    "pk",
                    vec![IndexedColSchema {
                        col_idx: Some(col_idx),
                        expr: None,
                        name: cdef.name.clone(),
                        sort_order: KeySortOrder {
                            desc: pk_desc,
                            nulls_last: pk_desc,
                            collation,
                        },
                    }],
                )?;
            }
        }

        for u_conflict in unique_constraints {
            push_inline_autoindex(
                tbl_name,
                &mut inline_autos,
                u_conflict,
                "u",
                vec![IndexedColSchema {
                    col_idx: Some(col_idx),
                    expr: None,
                    name: cdef.name.clone(),
                    sort_order: KeySortOrder {
                        desc: false,
                        nulls_last: false,
                        collation,
                    },
                }],
            )?;
        }
    }

    // Process table-level constraints (`PRIMARY KEY(...)`, `UNIQUE(...)`, `CHECK(...)`)
    for tcons in &ct.constraints {
        match tcons {
            TableConstraint::PrimaryKey {
                columns: pk_cols,
                conflict,
            } => {
                if has_pk {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("table \"{tbl_name}\" has more than one primary key"),
                    ));
                }
                has_pk = true;
                let resolved = resolve_indexed_columns(&columns, pk_cols, false)?;
                for (ord, ic) in resolved.iter().enumerate() {
                    if let Some(ci) = ic.col_idx {
                        columns[ci].is_primary_key = true;
                        columns[ci].pk_order = ord + 1;
                    }
                }

                // Single-column `PRIMARY KEY(col)` (ASC) on a column declared `INTEGER` is also a rowid alias!
                if !ct.without_rowid
                    && resolved.len() == 1
                    && !resolved[0].sort_order.desc
                    && let Some(pk_ci) = resolved[0].col_idx
                    && columns[pk_ci]
                        .type_name
                        .trim()
                        .eq_ignore_ascii_case("INTEGER")
                {
                    rowid_alias_col = Some(pk_ci);
                    rowid_pk_conflict = *conflict;
                } else {
                    push_inline_autoindex(tbl_name, &mut inline_autos, *conflict, "pk", resolved)?;
                }
            }
            TableConstraint::Unique {
                columns: u_cols,
                conflict,
            } => {
                let resolved = resolve_indexed_columns(&columns, u_cols, false)?;
                push_inline_autoindex(tbl_name, &mut inline_autos, *conflict, "u", resolved)?;
            }
            TableConstraint::Check {
                name: cname,
                expr_sql,
                expr,
            } => {
                checks.push(CheckSchema {
                    label: cname.clone().unwrap_or_else(|| expr_sql.clone()),
                    expr: expr.clone(),
                });
            }
            TableConstraint::ForeignKey {
                columns: fk_cols,
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                deferred,
            } => {
                let mut from_cols = Vec::with_capacity(fk_cols.len());
                let mut from_col_names = Vec::with_capacity(fk_cols.len());
                for cname in fk_cols {
                    let Some((ci, cs)) = columns
                        .iter()
                        .enumerate()
                        .find(|(_, c)| c.name.eq_ignore_ascii_case(cname))
                    else {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("unknown column \"{cname}\" in foreign key definition"),
                        ));
                    };
                    from_cols.push(ci);
                    from_col_names.push(cs.name.clone());
                }
                if !ref_columns.is_empty() && ref_columns.len() != from_cols.len() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "number of columns in foreign key does not match the number of columns in the referenced table",
                    ));
                }
                let fk_id = foreign_keys.len();
                foreign_keys.push(ForeignKeySchema {
                    id: fk_id,
                    from_cols,
                    from_col_names,
                    ref_table: ref_table.clone(),
                    ref_cols: if ref_columns.is_empty() {
                        None
                    } else {
                        Some(ref_columns.clone())
                    },
                    on_delete: *on_delete,
                    on_update: *on_update,
                    deferred: *deferred,
                });
            }
        }
    }

    if columns.iter().all(|c| c.generated.is_some()) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "must have at least one non-generated column",
        ));
    }

    if ct.strict || ct.without_rowid {
        for (ci, col) in columns.iter_mut().enumerate() {
            if col.is_primary_key && rowid_alias_col != Some(ci) && col.not_null.is_none() {
                col.not_null = Some(ConflictAction::Abort);
            }
        }
    }

    let mut without_rowid_pk_cols = Vec::new();
    let mut without_rowid_extra_cols = Vec::new();
    let mut without_rowid_pk_conflict = ConflictAction::Abort;
    let mut without_rowid_pk_autoindex_name = String::new();

    if ct.without_rowid {
        if !has_pk {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("PRIMARY KEY missing on table {tbl_name}"),
            ));
        }
        // `convertToWithoutRowidTable` in `sqlite/src/build.c`:
        // 1. Locate the `"pk"` autoindex and deduplicate repeated `(col_idx, collation)` columns.
        let Some(pk_pos) = inline_autos.iter().position(|a| a.origin == "pk") else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("PRIMARY KEY missing on table {tbl_name}"),
            ));
        };
        let pk_auto = inline_autos.remove(pk_pos);
        without_rowid_pk_conflict = pk_auto.conflict;
        without_rowid_pk_autoindex_name = pk_auto.name;

        for ic in pk_auto.columns {
            if let Some(ci) = ic.col_idx
                && columns[ci]
                    .generated
                    .as_ref()
                    .is_some_and(|(_, stored)| !*stored)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot use VIRTUAL columns in PRIMARY KEY",
                ));
            }
            let is_dup = without_rowid_pk_cols.iter().any(|prev: &IndexedColSchema| {
                prev.col_idx == ic.col_idx && prev.sort_order.collation == ic.sort_order.collation
            });
            if !is_dup {
                without_rowid_pk_cols.push(ic);
            }
        }

        // 2. Append non-redundant PK columns (`desc = false` per `build.c:2558`) to all inline `UNIQUE` autoindexes.
        for u_auto in &mut inline_autos {
            for pk_ic in &without_rowid_pk_cols {
                let is_dup = u_auto.columns.iter().any(|ic| {
                    ic.col_idx == pk_ic.col_idx
                        && ic.sort_order.collation == pk_ic.sort_order.collation
                });
                if !is_dup {
                    let mut trailing = pk_ic.clone();
                    trailing.sort_order.desc = false;
                    trailing.sort_order.nulls_last = false;
                    u_auto.trailing_pk_cols.push(trailing);
                }
            }
        }

        // 3. Collect remaining non-VIRTUAL columns in table declaration order (`build.c:2572-2587`).
        for (ci, col) in columns.iter().enumerate() {
            let is_virt = col.generated.as_ref().is_some_and(|(_, stored)| !*stored);
            if is_virt {
                continue;
            }
            let in_pk = without_rowid_pk_cols
                .iter()
                .any(|pk_ic| pk_ic.col_idx == Some(ci));
            if !in_pk {
                without_rowid_extra_cols.push(ci);
            }
        }
    }

    let tbl = TableSchema {
        schema_rowid: 0,
        name: ct.name.name.clone(),
        root_pgno,
        columns,
        rowid_alias_col,
        rowid_pk_conflict,
        autoincrement,
        without_rowid: ct.without_rowid,
        without_rowid_pk_cols,
        without_rowid_extra_cols,
        without_rowid_pk_conflict,
        without_rowid_pk_autoindex_name,
        strict: ct.strict,
        checks,
        foreign_keys,
        raw_sql: ct.catalog_sql.clone(),
    };

    Ok((tbl, inline_autos))
}

fn push_inline_autoindex(
    tbl_name: &str,
    inline_autos: &mut Vec<PendingAutoIndex>,
    conflict: ConflictAction,
    origin: &'static str,
    columns: Vec<IndexedColSchema>,
) -> Result<()> {
    // Per `sqlite/src/build.c:4425-4458`: suppress duplicate inline UNIQUE/PK constraints
    // that have identical column indices and collating sequences.
    for existing in inline_autos.iter_mut() {
        if existing.columns.len() == columns.len()
            && existing.columns.iter().zip(columns.iter()).all(|(a, b)| {
                a.col_idx == b.col_idx && a.sort_order.collation == b.sort_order.collation
            })
        {
            if origin == "pk" {
                existing.origin = "pk";
                existing.columns = columns;
            }
            if existing.conflict != conflict {
                if existing.conflict == ConflictAction::Abort {
                    existing.conflict = conflict;
                } else if conflict != ConflictAction::Abort {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "conflicting ON CONFLICT clauses specified",
                    ));
                }
            }
            return Ok(());
        }
    }
    let idx_num = inline_autos.len() + 1;
    inline_autos.push(PendingAutoIndex {
        name: format!("sqlite_autoindex_{tbl_name}_{idx_num}"),
        unique: true,
        conflict,
        origin,
        columns,
        trailing_pk_cols: Vec::new(),
    });
    Ok(())
}

pub fn build_index_schema_from_ast(
    ci: &CreateIndexStmt,
    tbl: &TableSchema,
    root_pgno: Pgno,
) -> Result<IndexSchema> {
    let resolved = resolve_indexed_columns(&tbl.columns, &ci.columns, true)?;
    let mut trailing_pk_cols = Vec::new();
    if tbl.without_rowid {
        // Per `sqlite3CreateIndex` (`sqlite/src/build.c:4359-4372`): append non-duplicate PK columns
        // preserving `pPk->aSortOrder[j]`!
        for pk_ic in &tbl.without_rowid_pk_cols {
            let is_dup = resolved.iter().any(|ic| {
                ic.col_idx == pk_ic.col_idx && ic.sort_order.collation == pk_ic.sort_order.collation
            });
            if !is_dup {
                trailing_pk_cols.push(pk_ic.clone());
            }
        }
    }
    Ok(IndexSchema {
        schema_rowid: 0,
        name: ci.name.name.clone(),
        table_name: tbl.name.clone(),
        root_pgno,
        unique: ci.unique,
        conflict: ConflictAction::Abort,
        columns: resolved,
        where_clause: ci.where_clause.clone(),
        is_autoindex: false,
        origin: "c",
        raw_sql: Some(ci.catalog_sql.clone()),
        table_without_rowid: tbl.without_rowid,
        trailing_pk_cols,
    })
}

pub fn resolve_column_affinity(
    tbl_name: &str,
    col_name: &str,
    type_name: &str,
    strict: bool,
) -> Result<TypeAffinity> {
    if !strict {
        return Ok(TypeAffinity::from_declared_type(type_name));
    }
    let trimmed = type_name.trim();
    if trimmed.is_empty() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("missing datatype for {tbl_name}.{col_name}"),
        ));
    }
    match trimmed.to_ascii_uppercase().as_str() {
        "INT" | "INTEGER" => Ok(TypeAffinity::Integer),
        "REAL" => Ok(TypeAffinity::Real),
        "TEXT" => Ok(TypeAffinity::Text),
        "BLOB" | "ANY" => Ok(TypeAffinity::Blob),
        _ => Err(SqliteError::new(
            ErrorCode::Error,
            format!("unknown datatype for {tbl_name}.{col_name}: \"{trimmed}\""),
        )),
    }
}

pub fn build_column_schema_from_def(
    tbl_name: &str,
    strict: bool,
    cdef: &ColumnDef,
) -> Result<ColumnSchema> {
    let affinity = resolve_column_affinity(tbl_name, &cdef.name, &cdef.type_name, strict)?;
    let mut collation = Collation::Binary;
    let mut not_null = None;
    let mut default_expr = None;
    let mut generated = None;
    for cons in &cdef.constraints {
        match cons {
            ColumnConstraint::PrimaryKey { .. } => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "Cannot add a PRIMARY KEY column",
                ));
            }
            ColumnConstraint::Unique { .. } => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "Cannot add a UNIQUE column",
                ));
            }
            ColumnConstraint::NotNull { conflict } => {
                not_null = Some(*conflict);
            }
            ColumnConstraint::Default(e) => {
                default_expr = Some(e.clone());
            }
            ColumnConstraint::Generated { expr, stored } => {
                if *stored {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "cannot add a STORED column",
                    ));
                }
                generated = Some((expr.clone(), false));
            }
            ColumnConstraint::Collate(cname) => {
                collation = Collation::from_name(cname).ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        format!("no such collation sequence: {cname}"),
                    )
                })?;
            }
            _ => {}
        }
    }
    if not_null.is_some() && default_expr.is_none() && generated.is_none() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "Cannot add a NOT NULL column with default value NULL",
        ));
    }
    let mut is_unique = false;
    for cons in &cdef.constraints {
        if matches!(cons, ColumnConstraint::Unique { .. }) {
            is_unique = true;
        }
    }
    Ok(ColumnSchema {
        name: cdef.name.clone(),
        type_name: cdef.type_name.clone(),
        affinity,
        collation,
        not_null,
        default_expr,
        generated,
        is_primary_key: false,
        is_unique,
        pk_order: 0,
    })
}

fn resolve_indexed_columns(
    table_cols: &[ColumnSchema],
    idx_cols: &[IndexedColumn],
    allow_expr: bool,
) -> Result<Vec<IndexedColSchema>> {
    let mut out = Vec::with_capacity(idx_cols.len());
    for ic in idx_cols {
        if let Some(ref e) = ic.expr {
            if !allow_expr {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "expressions prohibited in PRIMARY KEY and UNIQUE constraints",
                ));
            }
            let collation = match &ic.collation {
                Some(cname) => Collation::from_name(cname).ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        format!("no such collation sequence: {cname}"),
                    )
                })?,
                None => Collation::Binary,
            };
            let desc = ic.desc;
            let nulls_last = ic.nulls_last.unwrap_or(desc);
            out.push(IndexedColSchema {
                col_idx: None,
                expr: Some(e.clone()),
                name: crate::compiler::expr_column_label(e),
                sort_order: KeySortOrder {
                    desc,
                    nulls_last,
                    collation,
                },
            });
            continue;
        }
        let Some((col_idx, col_schema)) = table_cols
            .iter()
            .enumerate()
            .find(|(_, c)| c.name.eq_ignore_ascii_case(&ic.name))
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {}", ic.name),
            ));
        };
        let collation = match &ic.collation {
            Some(cname) => Collation::from_name(cname).ok_or_else(|| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!("no such collation sequence: {cname}"),
                )
            })?,
            None => col_schema.collation,
        };
        let desc = ic.desc;
        let nulls_last = ic.nulls_last.unwrap_or(desc);
        out.push(IndexedColSchema {
            col_idx: Some(col_idx),
            expr: None,
            name: col_schema.name.clone(),
            sort_order: KeySortOrder {
                desc,
                nulls_last,
                collation,
            },
        });
    }
    Ok(out)
}

// ============================================================================
// Mutating `sqlite_schema` on Page 1 & Bumping `schema_cookie`
// ============================================================================

pub fn insert_schema_entry(
    btree: &mut Btree,
    obj_type: &str,
    name: &str,
    tbl_name: &str,
    rootpage: Pgno,
    sql: Option<&str>,
) -> Result<i64> {
    let enc = btree.encoding();
    let rowid = btree.next_rowid(1)?;
    let rec = encode_record_with_encoding(
        &[
            Value::Text(obj_type.to_owned()),
            Value::Text(name.to_owned()),
            Value::Text(tbl_name.to_owned()),
            Value::Integer(i64::from(rootpage)),
            sql.map_or(Value::Null, |s| Value::Text(s.to_owned())),
        ],
        enc,
    );
    btree.insert_table(1, rowid, &rec)?;
    bump_schema_cookie(btree)?;
    Ok(rowid)
}

pub fn update_schema_entry(
    btree: &mut Btree,
    schema_rowid: i64,
    obj_type: &str,
    name: &str,
    tbl_name: &str,
    rootpage: Pgno,
    sql: Option<&str>,
) -> Result<()> {
    let enc = btree.encoding();
    let rec = encode_record_with_encoding(
        &[
            Value::Text(obj_type.to_owned()),
            Value::Text(name.to_owned()),
            Value::Text(tbl_name.to_owned()),
            Value::Integer(i64::from(rootpage)),
            sql.map_or(Value::Null, |s| Value::Text(s.to_owned())),
        ],
        enc,
    );
    btree.insert_table(1, schema_rowid, &rec)?;
    bump_schema_cookie(btree)
}

pub fn delete_schema_entry(btree: &mut Btree, schema_rowid: i64) -> Result<()> {
    btree.delete_table(1, schema_rowid)?;
    bump_schema_cookie(btree)
}

pub fn bump_schema_cookie(btree: &mut Btree) -> Result<()> {
    let hdr = btree.pager_mut().header_mut()?;
    hdr.schema_cookie = hdr.schema_cookie.wrapping_add(1);
    if hdr.schema_format == 0 {
        hdr.schema_format = 4;
    }
    Ok(())
}

/// Scans all `(rowid, payload)` entries of a Table B+Tree using an incremental `BtreeCursor`,
/// checking the connection interrupt flag on every row and closing the cursor before returning.
pub(crate) fn scan_table_incremental(
    btree: &mut Btree,
    root_pgno: Pgno,
) -> Result<Vec<(i64, Vec<u8>)>> {
    if root_pgno == 0 {
        return Ok(Vec::new());
    }
    let cid = btree.open_table_cursor(root_pgno, false);
    let res = (|| {
        let mut out = Vec::new();
        if !btree.cursor_first(cid)? {
            return Ok(out);
        }
        loop {
            let rowid = btree.cursor_rowid(cid)?;
            let payload = btree.cursor_payload(cid)?;
            out.push((rowid, payload));
            if !btree.cursor_next(cid)? {
                break;
            }
        }
        Ok(out)
    })();
    btree.close_cursor(cid);
    res
}

/// Scans all `(rowid, payload)` entries of a Table B+Tree while driving `services` interrupt and
/// progress ticks on every row.
pub(crate) fn scan_table_incremental_with_services(
    btree: &mut Btree,
    root_pgno: Pgno,
    services: &mut dyn ExecutionServices,
) -> Result<Vec<(i64, Vec<u8>)>> {
    services.check_interrupted()?;
    services.tick_progress()?;
    if root_pgno == 0 {
        return Ok(Vec::new());
    }
    let cid = btree.open_table_cursor(root_pgno, false);
    let res = (|| {
        let mut out = Vec::new();
        if !btree.cursor_first(cid)? {
            return Ok(out);
        }
        loop {
            services.check_interrupted()?;
            services.tick_progress()?;
            let rowid = btree.cursor_rowid(cid)?;
            let payload = btree.cursor_payload(cid)?;
            out.push((rowid, payload));
            if !btree.cursor_next(cid)? {
                break;
            }
        }
        Ok(out)
    })();
    btree.close_cursor(cid);
    res
}

/// Scans all key payloads of an Index B-Tree using an incremental `BtreeCursor`,
/// checking the connection interrupt flag on every entry and closing the cursor before returning.
pub(crate) fn scan_index_incremental(
    btree: &mut Btree,
    root_pgno: Pgno,
    orders: Vec<KeySortOrder>,
) -> Result<Vec<Vec<u8>>> {
    if root_pgno == 0 {
        return Ok(Vec::new());
    }
    let cid = btree.open_index_cursor(root_pgno, orders, false);
    let res = (|| {
        let mut out = Vec::new();
        if !btree.cursor_first(cid)? {
            return Ok(out);
        }
        loop {
            let payload = btree.cursor_payload(cid)?;
            out.push(payload);
            if !btree.cursor_next(cid)? {
                break;
            }
        }
        Ok(out)
    })();
    btree.close_cursor(cid);
    res
}

/// Scans all key payloads of an Index B-Tree while driving `services` interrupt and progress
/// ticks on every entry.
pub(crate) fn scan_index_incremental_with_services(
    btree: &mut Btree,
    root_pgno: Pgno,
    orders: Vec<KeySortOrder>,
    services: &mut dyn ExecutionServices,
) -> Result<Vec<Vec<u8>>> {
    services.check_interrupted()?;
    services.tick_progress()?;
    if root_pgno == 0 {
        return Ok(Vec::new());
    }
    let cid = btree.open_index_cursor(root_pgno, orders, false);
    let res = (|| {
        let mut out = Vec::new();
        if !btree.cursor_first(cid)? {
            return Ok(out);
        }
        loop {
            services.check_interrupted()?;
            services.tick_progress()?;
            let payload = btree.cursor_payload(cid)?;
            out.push(payload);
            if !btree.cursor_next(cid)? {
                break;
            }
        }
        Ok(out)
    })();
    btree.close_cursor(cid);
    res
}

// ============================================================================
// SchemaFixer (`sqlite3FixInit` / `sqlite3FixSelect` in `sqlite/src/attach.c`)
// ============================================================================

/// Rejects any bind parameter (`?`, `?NNN`, `:name`, `@name`, `$name`) inside a `VIEW`'s `SELECT`
/// AST (`"parameters are not allowed in views"`, `sqlite/src/build.c:3089`).
pub fn validate_view_no_parameters(select: &SelectStmt) -> Result<()> {
    for cte in &select.ctes {
        validate_view_no_parameters(&cte.query)?;
    }
    validate_core_no_params(&select.body.head)?;
    for (_, core) in &select.body.compounds {
        validate_core_no_params(core)?;
    }
    for ob in &select.order_by {
        validate_expr_no_params(&ob.expr)?;
    }
    if let Some(ref lim) = select.limit {
        validate_expr_no_params(&lim.limit)?;
        if let Some(ref off) = lim.offset {
            validate_expr_no_params(off)?;
        }
    }
    Ok(())
}

fn validate_core_no_params(core: &SelectCore) -> Result<()> {
    match core {
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    validate_expr_no_params(expr)?;
                }
            }
            if let Some(f) = from {
                validate_tos_no_params(&f.head)?;
                for j in &f.joins {
                    validate_tos_no_params(&j.right)?;
                    if let Some(JoinConstraint::On(ref on_expr)) = j.constraint {
                        validate_expr_no_params(on_expr)?;
                    }
                }
            }
            if let Some(w) = selection {
                validate_expr_no_params(w)?;
            }
            for g in group_by {
                validate_expr_no_params(g)?;
            }
            if let Some(h) = having {
                validate_expr_no_params(h)?;
            }
        }
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    validate_expr_no_params(e)?;
                }
            }
        }
    }
    Ok(())
}

fn validate_tos_no_params(tos: &TableOrSubquery) -> Result<()> {
    if let TableOrSubquery::Subquery { query, .. } = tos {
        validate_view_no_parameters(query)?;
    }
    Ok(())
}

fn validate_expr_no_params(expr: &Expr) -> Result<()> {
    match expr {
        Expr::Bind(_) => Err(SqliteError::new(
            ErrorCode::Error,
            "parameters are not allowed in views",
        )),
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Column(_) => Ok(()),
        Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => validate_expr_no_params(inner),
        Expr::Binary { left, right, .. } => {
            validate_expr_no_params(left)?;
            validate_expr_no_params(right)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            validate_expr_no_params(e)?;
            validate_expr_no_params(low)?;
            validate_expr_no_params(high)
        }
        Expr::InList { expr: e, list, .. } => {
            validate_expr_no_params(e)?;
            for item in list {
                validate_expr_no_params(item)?;
            }
            Ok(())
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            validate_expr_no_params(e)?;
            validate_view_no_parameters(subquery)
        }
        Expr::InTable { expr: e, .. } => validate_expr_no_params(e),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            validate_expr_no_params(e)?;
            validate_expr_no_params(pattern)?;
            if let Some(esc) = escape {
                validate_expr_no_params(esc)?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                validate_expr_no_params(op)?;
            }
            for (c, v) in branches {
                validate_expr_no_params(c)?;
                validate_expr_no_params(v)?;
            }
            if let Some(el) = else_expr {
                validate_expr_no_params(el)?;
            }
            Ok(())
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    validate_expr_no_params(e)?;
                }
            }
            if let Some(f) = filter {
                validate_expr_no_params(f)?;
            }
            Ok(())
        }
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            validate_view_no_parameters(subquery)
        }
        Expr::RowValue(es) => {
            for e in es {
                validate_expr_no_params(e)?;
            }
            Ok(())
        }
        Expr::Raise { message, .. } => {
            if let Some(m) = message {
                validate_expr_no_params(m)?;
            }
            Ok(())
        }
    }
}

/// Applies `SchemaFixer` (`sqlite3FixInit` / `sqlite3FixSelect`) to a permanent view's `SelectStmt`:
/// - Rejects any explicit schema qualification (`schema.table`, `schema.table.col`, `schema.table.*`,
///   `IN schema.table`) whose schema does not case-insensitively match `db_schema_name`
///   (`"view <view_name> cannot reference objects in database <other_schema>"`).
/// - Pins unqualified `FROM` / `JOIN` / `IN table` object references that do NOT match a locally
///   in-scope CTE name strictly to `db_schema_name`, preventing `TEMP` tables or views from ever
///   shadowing objects referenced inside a permanent view.
pub fn fix_permanent_view_select(
    view_name: &str,
    db_schema_name: &str,
    select: &SelectStmt,
) -> Result<SelectStmt> {
    fix_select_with_ctes(view_name, db_schema_name, select, &[])
}

fn check_allowed_schema(view_name: &str, db_schema_name: &str, explicit: &str) -> Result<()> {
    if !explicit.eq_ignore_ascii_case(db_schema_name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("view {view_name} cannot reference objects in database {explicit}"),
        ));
    }
    Ok(())
}

fn fix_qualified_relation(
    view_name: &str,
    db_schema_name: &str,
    qn: &QualifiedName,
    in_scope_ctes: &[String],
) -> Result<QualifiedName> {
    if let Some(ref s) = qn.schema {
        check_allowed_schema(view_name, db_schema_name, s)?;
        Ok(QualifiedName::qualified(
            db_schema_name.to_owned(),
            qn.name.clone(),
        ))
    } else if in_scope_ctes
        .iter()
        .any(|cte_name| cte_name.eq_ignore_ascii_case(&qn.name))
    {
        Ok(qn.clone())
    } else {
        Ok(QualifiedName::qualified(
            db_schema_name.to_owned(),
            qn.name.clone(),
        ))
    }
}

fn fix_select_with_ctes(
    view_name: &str,
    db_schema_name: &str,
    select: &SelectStmt,
    outer_ctes: &[String],
) -> Result<SelectStmt> {
    let mut full_scope: Vec<String> = outer_ctes.to_vec();
    for cte in &select.ctes {
        full_scope.push(cte.name.clone());
    }

    let mut fixed_ctes = Vec::with_capacity(select.ctes.len());
    for cte in &select.ctes {
        let mut sc: Vec<String> = outer_ctes.to_vec();
        for sibling in &select.ctes {
            sc.push(sibling.name.clone());
        }
        fixed_ctes.push(CommonTableExpr {
            name: cte.name.clone(),
            columns: cte.columns.clone(),
            materialized: cte.materialized,
            query: fix_select_with_ctes(view_name, db_schema_name, &cte.query, &sc)?,
        });
    }

    let head = fix_core_with_ctes(view_name, db_schema_name, &select.body.head, &full_scope)?;
    let mut compounds = Vec::with_capacity(select.body.compounds.len());
    for (op, core) in &select.body.compounds {
        compounds.push((
            *op,
            fix_core_with_ctes(view_name, db_schema_name, core, &full_scope)?,
        ));
    }
    let mut order_by = Vec::with_capacity(select.order_by.len());
    for ob in &select.order_by {
        order_by.push(OrderByTerm {
            expr: fix_expr_with_ctes(view_name, db_schema_name, &ob.expr, &full_scope)?,
            desc: ob.desc,
            nulls_last: ob.nulls_last,
        });
    }
    let limit = match &select.limit {
        Some(lim) => Some(LimitClause {
            limit: fix_expr_with_ctes(view_name, db_schema_name, &lim.limit, &full_scope)?,
            offset: match &lim.offset {
                Some(off) => Some(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    off,
                    &full_scope,
                )?),
                None => None,
            },
        }),
        None => None,
    };

    Ok(SelectStmt {
        ctes: fixed_ctes,
        body: SelectBody { head, compounds },
        order_by,
        limit,
    })
}

fn fix_core_with_ctes(
    view_name: &str,
    db_schema_name: &str,
    core: &SelectCore,
    in_scope_ctes: &[String],
) -> Result<SelectCore> {
    match core {
        SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
        } => {
            let mut fixed_proj = Vec::with_capacity(projection.len());
            for rc in projection {
                match rc {
                    ResultColumn::Star => fixed_proj.push(ResultColumn::Star),
                    ResultColumn::TableStar { schema, table } => {
                        if let Some(s) = schema {
                            check_allowed_schema(view_name, db_schema_name, s)?;
                        }
                        fixed_proj.push(ResultColumn::TableStar {
                            schema: schema.clone(),
                            table: table.clone(),
                        });
                    }
                    ResultColumn::Expr { expr, alias } => {
                        fixed_proj.push(ResultColumn::Expr {
                            expr: fix_expr_with_ctes(
                                view_name,
                                db_schema_name,
                                expr,
                                in_scope_ctes,
                            )?,
                            alias: alias.clone(),
                        });
                    }
                }
            }
            let fixed_from = match from {
                Some(f) => {
                    let head =
                        fix_tos_with_ctes(view_name, db_schema_name, &f.head, in_scope_ctes)?;
                    let mut joins = Vec::with_capacity(f.joins.len());
                    for j in &f.joins {
                        let right =
                            fix_tos_with_ctes(view_name, db_schema_name, &j.right, in_scope_ctes)?;
                        let constraint = match &j.constraint {
                            Some(JoinConstraint::On(e)) => Some(JoinConstraint::On(
                                fix_expr_with_ctes(view_name, db_schema_name, e, in_scope_ctes)?,
                            )),
                            Some(JoinConstraint::Using(cols)) => {
                                Some(JoinConstraint::Using(cols.clone()))
                            }
                            None => None,
                        };
                        joins.push(JoinClause {
                            join_type: j.join_type,
                            natural: j.natural,
                            right,
                            constraint,
                        });
                    }
                    Some(FromClause { head, joins })
                }
                None => None,
            };
            let fixed_sel = match selection {
                Some(w) => Some(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    w,
                    in_scope_ctes,
                )?),
                None => None,
            };
            let mut fixed_gb = Vec::with_capacity(group_by.len());
            for g in group_by {
                fixed_gb.push(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    g,
                    in_scope_ctes,
                )?);
            }
            let fixed_hav = match having {
                Some(h) => Some(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    h,
                    in_scope_ctes,
                )?),
                None => None,
            };
            Ok(SelectCore::Select {
                distinct: *distinct,
                projection: fixed_proj,
                from: fixed_from,
                selection: fixed_sel,
                group_by: fixed_gb,
                having: fixed_hav,
            })
        }
        SelectCore::Values(rows) => {
            let mut out = Vec::with_capacity(rows.len());
            for r in rows {
                let mut row_out = Vec::with_capacity(r.len());
                for e in r {
                    row_out.push(fix_expr_with_ctes(
                        view_name,
                        db_schema_name,
                        e,
                        in_scope_ctes,
                    )?);
                }
                out.push(row_out);
            }
            Ok(SelectCore::Values(out))
        }
    }
}

fn fix_tos_with_ctes(
    view_name: &str,
    db_schema_name: &str,
    tos: &TableOrSubquery,
    in_scope_ctes: &[String],
) -> Result<TableOrSubquery> {
    match tos {
        TableOrSubquery::Table {
            name,
            alias,
            indexed_by,
        } => Ok(TableOrSubquery::Table {
            name: fix_qualified_relation(view_name, db_schema_name, name, in_scope_ctes)?,
            alias: alias.clone(),
            indexed_by: indexed_by.clone(),
        }),
        TableOrSubquery::Subquery { query, alias } => Ok(TableOrSubquery::Subquery {
            query: Box::new(fix_select_with_ctes(
                view_name,
                db_schema_name,
                query,
                in_scope_ctes,
            )?),
            alias: alias.clone(),
        }),
    }
}

fn fix_expr_with_ctes(
    view_name: &str,
    db_schema_name: &str,
    expr: &Expr,
    in_scope_ctes: &[String],
) -> Result<Expr> {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => Ok(expr.clone()),
        Expr::Column(ColumnRef {
            schema,
            table,
            column,
        }) => {
            if let Some(s) = schema {
                check_allowed_schema(view_name, db_schema_name, s)?;
            }
            Ok(Expr::Column(ColumnRef {
                schema: schema.clone(),
                table: table.clone(),
                column: column.clone(),
            }))
        }
        Expr::Unary { op, expr: inner } => Ok(Expr::Unary {
            op: *op,
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
        }),
        Expr::Binary { left, op, right } => Ok(Expr::Binary {
            left: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                left,
                in_scope_ctes,
            )?),
            op: *op,
            right: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                right,
                in_scope_ctes,
            )?),
        }),
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
        } => Ok(Expr::Between {
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            low: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                low,
                in_scope_ctes,
            )?),
            high: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                high,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::InList {
            expr: e,
            list,
            negated,
        } => {
            let mut fixed_list = Vec::with_capacity(list.len());
            for item in list {
                fixed_list.push(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    item,
                    in_scope_ctes,
                )?);
            }
            Ok(Expr::InList {
                expr: Box::new(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    e,
                    in_scope_ctes,
                )?),
                list: fixed_list,
                negated: *negated,
            })
        }
        Expr::InSubquery {
            expr: e,
            subquery,
            negated,
        } => Ok(Expr::InSubquery {
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            subquery: Box::new(fix_select_with_ctes(
                view_name,
                db_schema_name,
                subquery,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::InTable {
            expr: e,
            table,
            negated,
        } => Ok(Expr::InTable {
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            table: fix_qualified_relation(view_name, db_schema_name, table, in_scope_ctes)?,
            negated: *negated,
        }),
        Expr::Like {
            expr: e,
            pattern,
            escape,
            op,
            negated,
        } => Ok(Expr::Like {
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                e,
                in_scope_ctes,
            )?),
            pattern: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                pattern,
                in_scope_ctes,
            )?),
            escape: match escape {
                Some(esc) => Some(Box::new(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    esc,
                    in_scope_ctes,
                )?)),
                None => None,
            },
            op: *op,
            negated: *negated,
        }),
        Expr::IsNull {
            expr: inner,
            negated,
        } => Ok(Expr::IsNull {
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::Collate {
            expr: inner,
            collation,
        } => Ok(Expr::Collate {
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
            collation: collation.clone(),
        }),
        Expr::Cast {
            expr: inner,
            type_name,
        } => Ok(Expr::Cast {
            expr: Box::new(fix_expr_with_ctes(
                view_name,
                db_schema_name,
                inner,
                in_scope_ctes,
            )?),
            type_name: type_name.clone(),
        }),
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            let fixed_op = match operand {
                Some(op) => Some(Box::new(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    op,
                    in_scope_ctes,
                )?)),
                None => None,
            };
            let mut fixed_branches = Vec::with_capacity(branches.len());
            for (c, v) in branches {
                fixed_branches.push((
                    fix_expr_with_ctes(view_name, db_schema_name, c, in_scope_ctes)?,
                    fix_expr_with_ctes(view_name, db_schema_name, v, in_scope_ctes)?,
                ));
            }
            let fixed_else = match else_expr {
                Some(el) => Some(Box::new(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    el,
                    in_scope_ctes,
                )?)),
                None => None,
            };
            Ok(Expr::Case {
                operand: fixed_op,
                branches: fixed_branches,
                else_expr: fixed_else,
            })
        }
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            let fixed_args = match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => {
                    let mut out = Vec::with_capacity(es.len());
                    for e in es {
                        out.push(fix_expr_with_ctes(
                            view_name,
                            db_schema_name,
                            e,
                            in_scope_ctes,
                        )?);
                    }
                    FunctionArgs::Exprs(out)
                }
            };
            let fixed_filter = match filter {
                Some(f) => Some(Box::new(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    f,
                    in_scope_ctes,
                )?)),
                None => None,
            };
            Ok(Expr::FunctionCall {
                name: name.clone(),
                distinct: *distinct,
                args: fixed_args,
                filter: fixed_filter,
            })
        }
        Expr::Exists { subquery, negated } => Ok(Expr::Exists {
            subquery: Box::new(fix_select_with_ctes(
                view_name,
                db_schema_name,
                subquery,
                in_scope_ctes,
            )?),
            negated: *negated,
        }),
        Expr::Subquery(subquery) => Ok(Expr::Subquery(Box::new(fix_select_with_ctes(
            view_name,
            db_schema_name,
            subquery,
            in_scope_ctes,
        )?))),
        Expr::RowValue(es) => {
            let mut out = Vec::with_capacity(es.len());
            for e in es {
                out.push(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    e,
                    in_scope_ctes,
                )?);
            }
            Ok(Expr::RowValue(out))
        }
        Expr::Raise { action, message } => Ok(Expr::Raise {
            action: *action,
            message: match message {
                Some(m) => Some(Box::new(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    m,
                    in_scope_ctes,
                )?)),
                None => None,
            },
        }),
    }
}
