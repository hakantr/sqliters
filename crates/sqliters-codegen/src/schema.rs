//! The schema adapter: C's `Table` / `Column` / `Index` objects built from the declarations
//! the engine hands over ([`TableDef`]), with the logic of `build.c` that shapes them
//! (`sqlite3AddColumn`, `sqlite3AddPrimaryKey`, `sqlite3CreateIndex`,
//! `convertToWithoutRowidTable`, `sqlite3EndTable`, `sqlite3DefaultRowEst`,
//! `estimateTableWidth` / `estimateIndexWidth`, `recomputeColumnsNotIndexed`) and the lookup
//! functions of `build.c` (`sqlite3FindTable`, `sqlite3LocateTable`, `sqlite3LocateTableItem`,
//! `sqlite3PrimaryKeyIndex`, `sqlite3TableColumnToStorage`, `sqlite3KeyInfoOfIndex`, …).
//!
//! C builds these objects once, when the schema is read (`sqlite3InitOne`), by running its
//! parser over the `CREATE` statements of `sqlite_schema` and then loading `sqlite_stat1`
//! (`sqlite3AnalysisLoad`). The engine has its own parsed schema; one adapter in the engine
//! (`sqliters-core/src/cgen.rs`) turns it into [`TableDef`]s, and this module derives
//! everything else exactly as `build.c` does, so that the query planner sees C's numbers.

use std::sync::Arc;

use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{KeyInfo, P4, P4Table};

use crate::callback::*;
use crate::consts::*;
use crate::select::sqlite3_log_est;
use crate::sqliteint::*;

/// A column as declared in `CREATE TABLE`.
#[derive(Debug, Clone, Default)]
pub struct ColumnDef {
    /// Column name (dequoted)
    pub name: String,
    /// The declared type exactly as written (dequoted), `None` if the column has no type
    pub z_type: Option<String>,
    /// `Column.notNull` as declared by a NOT NULL constraint: an `OE_*` code, 0 if none
    pub not_null: u8,
    /// `COLFLAG_PRIMKEY`, `COLFLAG_UNIQUE`, `COLFLAG_HIDDEN`, `COLFLAG_VIRTUAL`, `COLFLAG_STORED`
    pub col_flags: u16,
    /// The name of the COLLATE clause of the column, if any
    pub z_coll: Option<String>,
    /// The column has a DEFAULT clause (or is a generated column): `iDflt!=0`
    pub has_dflt: bool,
    /// `sqlite3ValueFromExpr()` of the DEFAULT expression with the column affinity; `None`
    /// when the expression is not a constant
    pub dflt_value: Option<sqliters_types::Value>,
}

/// One key column of an index declaration.
#[derive(Debug, Clone, Default)]
pub struct IndexColDef {
    /// Table column number, or `XN_EXPR` (-2) for an expression
    pub i_column: i32,
    /// The collation named by an explicit COLLATE in the index declaration
    pub z_coll: Option<String>,
    /// DESC
    pub desc: bool,
}

/// `sqlite_stat1` data of an index as `decodeIntArray()` produced it.
#[derive(Debug, Clone, Default)]
pub struct IndexStatDef {
    /// `aiRowLogEst[0..=nKeyCol]`
    pub ai_row_log_est: Vec<LogEst>,
    pub b_unordered: bool,
    pub no_skip_scan: bool,
    /// `szIdxRow` from an `sz=N` argument
    pub sz_idx_row: Option<LogEst>,
}

/// An index as declared (`CREATE INDEX`, or a UNIQUE / PRIMARY KEY constraint).
#[derive(Debug, Clone, Default)]
pub struct IndexDef {
    pub name: String,
    /// The declared key columns
    pub cols: Vec<IndexColDef>,
    /// `onError`: `OE_None` for a non-unique index
    pub on_error: u8,
    /// `SQLITE_IDXTYPE_APPDEF`, `_UNIQUE` or `_PRIMARYKEY`
    pub idx_type: u8,
    /// Root page
    pub tnum: u32,
    /// The index has a WHERE clause
    pub is_partial: bool,
    /// Text of the CREATE INDEX statement (needed for expression columns and WHERE)
    pub z_sql: Option<String>,
    /// `sqlite_stat1` data, if the index has any
    pub stat: Option<IndexStatDef>,
}

/// A table or view as declared.
#[derive(Debug, Clone, Default)]
pub struct TableDef {
    pub name: String,
    pub cols: Vec<ColumnDef>,
    /// `TABTYP_NORM`, `TABTYP_VTAB` or `TABTYP_VIEW`
    pub e_tab_type: u8,
    /// Root page
    pub tnum: u32,
    /// Column that is the INTEGER PRIMARY KEY (rowid alias), -1 if none
    pub i_p_key: i32,
    /// Conflict resolution of the INTEGER PRIMARY KEY
    pub key_conf: u8,
    pub without_rowid: bool,
    pub strict: bool,
    pub autoincrement: bool,
    pub has_primary_key: bool,
    /// Indexes in creation order: the automatic indexes of the CREATE TABLE statement in
    /// the order of their constraints, then the CREATE INDEX statements in the order of
    /// `sqlite_schema`
    pub indexes: Vec<IndexDef>,
    /// `nRowLogEst` from `sqlite_stat1` (200 without)
    pub n_row_log_est: LogEst,
    /// `szTabRow` from an `sz=N` argument of the table's own `sqlite_stat1` row
    pub sz_tab_row: Option<LogEst>,
    /// `TF_HasStat1`
    pub has_stat1: bool,
    /// Text of the CREATE VIEW statement of a view
    pub z_sql: Option<String>,
    /// The table has triggers
    pub has_trigger: bool,
    /// Set by the engine's adapter when its schema objects are known to differ from C's for
    /// this table: statements that use the table are reported as unsupported.
    pub unsupported: Option<&'static str>,
}

/// `sqlite3AffinityType(zIn, pCol)`: the column affinity of a declared type and the size
/// estimate `Column.szEst` (scaled so that the size of an integer is 1).
pub fn sqlite3_affinity_type_sz(z_in: &str) -> (u8, u8) {
    let b = z_in.as_bytes();
    let mut h: u32 = 0;
    let mut aff = SQLITE_AFF_NUMERIC;
    let mut z_char: Option<usize> = None;
    let mut i = 0;
    while i < b.len() {
        let x = b[i];
        h = (h << 8).wrapping_add(x.to_ascii_lowercase() as u32);
        i += 1;
        if h == u32::from_be_bytes(*b"char") {
            aff = SQLITE_AFF_TEXT;
            z_char = Some(i);
        } else if h == u32::from_be_bytes(*b"clob") {
            aff = SQLITE_AFF_TEXT;
        } else if h == u32::from_be_bytes(*b"text") {
            aff = SQLITE_AFF_TEXT;
        } else if h == u32::from_be_bytes(*b"blob") && (aff == SQLITE_AFF_NUMERIC || aff == SQLITE_AFF_REAL) {
            aff = SQLITE_AFF_BLOB;
            if i < b.len() && b[i] == b'(' {
                z_char = Some(i);
            }
        } else if h == u32::from_be_bytes(*b"real") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if h == u32::from_be_bytes(*b"floa") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if h == u32::from_be_bytes(*b"doub") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if (h & 0x00FF_FFFF) == ((b'i' as u32) << 16) + ((b'n' as u32) << 8) + b't' as u32 {
            aff = SQLITE_AFF_INTEGER;
            break;
        }
    }
    // If pCol is not NULL, store an estimate of the field size. The estimate is scaled so
    // that the size of an integer is 1.
    let mut v: i32 = 0; // default size is approx 4 bytes
    if aff < SQLITE_AFF_NUMERIC {
        if let Some(mut k) = z_char {
            while k < b.len() {
                if b[k].is_ascii_digit() {
                    // BLOB(k), VARCHAR(k), CHAR(k) -> r=(k/4+1)
                    let s = std::str::from_utf8(&b[k..]).unwrap_or("");
                    v = crate::util::sqlite3_get_int32(s).unwrap_or(0);
                    break;
                }
                k += 1;
            }
        } else {
            v = 16; // BLOB, TEXT, CLOB -> r=5  (approx 20 bytes)
        }
    }
    v = v / 4 + 1;
    if v > 255 {
        v = 255;
    }
    (aff, v as u8)
}

/// `sqlite3StdType[]` / `sqlite3StdTypeAffinity[]`
const STD_TYPE: [(&str, u8); 6] = [
    ("ANY", SQLITE_AFF_NUMERIC),
    ("BLOB", SQLITE_AFF_BLOB),
    ("INT", SQLITE_AFF_INTEGER),
    ("INTEGER", SQLITE_AFF_INTEGER),
    ("REAL", SQLITE_AFF_REAL),
    ("TEXT", SQLITE_AFF_TEXT),
];

/// The type handling of `sqlite3AddColumn()`: the column object for a declared name and type.
fn add_column(def: &ColumnDef) -> Column {
    let mut col = Column { z_cn_name: def.name.clone(), ..Column::default() };
    let mut e_type = COLTYPE_CUSTOM as u8;
    let mut sz_est: u8 = 1;
    let mut affinity = SQLITE_AFF_BLOB;
    let mut s_type: Option<&str> = def.z_type.as_deref().filter(|t| !t.is_empty());
    // Check for standard typenames. For standard typenames we will set the Column.eType
    // field rather than storing the typename after the column name, in order to save space.
    if let Some(t) = s_type
        && t.len() >= 3
    {
        for (i, (name, aff)) in STD_TYPE.iter().enumerate() {
            if t.eq_ignore_ascii_case(name) {
                s_type = None;
                e_type = i as u8 + 1;
                affinity = *aff;
                if affinity <= SQLITE_AFF_TEXT {
                    sz_est = 5;
                }
                break;
            }
        }
    }
    match s_type {
        None => {
            // If there is no type specified, columns have the default affinity 'BLOB' with a
            // default size of 4 bytes.
            col.affinity = affinity;
            col.e_c_type = e_type;
            col.sz_est = sz_est;
        }
        Some(t) => {
            let (aff, sz) = sqlite3_affinity_type_sz(t);
            col.affinity = aff;
            col.sz_est = sz;
            col.z_type = Some(t.to_owned());
            col.col_flags |= COLFLAG_HASTYPE as u16;
        }
    }
    col.not_null = def.not_null;
    col.col_flags |= def.col_flags & (COLFLAG_PRIMKEY | COLFLAG_UNIQUE | COLFLAG_HIDDEN | COLFLAG_VIRTUAL | COLFLAG_STORED) as u16;
    if let Some(c) = &def.z_coll {
        col.z_coll = Some(c.clone());
        col.col_flags |= COLFLAG_HASCOLL as u16;
    }
    col.i_dflt = def.has_dflt as u16;
    col.dflt_value = def.dflt_value.clone();
    col
}

/// `sqlite3ColumnColl(pCol)`: the collating sequence name for a column, `None` for the
/// default.
pub fn sqlite3_column_coll(col: &Column) -> Option<&str> {
    if (col.col_flags & COLFLAG_HASCOLL as u16) == 0 {
        return None;
    }
    col.z_coll.as_deref()
}

/// `sqlite3ColumnType(pCol, zDflt)`: the declared type of a column.
pub fn sqlite3_column_type<'a>(col: &'a Column, z_dflt: Option<&'a str>) -> Option<&'a str> {
    if (col.col_flags & COLFLAG_HASTYPE as u16) != 0 {
        col.z_type.as_deref()
    } else if col.e_c_type != 0 {
        Some(STD_TYPE[col.e_c_type as usize - 1].0)
    } else {
        z_dflt
    }
}

/// `sqlite3DefaultRowEst(pIdx)`: fill the Index.aiRowLogEst[] array with default information
/// - information to be used when we have not run the ANALYZE command.
///
/// aiRowLogEst[0] is supposed to contain the number of elements in the index. Since we do not
/// know, guess 1 million. aiRowLogEst[1] is an estimate of the number of rows returned by a
/// lookup on the first column of the index, and so on.
pub fn sqlite3_default_row_est(idx: &mut Index, tab_n_row_log_est: &mut LogEst) {
    const A_VAL: [LogEst; 5] = [33, 32, 30, 28, 26];
    let n_key_col = idx.n_key_col as usize;
    let n_copy = A_VAL.len().min(n_key_col);
    idx.ai_row_log_est = vec![0; n_key_col + 1];
    // Set the first entry (number of rows in the index) to the estimated number of rows in
    // the table, or half the number of rows in the table for a partial index.
    //
    // 2020-05-27: If some of the stat data is coming from the sqlite_stat1 table but other
    // parts we are having to guess at, then do not let the estimated number of rows in the
    // table be less than 1000 (LogEst 99).
    let mut x = *tab_n_row_log_est;
    if x < 99 {
        x = 99;
        *tab_n_row_log_est = x;
    }
    if idx.p_part_idx_where.nn() || idx.is_partial_decl() {
        x -= 10;
    }
    idx.ai_row_log_est[0] = x;
    // Estimate that a[1] is 10, a[2] is 9, a[3] is 8, a[4] is 7, a[5] is 6 and each
    // subsequent value (if any) is 5.
    idx.ai_row_log_est[1..=n_copy].copy_from_slice(&A_VAL[..n_copy]);
    for i in n_copy + 1..=n_key_col {
        idx.ai_row_log_est[i] = 23;
    }
    if idx.is_unique() {
        idx.ai_row_log_est[n_key_col] = 0;
    }
}

impl Index {
    /// `pPartIdxWhere!=0` for an index whose WHERE clause is not parsed yet.
    #[inline]
    pub fn is_partial_decl(&self) -> bool {
        self.b_partial
    }
}

/// `isDupColumn(pIdx, nKey, pPk, iCol)`: return true if any of the first nKey entries of
/// index pIdx exactly match the iCol-th entry of pPk. pPk is always a WITHOUT ROWID PRIMARY
/// KEY index and pIdx is an index on the same table.
fn is_dup_column(idx: &Index, n_key: usize, pk_ai_column: i16, pk_coll: &str) -> bool {
    for i in 0..n_key {
        if idx.ai_column[i] == pk_ai_column && idx.az_coll[i].eq_ignore_ascii_case(pk_coll) {
            return true;
        }
    }
    false
}

/// `recomputeColumnsNotIndexed(pIdx)`: recompute the colNotIdxed field of the Index.
///
/// colNotIdxed is a bitmask that has a 0 bit representing each indexed columns that are
/// within the first 63 columns of the table and a 1 for all other bits (all columns that are
/// not in the index). The high-order bit (bit 63) represents all columns with an index
/// greater than 62 on the table.
fn recompute_columns_not_indexed(idx: &mut Index, a_col: &[Column]) {
    let mut m: Bitmask = 0;
    for j in (0..idx.n_column as usize).rev() {
        let x = idx.ai_column[j] as i32;
        if x >= 0 && (a_col[x as usize].col_flags & COLFLAG_VIRTUAL as u16) == 0 && x < BMS - 1 {
            m |= 1u64 << x;
        }
    }
    idx.col_not_idxed = !m;
}

/// `estimateIndexWidth(pIdx)`: estimate the average size of a row for an index.
fn estimate_index_width(idx: &mut Index, a_col: &[Column]) {
    let mut w_index: u32 = 0;
    for i in 0..idx.n_column as usize {
        let x = idx.ai_column[i];
        w_index += if x < 0 { 1 } else { a_col[x as usize].sz_est as u32 };
    }
    idx.sz_idx_row = sqlite3_log_est(w_index as u64 * 4);
}

/// The part of `sqlite3CreateIndex()` that builds the Index object for a declaration.
/// `pk` is the PRIMARY KEY index of a WITHOUT ROWID table that is already converted
/// (`pPk`), `None` for a rowid table and for the indexes made while the CREATE TABLE
/// statement of a WITHOUT ROWID table is still being parsed.
fn create_index(tab: &Table, def: &IndexDef, pk: Option<&Index>, is_create_index_stmt: bool, file_format: u8) -> Index {
    let n_extra_col = pk.map_or(1, |p| p.n_key_col as usize);
    let n = def.cols.len();
    let mut idx = Index {
        z_name: def.name.clone(),
        on_error: def.on_error,
        uniq_not_null: def.on_error != OE_None as u8,
        idx_type: def.idx_type,
        n_key_col: n as u16,
        n_column: (n + n_extra_col) as u16,
        tnum: def.tnum,
        b_partial: def.is_partial,
        ..Index::default()
    };
    // Check to see if we should honor DESC requests on index columns
    let honor_desc = file_format >= 4;
    for c in &def.cols {
        let j = c.i_column;
        if j == XN_EXPR {
            idx.ai_column.push(XN_EXPR as i16);
            idx.uniq_not_null = false;
            idx.b_has_expr = true;
        } else {
            // lookupName() gives iColumn==-1 for the rowid and for the INTEGER PRIMARY KEY
            // column; a declaration handed over by the engine names the column itself.
            let mut j = if j >= 0 && j == tab.i_p_key as i32 { -1 } else { j };
            if j < 0 {
                j = tab.i_p_key as i32;
            } else {
                if tab.a_col[j as usize].not_null == 0 {
                    idx.uniq_not_null = false;
                }
                if tab.a_col[j as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
                    idx.b_has_v_col = true;
                    idx.b_has_expr = true;
                }
            }
            idx.ai_column.push(j as i16);
        }
        let z_coll: Option<String> = match &c.z_coll {
            Some(z) => Some(z.clone()),
            None => {
                let j = *idx.ai_column.last().expect("column");
                if j >= 0 { sqlite3_column_coll(&tab.a_col[j as usize]).map(|z| z.to_owned()) } else { None }
            }
        };
        // if( !zColl ) zColl = sqlite3StrBINARY;
        idx.a_coll_static.push(z_coll.is_none());
        idx.az_coll.push(z_coll.unwrap_or_else(|| "BINARY".to_owned()));
        idx.a_sort_order.push((c.desc && honor_desc) as u8);
    }
    // Append the table key to the end of the index. For WITHOUT ROWID tables (when pPk!=0)
    // this will be the declared PRIMARY KEY. For normal tables (when pPk==0) this will be the
    // rowid.
    if let Some(pk) = pk {
        for j in 0..pk.n_key_col as usize {
            let x = pk.ai_column[j];
            if is_dup_column(&idx, idx.n_key_col as usize, x, &pk.az_coll[j]) {
                idx.n_column -= 1;
            } else {
                idx.ai_column.push(x);
                idx.az_coll.push(pk.az_coll[j].clone());
                idx.a_coll_static.push(pk.a_coll_static[j]);
                idx.a_sort_order.push(pk.a_sort_order[j]);
            }
        }
    } else {
        idx.ai_column.push(XN_ROWID as i16);
        idx.az_coll.push("BINARY".to_owned());
        idx.a_coll_static.push(true);
        idx.a_sort_order.push(0);
    }
    if is_create_index_stmt {
        estimate_index_width(&mut idx, &tab.a_col);
    }
    // If this index contains every column of its table, then mark it as a covering index
    recompute_columns_not_indexed(&mut idx, &tab.a_col);
    if is_create_index_stmt && idx.n_column as usize >= tab.a_col.len() {
        idx.is_covering = true;
        for j in 0..tab.a_col.len() {
            if j as i32 == tab.i_p_key as i32 {
                continue;
            }
            if sqlite3_table_column_to_index(&idx, j as i32) >= 0 {
                continue;
            }
            idx.is_covering = false;
            break;
        }
    }
    idx
}

/// The non-space, non-comment tokens of `z`: (token type, start, length).
fn scan_tokens(z: &str) -> Vec<(u8, usize, usize)> {
    use crate::tk::{TK_COMMENT, TK_SPACE};
    let b = z.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < b.len() {
        let (n, t) = crate::tokenize::sqlite3_get_token(&b[pos..]);
        if n == 0 {
            break;
        }
        if t != TK_SPACE && t != TK_COMMENT {
            out.push((t, pos, n));
        }
        pos += n;
    }
    out
}

/// For each column definition of a CREATE TABLE statement: whether it has a NOT NULL
/// constraint written out. `None` if the text does not have the expected shape.
fn declared_not_null(z: &str, n_col: usize) -> Option<Vec<bool>> {
    use crate::tk::*;
    let t = scan_tokens(z);
    let lp = t.iter().position(|x| x.0 == TK_LP)?;
    let mut out = Vec::new();
    let mut depth = 0;
    let mut seg_start = true;
    let mut in_cols = true;
    let mut cur = false;
    for k in lp..t.len() {
        let x = t[k];
        if x.0 == TK_LP {
            depth += 1;
            if depth == 1 {
                seg_start = true;
            }
            continue;
        }
        if x.0 == TK_RP {
            depth -= 1;
            if depth == 0 {
                if in_cols {
                    out.push(cur);
                }
                break;
            }
            continue;
        }
        if depth != 1 {
            continue;
        }
        if x.0 == TK_COMMA {
            if in_cols {
                out.push(cur);
            }
            cur = false;
            seg_start = true;
            continue;
        }
        if seg_start {
            seg_start = false;
            if matches!(x.0, TK_CONSTRAINT | TK_PRIMARY | TK_UNIQUE | TK_CHECK | TK_FOREIGN) {
                in_cols = false;
            }
            continue;
        }
        if in_cols && x.0 == TK_NOT && t.get(k + 1).is_some_and(|y| y.0 == TK_NULL) {
            cur = true;
        }
    }
    if out.len() == n_col { Some(out) } else { None }
}

/// True if a UNIQUE keyword of a CREATE TABLE statement (column or table constraint) comes
/// before its PRIMARY keyword.
fn unique_before_primary(z: &str) -> bool {
    use crate::tk::*;
    let t = scan_tokens(z);
    let Some(lp) = t.iter().position(|x| x.0 == TK_LP) else { return true };
    let mut depth = 0;
    for x in &t[lp..] {
        if x.0 == TK_LP {
            depth += 1;
        } else if x.0 == TK_RP {
            depth -= 1;
        } else if depth == 1 && x.0 == TK_PRIMARY {
            return false;
        } else if depth == 1 && x.0 == TK_UNIQUE {
            return true;
        }
    }
    true
}

/// True if the text has `COLLATE binary` (any case, quoted or not).
fn has_explicit_binary_collate(z: &str) -> bool {
    let t = scan_tokens(z);
    t.windows(2).any(|w| {
        w[0].0 == crate::tk::TK_COLLATE
            && crate::util::sqlite3_dequote(&z[w[1].1..w[1].1 + w[1].2]).eq_ignore_ascii_case("binary")
    })
}

/// The key list and WHERE clause of a CREATE INDEX statement, parsed and resolved as
/// `sqlite3CreateIndex()` does while the schema is loaded.
struct IndexDecl {
    cols: Vec<IndexColDef>,
    /// The parsed `sortlist` (becomes `Index.aColExpr` when the index has an expression)
    list: ExprListId,
    has_expr: bool,
    /// `pPIWhere`
    p_where: ExprId,
}

/// Parse the `( sortlist ) [WHERE expr]` part of the text of a CREATE INDEX statement. C did
/// this without error when the schema was loaded: a failure here (`None`) leaves no error
/// behind and makes the table unsupported.
fn index_decl_from_sql(p: &mut Parse, tab: &Table, z_sql: &str) -> Option<IndexDecl> {
    let saved = (p.z_err_msg.clone(), p.n_err, p.rc, p.err_byte_offset);
    let r = index_decl_from_sql_inner(p, tab, z_sql);
    if r.is_none() && p.unsupported.is_none() {
        (p.z_err_msg, p.n_err, p.rc, p.err_byte_offset) = saved;
    }
    r
}

fn index_decl_from_sql_inner(p: &mut Parse, tab: &Table, z_sql: &str) -> Option<IndexDecl> {
    use crate::tk::*;
    let t = scan_tokens(z_sql);
    // CREATE [UNIQUE] INDEX [IF NOT EXISTS] nm [. nm] ON nm ( sortlist ) [WHERE expr]
    let on = t.iter().position(|x| x.0 == TK_ON)?;
    let lp = on + 2;
    if t.get(lp)?.0 != TK_LP {
        return None;
    }
    let mut depth = 0;
    let mut rp = None;
    for (k, x) in t.iter().enumerate().skip(lp) {
        if x.0 == TK_LP {
            depth += 1;
        } else if x.0 == TK_RP {
            depth -= 1;
            if depth == 0 {
                rp = Some(k);
                break;
            }
        }
    }
    let rp = rp?;
    let list_text = &z_sql[t[lp].1 + 1..t[rp].1];
    let where_text = match t.get(rp + 1) {
        Some(x) if x.0 == TK_WHERE => Some(&z_sql[x.1 + x.2..]),
        Some(_) => return None,
        None => None,
    };
    let text = match where_text {
        Some(w) => format!("SELECT 1 FROM x WHERE\n{w}\nORDER BY\n{list_text}\n"),
        None => format!("SELECT 1 FROM x ORDER BY\n{list_text}\n"),
    };
    let sel = parse_schema_select(p, &text);
    if sel.is_null() {
        return None;
    }
    let list = p.s(sel).p_order_by;
    let p_where = p.s(sel).p_where;
    if list.is_null() {
        return None;
    }
    // The table is not in the arena yet: resolve against a copy of it (the columns are final).
    let tab_id = p.new_tab(tab.clone());
    let n_err = p.n_err;
    if p_where.nn() {
        crate::resolve::sqlite3_resolve_self_reference(p, tab_id, NC_PartIdx, p_where, ExprListId::NULL);
    }
    let mut cols = Vec::new();
    let mut has_expr = false;
    for i in 0..p.l(list).a.len() {
        let e = p.l(list).a[i].p_expr;
        // sqlite3StringToId(pListItem->pExpr)
        if p.e(e).op == TK_STRING {
            p.em(e).op = TK_ID;
        } else if p.e(e).op == TK_COLLATE && p.e(p.e(e).p_left).op == TK_STRING {
            let l = p.e(e).p_left;
            p.em(l).op = TK_ID;
        }
        crate::resolve::sqlite3_resolve_self_reference(p, tab_id, NC_IdxExpr, e, ExprListId::NULL);
        if p.n_err != n_err {
            return None;
        }
        let c_expr = crate::expr::sqlite3_expr_skip_collate(p, e);
        let i_column = if p.e(c_expr).op != TK_COLUMN {
            has_expr = true;
            XN_EXPR
        } else {
            p.e(c_expr).i_column as i32
        };
        let z_coll = if p.e(e).op == TK_COLLATE { p.e(e).z_token.clone() } else { None };
        let desc = (p.l(list).a[i].sort_flags & sqliters_vdbe::vdbeaux::KEYINFO_ORDER_DESC) != 0;
        cols.push(IndexColDef { i_column, z_coll, desc });
    }
    if p.n_err != n_err {
        return None;
    }
    Some(IndexDecl { cols, list, has_expr, p_where })
}

/// Run the parser over the text of a SELECT statement the way C does while the schema is
/// loaded (a separate Parse with `db->init.busy`): the Select tree is built in the arenas of
/// this Parse, nothing is resolved or coded, and the counters of the statement being compiled
/// are untouched. Returns NULL if the text does not parse.
pub fn parse_schema_select(p: &mut Parse, text: &str) -> SelectId {
    let saved_sql = std::mem::replace(&mut p.z_sql, text.to_owned());
    let saved_tail = p.z_tail;
    let saved_last = p.s_last_token;
    let saved_rc = p.rc;
    let saved_n_select = p.n_select;
    let saved_explain = p.explain;
    let saved_exists = p.b_has_exists;
    let saved_with = p.b_has_with;
    let saved_compound = p.has_compound;
    let saved_n_var = p.n_var;
    let saved_vlist = std::mem::take(&mut p.p_v_list);
    let saved_p_with = p.p_with;
    let saved_dump = p.tree_dump.take();
    let saved_capture = p.capture_select;
    let saved_init = p.db.init_busy;
    let saved_err = p.z_err_msg.take();
    let saved_n_err = p.n_err;
    let saved_ofs = p.err_byte_offset;
    p.n_select = 0;
    p.capture_select = Some(SelectId::NULL);
    p.db.init_busy = true;
    crate::tokenize::sqlite3_run_parser(p);
    let failed = p.z_err_msg.is_some() || p.n_err != saved_n_err;
    let sel = p.capture_select.unwrap_or(SelectId::NULL);
    p.z_sql = saved_sql;
    p.z_tail = saved_tail;
    p.s_last_token = saved_last;
    p.n_select = saved_n_select;
    p.explain = saved_explain;
    p.b_has_exists = saved_exists;
    p.b_has_with = saved_with;
    p.has_compound = saved_compound;
    p.n_var = saved_n_var;
    p.p_v_list = saved_vlist;
    p.p_with = saved_p_with;
    p.tree_dump = saved_dump;
    p.capture_select = saved_capture;
    p.db.init_busy = saved_init;
    if failed && p.unsupported.is_some() {
        // a construct of the text that is not ported: the whole statement is unsupported
        return SelectId::NULL;
    }
    // (C accepted the text when the schema was loaded: a failure here is reported by the
    // caller as unsupported)
    p.z_err_msg = saved_err;
    p.n_err = saved_n_err;
    p.rc = saved_rc;
    p.err_byte_offset = saved_ofs;
    if failed { SelectId::NULL } else { sel }
}

/// The linking of `sqlite3CreateIndex()`: a new index goes to the head of `pTab->pIndex`;
/// then all REPLACE indexes are moved to the end of the list (at most the new one is out of
/// order).
fn link_index(list: &mut Vec<IndexId>, a_idx: &[Index], new: IndexId) {
    list.insert(0, new);
    let mut i = 0;
    while i < list.len() {
        if a_idx[list[i].ix()].on_error != OE_Replace as u8 {
            i += 1;
            continue;
        }
        while i + 1 < list.len() && a_idx[list[i + 1].ix()].on_error != OE_Replace as u8 {
            list.swap(i, i + 1);
            i += 1;
        }
        break;
    }
}

/// Build C's `Table` (and its `Index` list) for a declaration of database `i_db` and add it
/// to the arenas of the Parse. This is what `sqlite3InitOne()` leaves in the schema hash
/// after running the parser over the CREATE statements and `sqlite3AnalysisLoad()`.
pub fn build_table(p: &mut Parse, def: &TableDef, i_db: usize) -> TableId {
    let file_format = p.db.a_db.get(i_db).map_or(4, |d| d.file_format);
    let mut tab = Table {
        z_name: def.name.clone(),
        tnum: def.tnum,
        i_p_key: -1,
        n_tab_ref: 1,       // sqlite3StartTable(): pTable->nTabRef = 1
        n_row_log_est: 200, // sqlite3StartTable(): pTable->nRowLogEst = 200
        e_tab_type: def.e_tab_type,
        i_db: i_db as i32,
        z_sql: def.z_sql.clone(),
        has_trigger: def.has_trigger,
        ..Table::default()
    };
    for c in &def.cols {
        let col = add_column(c);
        if col.not_null != 0 {
            tab.tab_flags |= TF_HasNotNull;
        }
        if col.col_flags & COLFLAG_HIDDEN as u16 != 0 {
            tab.tab_flags |= TF_HasHidden;
        }
        if col.col_flags & COLFLAG_VIRTUAL as u16 != 0 {
            tab.tab_flags |= TF_HasVirtual;
        }
        if col.col_flags & COLFLAG_STORED as u16 != 0 {
            tab.tab_flags |= TF_HasStored;
        }
        tab.a_col.push(col);
    }
    // The engine's schema already has the NOT NULL that C adds to the PRIMARY KEY columns of
    // a WITHOUT ROWID or STRICT table at the end of the CREATE TABLE statement
    // (convertToWithoutRowidTable(), sqlite3EndTable()). The automatic indexes are created
    // before that, with the columns as declared (Index.uniqNotNull): take the declared state
    // from the text; the two steps below then add the NOT NULL as C does.
    if def.without_rowid || def.strict {
        match def.z_sql.as_deref().and_then(|z| declared_not_null(z, tab.a_col.len())) {
            Some(decl) => {
                for (c, d) in tab.a_col.iter_mut().zip(decl) {
                    if (c.col_flags & COLFLAG_PRIMKEY as u16) != 0 && !d {
                        c.not_null = OE_None as u8;
                    }
                }
                if tab.a_col.iter().all(|c| c.not_null == 0) {
                    tab.tab_flags &= !TF_HasNotNull;
                }
            }
            None => tab.unsupported_decl = Some("CREATE TABLE text whose column definitions could not be scanned"),
        }
    }
    tab.n_nv_col = tab.a_col.iter().filter(|c| c.col_flags & COLFLAG_VIRTUAL as u16 == 0).count() as i16;
    if def.has_primary_key {
        tab.tab_flags |= TF_HasPrimaryKey;
    }
    // sqlite3AddPrimaryKey(): a single-column INTEGER PRIMARY KEY (not DESC) is the rowid
    tab.i_p_key = def.i_p_key as i16;
    tab.key_conf = def.key_conf;
    if def.autoincrement {
        tab.tab_flags |= TF_Autoincrement;
    }
    if tab.tnum == 1 {
        tab.tab_flags |= TF_Readonly;
    }
    if def.e_tab_type == TABTYP_VIEW as u8 {
        // A view: the columns are computed by sqlite3ViewGetColumnNames() when it is used.
        // sqlite3CreateView(): never allow rowid in view (no SQLITE_ALLOW_ROWID_IN_VIEW)
        tab.tab_flags |= TF_NoVisibleRowid;
        tab.a_col.clear();
        tab.n_nv_col = 0;
        return p.new_tab(tab);
    }

    // What the adapter cannot represent yet
    // An automatic index over the INTEGER PRIMARY KEY column: whether C saw the column as the
    // rowid when it created the index (Index.uniqNotNull, aiColumn) depends on the order of
    // the constraints in the CREATE TABLE text, which the engine's schema does not record.
    // When every UNIQUE constraint comes after the PRIMARY KEY in the text, the column is
    // already the rowid for all of them (lookupName() gives iColumn==-1: create_index()).
    if tab.i_p_key >= 0
        && def
            .indexes
            .iter()
            .any(|d| d.idx_type != SQLITE_IDXTYPE_APPDEF as u8 && d.cols.iter().any(|c| c.i_column == tab.i_p_key as i32))
        && def.z_sql.as_deref().is_none_or(unique_before_primary)
    {
        tab.unsupported_decl = Some("UNIQUE constraint written before the INTEGER PRIMARY KEY it covers");
    }
    if def.unsupported.is_some() {
        tab.unsupported_decl = def.unsupported;
    }
    if tab.a_col.iter().any(|c| c.col_flags & COLFLAG_VIRTUAL as u16 != 0) {
        tab.unsupported_decl = Some("table with a VIRTUAL generated column");
    }
    if let Some(z) = &def.z_sql
        && has_explicit_binary_collate(z)
    {
        // C keeps "COLLATE BINARY" written in a column definition or constraint as an explicit
        // collation (shown as "B" in a KeyInfo); the engine's schema does not record it.
        tab.unsupported_decl = Some("CREATE TABLE with an explicit COLLATE BINARY");
    }
    // The automatic indexes of the CREATE TABLE statement are created while the statement
    // is parsed: the table is still a rowid table then (pPk==0).
    let mut list: Vec<IndexId> = Vec::new();
    let mut new_indexes: Vec<Index> = Vec::new();
    let n_auto = def.indexes.iter().take_while(|d| d.idx_type != SQLITE_IDXTYPE_APPDEF as u8).count();
    for d in &def.indexes[..n_auto] {
        let mut idx = create_index(&tab, d, None, false, file_format);
        sqlite3_default_row_est(&mut idx, &mut tab.n_row_log_est);
        new_indexes.push(idx);
    }
    // sqlite3EndTable(): STRICT
    if def.strict {
        tab.tab_flags |= TF_Strict;
        for ii in 0..tab.a_col.len() {
            if tab.a_col[ii].e_c_type == COLTYPE_ANY as u8 {
                tab.a_col[ii].affinity = SQLITE_AFF_BLOB;
            }
            if (tab.a_col[ii].col_flags & COLFLAG_PRIMKEY as u16) != 0
                && tab.i_p_key as i32 != ii as i32
                && tab.a_col[ii].not_null == OE_None as u8
            {
                tab.a_col[ii].not_null = OE_Abort as u8;
                tab.tab_flags |= TF_HasNotNull;
            }
        }
    }
    // The indexes get their ids now (list order is decided as each one is linked).
    let first_id = p.a_idx.len();
    for (k, idx) in new_indexes.into_iter().enumerate() {
        p.a_idx.push(idx);
        link_index(&mut list, &p.a_idx, IndexId((first_id + k) as u32));
    }
    // Special processing for WITHOUT ROWID Tables
    if def.without_rowid {
        tab.tab_flags |= TF_WithoutRowid | TF_NoVisibleRowid;
        convert_to_without_rowid_table(p, &mut tab, &mut list);
    }
    // estimateTableWidth(p); estimateIndexWidth() of every implied index
    {
        let mut w_table: u32 = tab.a_col.iter().map(|c| c.sz_est as u32).sum();
        if tab.i_p_key < 0 {
            w_table += 1;
        }
        tab.sz_tab_row = sqlite3_log_est(w_table as u64 * 4);
        for &id in &list {
            estimate_index_width(&mut p.a_idx[id.ix()], &tab.a_col);
        }
    }
    // The CREATE INDEX statements, in the order of sqlite_schema
    for d in &def.indexes[n_auto..] {
        let pk = if def.without_rowid {
            list.iter().map(|&i| &p.a_idx[i.ix()]).find(|i| i.is_primary_key()).cloned()
        } else {
            None
        };
        // C runs its parser over the CREATE INDEX statement: the key columns, their explicit
        // COLLATE and sort order, the indexed expressions and the WHERE clause come from the
        // text.
        let decl = match &d.z_sql {
            Some(z) => index_decl_from_sql(p, &tab, z),
            None => None,
        };
        let mut idx = match &decl {
            Some(parsed) => {
                let d2 = IndexDef { cols: parsed.cols.clone(), is_partial: parsed.p_where.nn(), ..d.clone() };
                let mut idx = create_index(&tab, &d2, pk.as_ref(), true, file_format);
                if idx.b_has_expr && parsed.has_expr {
                    idx.a_col_expr = parsed.list;
                }
                idx.p_part_idx_where = parsed.p_where;
                idx
            }
            None => {
                if tab.unsupported_decl.is_none() {
                    tab.unsupported_decl = Some("index whose CREATE INDEX text could not be parsed");
                }
                create_index(&tab, d, pk.as_ref(), true, file_format)
            }
        };
        idx.exprs_done = true;
        sqlite3_default_row_est(&mut idx, &mut tab.n_row_log_est);
        let id = p.new_idx(idx);
        link_index(&mut list, &p.a_idx, id);
    }

    // sqlite3AnalysisLoad(): the content of sqlite_stat1
    if def.has_stat1 {
        tab.n_row_log_est = def.n_row_log_est;
        tab.tab_flags |= TF_HasStat1;
    }
    if let Some(sz) = def.sz_tab_row {
        tab.sz_tab_row = sz;
    }
    for d in &def.indexes {
        let Some(st) = &d.stat else { continue };
        let Some(&id) = list.iter().find(|&&i| p.a_idx[i.ix()].z_name.eq_ignore_ascii_case(&d.name)) else { continue };
        let idx = &mut p.a_idx[id.ix()];
        let n = idx.n_key_col as usize + 1;
        let mut a = st.ai_row_log_est.clone();
        a.resize(n, 0);
        idx.ai_row_log_est = a;
        idx.b_unordered = st.b_unordered;
        idx.no_skip_scan = st.no_skip_scan;
        if let Some(sz) = st.sz_idx_row {
            idx.sz_idx_row = sz;
        }
        idx.has_stat1 = true;
    }
    // Load default statistics for the indexes without sqlite_stat1 data
    for &id in &list {
        if !p.a_idx[id.ix()].has_stat1 {
            let mut n = tab.n_row_log_est;
            sqlite3_default_row_est(&mut p.a_idx[id.ix()], &mut n);
            tab.n_row_log_est = n;
        }
    }

    // sqlite3AllocateIndexObject(): aiRowLogEst[] has nColumn+1 entries; the ones past
    // nKeyCol are zero (an equality on the rowid / PRIMARY KEY suffix reads them).
    for &id in &list {
        let ix = &mut p.a_idx[id.ix()];
        let n = ix.n_column as usize + 1;
        if ix.ai_row_log_est.len() < n {
            ix.ai_row_log_est.resize(n, 0);
        }
    }

    tab.p_index = list.clone();
    let tab_id = p.new_tab(tab);
    for id in list {
        p.a_idx[id.ix()].p_table = tab_id;
    }
    tab_id
}

/// `convertToWithoutRowidTable(pParse, pTab)`: this routine runs at the end of parsing a
/// CREATE TABLE statement that has a WITHOUT ROWID clause. The job of this routine is to
/// convert both the internal schema representation and the generated VDBE code so that
/// the table is really a WITHOUT ROWID table.
fn convert_to_without_rowid_table(p: &mut Parse, tab: &mut Table, list: &mut Vec<IndexId>) {
    // Mark every PRIMARY KEY column as NOT NULL (except for imposter tables)
    for c in tab.a_col.iter_mut() {
        if (c.col_flags & COLFLAG_PRIMKEY as u16) != 0 && c.not_null == OE_None as u8 {
            c.not_null = OE_Abort as u8;
        }
    }
    tab.tab_flags |= TF_HasNotNull;

    // Locate the PRIMARY KEY index. Or, if this table was originally an INTEGER PRIMARY KEY
    // table, create a new PRIMARY KEY index.
    let pk_id: IndexId;
    if tab.i_p_key >= 0 {
        // (the adapter always hands over the PRIMARY KEY index of a WITHOUT ROWID table as a
        // declared index, also when the key is a single INTEGER column: C creates it here,
        // after the other implied indexes; `i_p_key` is -1 for WITHOUT ROWID declarations)
        tab.i_p_key = -1;
    }
    let Some(&found) = list.iter().find(|&&i| p.a_idx[i.ix()].is_primary_key()) else {
        return;
    };
    pk_id = found;
    {
        // Remove all redundant columns from the PRIMARY KEY. For example, change
        // "PRIMARY KEY(a,b,a,b,c,b,c,d)" into just "PRIMARY KEY(a,b,c,d)". Later code assumes
        // the PRIMARY KEY contains no repeated columns.
        let pk = &mut p.a_idx[pk_id.ix()];
        let mut j = 1usize;
        for i in 1..pk.n_key_col as usize {
            let (x, coll) = (pk.ai_column[i], pk.az_coll[i].clone());
            if is_dup_column(pk, j, x, &coll) {
                pk.n_column -= 1;
            } else {
                pk.az_coll[j] = pk.az_coll[i].clone();
                pk.a_coll_static[j] = pk.a_coll_static[i];
                pk.a_sort_order[j] = pk.a_sort_order[i];
                pk.ai_column[j] = pk.ai_column[i];
                j += 1;
            }
        }
        if pk.n_key_col > 0 {
            pk.n_key_col = j.max(1) as u16;
        }
        pk.is_covering = true;
        pk.uniq_not_null = true;
        pk.n_column = pk.n_key_col;
        let n_pk = pk.n_key_col as usize;
        pk.ai_column.truncate(n_pk);
        pk.az_coll.truncate(n_pk);
        pk.a_coll_static.truncate(n_pk);
        pk.a_sort_order.truncate(n_pk);
        // The root page of the PRIMARY KEY is the table root page
        pk.tnum = tab.tnum;
    }
    let pk = p.a_idx[pk_id.ix()].clone();
    let n_pk = pk.n_key_col as usize;

    // Update the in-memory representation of all UNIQUE indices by converting the final
    // rowid column into one or more columns of the PRIMARY KEY.
    for &id in list.iter() {
        let idx = &mut p.a_idx[id.ix()];
        if idx.is_primary_key() {
            continue;
        }
        let n_key = idx.n_key_col as usize;
        let mut n = 0;
        for i in 0..n_pk {
            if !is_dup_column(idx, n_key, pk.ai_column[i], &pk.az_coll[i]) {
                n += 1;
            }
        }
        // resizeIndexObject(pParse, pIdx, pIdx->nKeyCol+n): the arrays keep the first
        // nKeyCol entries
        idx.ai_column.truncate(n_key);
        idx.az_coll.truncate(n_key);
        idx.a_coll_static.truncate(n_key);
        idx.a_sort_order.truncate(n_key);
        if n == 0 {
            // This index is a superset of the primary key
            idx.n_column = idx.n_key_col;
            continue;
        }
        for i in 0..n_pk {
            if !is_dup_column(idx, n_key, pk.ai_column[i], &pk.az_coll[i]) {
                idx.ai_column.push(pk.ai_column[i]);
                idx.az_coll.push(pk.az_coll[i].clone());
                idx.a_coll_static.push(pk.a_coll_static[i]);
                idx.a_sort_order.push(0);
                if pk.a_sort_order[i] != 0 {
                    // See ticket https://sqlite.org/src/info/bba7b69f9849b5bf
                    idx.b_asc_key_bug = true;
                }
            }
        }
        // resizeIndexObject() is a no-op if the index already has that many columns (the
        // rowid slot counts)
        if (n_key + n) > idx.n_column as usize {
            idx.is_resized = true;
        }
        idx.n_column = (n_key + n) as u16;
    }

    // Add all table columns to the PRIMARY KEY index
    let pk = &mut p.a_idx[pk_id.ix()];
    for i in 0..tab.a_col.len() {
        if !pk.ai_column.contains(&(i as i16)) && (tab.a_col[i].col_flags & COLFLAG_VIRTUAL as u16) == 0 {
            // pPk->azColl[j] = zColl ? zColl : sqlite3StrBINARY
            let z_coll = sqlite3_column_coll(&tab.a_col[i]).map(|z| z.to_owned());
            pk.ai_column.push(i as i16);
            pk.a_coll_static.push(z_coll.is_none());
            pk.az_coll.push(z_coll.unwrap_or_else(|| "BINARY".to_owned()));
            pk.a_sort_order.push(0);
        }
    }
    // resizeIndexObject(pParse, pPk, nPk+nExtra) happened if the index grew
    if pk.ai_column.len() > pk.n_column as usize {
        pk.is_resized = true;
    }
    pk.n_column = pk.ai_column.len() as u16;
    recompute_columns_not_indexed(pk, &tab.a_col);
}

/// `sqlite3PrimaryKeyIndex(pTab)`: return the PRIMARY KEY index of a table.
pub fn sqlite3_primary_key_index(p: &Parse, tab: TableId) -> IndexId {
    for &i in &p.tab(tab).p_index {
        if p.idx(i).is_primary_key() {
            return i;
        }
    }
    IndexId::NULL
}

/// `sqlite3TableColumnToIndex(pIdx, iCol)`: convert a table column number into an index
/// column number. That is, for the column iCol in the table (as defined by the CREATE TABLE
/// statement) find the (first) offset of that column in index pIdx. Or return -1 if column
/// iCol is not used in index pIdx.
pub fn sqlite3_table_column_to_index(idx: &Index, i_col: i32) -> i32 {
    let i_col16 = i_col as i16;
    for i in 0..idx.n_column as usize {
        if i_col16 == idx.ai_column[i] {
            return i as i32;
        }
    }
    -1
}

/// `sqlite3StorageColumnToTable(pTab, iCol)`: convert a storage column number into a table
/// column number.
pub fn sqlite3_storage_column_to_table(tab: &Table, mut i_col: i32) -> i32 {
    if tab.tab_flags & TF_HasVirtual != 0 {
        let mut i = 0;
        while i <= i_col {
            if tab.a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
                i_col += 1;
            }
            i += 1;
        }
    }
    i_col
}

/// `sqlite3TableColumnToStorage(pTab, iCol)`: convert a table column number into a storage
/// column number.
pub fn sqlite3_table_column_to_storage(tab: &Table, i_col: i32) -> i32 {
    if (tab.tab_flags & TF_HasVirtual) == 0 || i_col < 0 {
        return i_col;
    }
    let mut n = 0;
    let mut i = 0;
    while i < i_col {
        if (tab.a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16) == 0 {
            n += 1;
        }
        i += 1;
    }
    if tab.a_col[i as usize].col_flags & COLFLAG_VIRTUAL as u16 != 0 {
        // iCol is a virtual column itself
        tab.n_nv_col as i32 + i - n
    } else {
        // iCol is a normal or stored column
        n
    }
}

/// `sqlite3FindTable(db, zName, zDatabase)`: locate the in-memory structure that describes a
/// particular database table given the name of that table and (optionally) the name of the
/// database containing the table. Return NULL if not found.
///
/// If zDatabase is 0, all databases are searched for the table and the first matching table
/// is returned. (No checking for duplicate table names is done.) The search order is TEMP
/// first, then MAIN, then any auxiliary databases added using the ATTACH command.
pub fn sqlite3_find_table(p: &mut Parse, z_name: &str, z_database: Option<&str>) -> TableId {
    const LEGACY_SCHEMA_TABLE: &str = "sqlite_master";
    const LEGACY_TEMP_SCHEMA_TABLE: &str = "sqlite_temp_master";
    const PREFERRED_SCHEMA_TABLE: &str = "sqlite_schema";
    const PREFERRED_TEMP_SCHEMA_TABLE: &str = "sqlite_temp_schema";
    let n_db = p.db.a_db.len();
    let is_sqlite_ = z_name.len() >= 7 && z_name.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_");
    let tail = if is_sqlite_ { &z_name[7..] } else { "" };
    if let Some(z_db) = z_database {
        let mut i = 0;
        while i < n_db {
            if z_db.eq_ignore_ascii_case(&p.db.a_db[i].z_db_s_name) {
                break;
            }
            i += 1;
        }
        if i >= n_db {
            // No match against the official names. But always match "main" to schema 0 as a
            // legacy fallback.
            if z_db.eq_ignore_ascii_case("main") {
                i = 0;
            } else {
                return TableId::NULL;
            }
        }
        let mut t = hash_find(p, i, z_name);
        if t.is_null() && is_sqlite_ {
            if i == 1 {
                if tail.eq_ignore_ascii_case(&PREFERRED_TEMP_SCHEMA_TABLE[7..])
                    || tail.eq_ignore_ascii_case(&PREFERRED_SCHEMA_TABLE[7..])
                    || tail.eq_ignore_ascii_case(&LEGACY_SCHEMA_TABLE[7..])
                {
                    t = hash_find(p, 1, LEGACY_TEMP_SCHEMA_TABLE);
                }
            } else if tail.eq_ignore_ascii_case(&PREFERRED_SCHEMA_TABLE[7..]) {
                t = hash_find(p, i, LEGACY_SCHEMA_TABLE);
            }
        }
        t
    } else {
        // Match against TEMP first
        let mut t = if n_db > 1 { hash_find(p, 1, z_name) } else { TableId::NULL };
        if t.nn() {
            return t;
        }
        // The main database is second
        t = hash_find(p, 0, z_name);
        if t.nn() {
            return t;
        }
        // Attached databases are in order of attachment
        for i in 2..n_db {
            t = hash_find(p, i, z_name);
            if t.nn() {
                break;
            }
        }
        if t.is_null() && is_sqlite_ {
            if tail.eq_ignore_ascii_case(&PREFERRED_SCHEMA_TABLE[7..]) {
                t = hash_find(p, 0, LEGACY_SCHEMA_TABLE);
            } else if tail.eq_ignore_ascii_case(&PREFERRED_TEMP_SCHEMA_TABLE[7..]) && n_db > 1 {
                t = hash_find(p, 1, LEGACY_TEMP_SCHEMA_TABLE);
            }
        }
        t
    }
}

/// `sqlite3HashFind(&db->aDb[i].pSchema->tblHash, zName)`
fn hash_find(p: &mut Parse, i_db: usize, z_name: &str) -> TableId {
    let key = (i_db, z_name.to_ascii_lowercase());
    if let Some(&t) = p.tab_cache.get(&key) {
        return t;
    }
    let host = p.db.host;
    let legacy = if i_db == 1 { "sqlite_temp_master" } else { "sqlite_master" };
    let t = match host.find_table(i_db, z_name) {
        Some(def) => build_table(p, &def, i_db),
        // Every database has its schema table (sqlite3InitOne() creates it from
        // "CREATE TABLE x(type text,name text,tbl_name text,rootpage int,sql text)", root page 1).
        None if z_name.eq_ignore_ascii_case(legacy) => {
            let col = |n: &str, ty: &str| ColumnDef { name: n.to_owned(), z_type: Some(ty.to_owned()), ..ColumnDef::default() };
            let def = TableDef {
                name: legacy.to_owned(),
                cols: vec![col("type", "text"), col("name", "text"), col("tbl_name", "text"), col("rootpage", "int"), col("sql", "text")],
                e_tab_type: TABTYP_NORM as u8,
                tnum: 1,
                i_p_key: -1,
                key_conf: OE_Abort as u8,
                n_row_log_est: 200,
                ..TableDef::default()
            };
            build_table(p, &def, i_db)
        }
        None => TableId::NULL,
    };
    p.tab_cache.insert(key, t);
    t
}

/// `sqlite3ReadSchema(pParse)`: this routine is a no-op if the database schema is already
/// initialized. Otherwise, the schema is loaded. The engine's schema is always available;
/// what remains of C's `sqlite3Init()` is its effect on the error offset: the nested
/// statements that read `sqlite_schema` reset `db->errByteOffset`.
pub fn sqlite3_read_schema(p: &mut Parse) {
    if p.db.schema_not_loaded {
        p.db.schema_not_loaded = false;
        p.err_byte_offset = -1;
    }
}

/// `sqlite3LocateTable(pParse, flags, zName, zDbase)`: locate the in-memory structure that
/// describes a particular database table given the name of that table and (optionally) the
/// name of the database containing the table. Return NULL if not found. Also leave an error
/// message in pParse->zErrMsg.
///
/// The difference between this routine and sqlite3FindTable() is that this routine leaves an
/// error message in pParse->zErrMsg where sqlite3FindTable() does not.
pub fn sqlite3_locate_table(p: &mut Parse, flags: u32, z_name: &str, z_dbase: Option<&str>) -> TableId {
    // Read the database schema. If an error occurs, leave an error message and code in
    // pParse and return NULL.
    sqlite3_read_schema(p);
    let mut t = sqlite3_find_table(p, z_name, z_dbase);
    if t.is_null() {
        // If zName is the not the name of a table in the schema created using CREATE, then
        // check to see if it is the name of an virtual table that can be an eponymous
        // virtual table.
        if (p.prep_flags as i32 & SQLITE_PREPARE_NO_VTAB) == 0 && !p.db.init_busy && is_eponymous_module(z_name) {
            p.not_supported("eponymous virtual table / table-valued function");
            return TableId::NULL;
        }
        if flags & LOCATE_NOERR as u32 != 0 {
            return TableId::NULL;
        }
        p.check_schema = true;
    } else if p.tab(t).is_virtual() {
        p.not_supported("virtual table");
        t = TableId::NULL;
        return t;
    }
    if t.nn()
        && let Some(why) = p.tab(t).unsupported_decl
    {
        p.not_supported(why);
        return TableId::NULL;
    }
    if t.is_null() {
        let z_msg = if flags & LOCATE_VIEW as u32 != 0 { "no such view" } else { "no such table" };
        match z_dbase {
            Some(d) => p.error_msg(format!("{z_msg}: {d}.{z_name}")),
            None => p.error_msg(format!("{z_msg}: {z_name}")),
        }
    }
    t
}

/// True if `z_name` can be an eponymous virtual table of the reference build: a registered
/// module with `xCreate==0` or `xCreate==xConnect`, `pragma_*` and the JSON table functions.
fn is_eponymous_module(z_name: &str) -> bool {
    let l = z_name.to_ascii_lowercase();
    l.starts_with("pragma_")
        || matches!(
            l.as_str(),
            "json_each" | "json_tree" | "jsonb_each" | "jsonb_tree" | "generate_series" | "dbstat" | "sqlite_dbpage"
                | "bytecode" | "tables_used" | "sqlite_stmt" | "completion" | "fts5vocab" | "rtree_i32" | "carray"
                | "sqlite_dbdata" | "sqlite_dbptr" | "fsdir" | "zipfile"
        )
}

/// `sqlite3LocateTableItem(pParse, flags, p)`: locate the table identified by a SrcItem.
pub fn sqlite3_locate_table_item(p: &mut Parse, flags: u32, src: SrcListId, i: usize) -> TableId {
    let (z_name, z_db) = {
        let it = &p.src(src).a[i];
        let z_db = if it.fixed_schema {
            Some(p.db.a_db[it.i_fixed_db as usize].z_db_s_name.clone())
        } else {
            it.z_database.clone()
        };
        (it.z_name.clone().unwrap_or_default(), z_db)
    };
    sqlite3_locate_table(p, flags, &z_name, z_db.as_deref())
}

/// `sqlite3KeyInfoOfIndex(pParse, pIdx)`: return a KeyInfo structure that is appropriate for
/// the given Index.
pub fn sqlite3_key_info_of_index(p: &mut Parse, idx: IndexId) -> Option<KeyInfo> {
    let (n_col, n_key, uniq) = {
        let i = p.idx(idx);
        (i.n_column as usize, i.n_key_col as usize, i.uniq_not_null)
    };
    if p.n_err != 0 {
        return None;
    }
    let mut key = if uniq {
        KeyInfo::alloc(p.enc(), n_key, n_col - n_key)
    } else {
        KeyInfo::alloc(p.enc(), n_col, 0)
    };
    for i in 0..n_col {
        let (z_coll, is_static) = (p.idx(idx).az_coll[i].clone(), p.idx(idx).a_coll_static[i]);
        // zColl==sqlite3StrBINARY ? 0 : sqlite3LocateCollSeq(pParse, zColl)
        key.a_coll[i] = if is_static { None } else { sqlite3_locate_coll_seq(p, &z_coll) };
        key.a_sort_flags[i] = p.idx(idx).a_sort_order[i];
    }
    if p.n_err != 0 {
        // (pIdx->bNoQuery: the collating sequence of an index is not registered. The index
        // cannot be used; C marks it and sets pParse->rc = SQLITE_ERROR_RETRY)
        return None;
    }
    Some(key)
}

/// `sqlite3VdbeSetP4KeyInfo(pParse, pIdx)`: set the P4 on the most recently added opcode to
/// the KeyInfo for the index pIdx.
pub fn sqlite3_vdbe_set_p4_key_info(p: &mut Parse, idx: IndexId) {
    if let Some(k) = sqlite3_key_info_of_index(p, idx) {
        p.v.append_p4(P4::KeyInfo(Arc::new(k)));
    }
}

/// `sqlite3TableLock(pParse, iDb, iTab, isWriteLock, zName)`: record the fact that we want to
/// lock a table at run-time. Shared-cache mode only: no btree of the engine is sharable, so
/// nothing is recorded (`if( !sqlite3BtreeSharable(pBtree) ) return`).
pub fn sqlite3_table_lock(_p: &mut Parse, _i_db: i32, _i_tab: u32, _is_write_lock: u8, _z_name: &str) {}

/// `sqlite3OpenTable(pParse, iCur, iDb, pTab, opcode)`: generate code that will open cursor
/// number `i_cur` for a table `tab` in database `i_db` for reading (`OP_OpenRead`) or writing
/// (`OP_OpenWrite`).
pub fn sqlite3_open_table(p: &mut Parse, i_cur: i32, i_db: i32, tab: TableId, opcode: u8) {
    let (tnum, z_name, has_rowid, n_nv_col) = {
        let t = p.tab(tab);
        (t.tnum, t.z_name.clone(), t.has_rowid(), t.n_nv_col as i32)
    };
    if p.within_rj_subrtn == 0 {
        sqlite3_table_lock(p, i_db, tnum, (opcode == OP_OpenWrite) as u8, &z_name);
    }
    if has_rowid {
        p.v.add_op4_int(opcode, i_cur, tnum as i32, i_db, n_nv_col);
    } else {
        let pk = sqlite3_primary_key_index(p, tab);
        let pk_tnum = p.idx(pk).tnum;
        p.v.add_op3(opcode, i_cur, pk_tnum as i32, i_db);
        sqlite3_vdbe_set_p4_key_info(p, pk);
    }
}

/// `sqlite3IndexAffinityStr(db, pIdx)`: return a pointer to the column affinity string
/// associated with index pIdx. A column affinity string has one character for each column in
/// the table, according to the affinity of the column:
///
/// ```text
///  Character      Column affinity
///  ------------------------------
///  'A'            BLOB
///  'B'            TEXT
///  'C'            NUMERIC
///  'D'            INTEGER
///  'F'            REAL
/// ```
///
/// An extra 'D' is appended to the end of the string to cover the rowid that appears as the
/// last column in every index.
pub fn sqlite3_index_affinity_str(p: &mut Parse, idx: IndexId) -> Vec<u8> {
    if let Some(z) = &p.idx(idx).z_col_aff {
        return z.clone();
    }
    // computeIndexAffStr(db, pIdx)
    let tab = p.idx(idx).p_table;
    let n = p.idx(idx).n_column as usize;
    let mut z = Vec::with_capacity(n);
    for i in 0..n {
        let x = p.idx(idx).ai_column[i] as i32;
        let mut aff = if x >= 0 {
            p.tab(tab).a_col[x as usize].affinity
        } else if x == XN_ROWID {
            SQLITE_AFF_INTEGER
        } else {
            let el = p.idx(idx).a_col_expr;
            let e = p.l(el).a[i].p_expr;
            crate::expr::sqlite3_expr_affinity(p, e)
        };
        if aff < SQLITE_AFF_BLOB {
            aff = SQLITE_AFF_BLOB;
        }
        if aff > SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_NUMERIC;
        }
        z.push(aff);
    }
    p.idxm(idx).z_col_aff = Some(z.clone());
    z
}

/// `sqlite3IndexColumnAffinity(db, pIdx, iCol)`: return the affinity for a single column of
/// an index.
pub fn sqlite3_index_column_affinity(p: &mut Parse, idx: IndexId, i_col: i32) -> u8 {
    sqlite3_index_affinity_str(p, idx)[i_col as usize]
}

/// The P4 of an `OP_OpenRead` on a table of a schema with shared-cache locks would be a
/// `P4_TABLE`; kept for completeness of the operand kinds.
#[allow(dead_code)]
fn p4_table(name: &str) -> P4 {
    P4::Table(Arc::new(P4Table { name: name.to_owned() }))
}

/// `fixExprCb(p, pExpr)` of `attach.c` (walker callback of sqlite3FixSelect; `b1` is
/// DbFixer.bTemp).
fn fix_expr_cb(p: &mut Parse, w: &mut Walker, e: ExprId) -> i32 {
    if !w.b1 {
        p.em(e).flags |= EP_FromDDL;
    }
    if p.e(e).op == crate::tk::TK_VARIABLE {
        // db->init.busy: a bound parameter of a schema object reads as NULL
        p.em(e).op = crate::tk::TK_NULL;
    }
    WRC_Continue
}

/// `fixSelectCb(p, pSelect)` of `attach.c` (`n` is the database of the schema object, `b1`
/// is DbFixer.bTemp): every table reference of a non-TEMP view is tied to the database of the
/// view.
fn fix_select_cb(p: &mut Parse, w: &mut Walker, sel: SelectId) -> i32 {
    let src = p.s(sel).p_src;
    if src.is_null() {
        return WRC_Continue;
    }
    for i in 0..p.src(src).a.len() {
        if !w.b1 && !p.src(src).a[i].is_subquery {
            let it = &mut p.srcm(src).a[i];
            if !it.fixed_schema && it.z_database.is_some() {
                // (a reference to another database is an error C reported when the view was
                // created)
                it.z_database = None;
                it.not_cte = true;
                it.had_schema = true;
            }
            it.i_fixed_db = w.n;
            it.from_ddl = true;
            it.fixed_schema = true;
        }
        let (is_using, on) = (p.src(src).a[i].is_using, p.src(src).a[i].p_on);
        if !is_using && crate::walker::sqlite3_walk_expr(p, w, on) != 0 {
            return WRC_Abort;
        }
    }
    let with = p.s(sel).p_with;
    if with.nn() {
        for i in 0..p.a_with[with.ix()].a.len() {
            let s = p.a_with[with.ix()].a[i].p_select;
            if crate::walker::sqlite3_walk_select(p, w, s) != 0 {
                return WRC_Abort;
            }
        }
    }
    WRC_Continue
}

/// The SELECT of a view (`Table.u.view.pSelect`) and its column-name list (`Table.pCheck`),
/// which C builds when the schema is loaded (`sqlite3CreateView()` with `db->init.busy`: the
/// parser runs over the CREATE VIEW text, `sqlite3FixSelect()` ties the table references to
/// the database of the view, and a reduced copy of the Select is kept). Returns false (with
/// the statement marked unsupported) if the text cannot be handled.
pub fn ensure_view_select(p: &mut Parse, tab: TableId) -> bool {
    use crate::tk::*;
    if p.tab(tab).p_select.nn() {
        return true;
    }
    let Some(z) = p.tab(tab).z_sql.clone() else {
        p.not_supported("view without CREATE VIEW text");
        return false;
    };
    // CREATE [TEMP|TEMPORARY] VIEW [IF NOT EXISTS] nm [. nm] [( eidlist )] AS select
    let t = scan_tokens(&z);
    let mut k = match t.iter().position(|x| x.0 == TK_VIEW) {
        Some(k) => k + 1,
        None => {
            p.not_supported("view whose CREATE VIEW text could not be parsed");
            return false;
        }
    };
    if t.get(k).is_some_and(|x| x.0 == TK_IF) {
        k += 3;
    }
    k += 1; // nm
    if t.get(k).is_some_and(|x| x.0 == TK_DOT) {
        k += 2;
    }
    let mut names: Vec<String> = Vec::new();
    if t.get(k).is_some_and(|x| x.0 == TK_LP) {
        let mut depth = 0;
        let mut want_name = false;
        while k < t.len() {
            let x = t[k];
            if x.0 == TK_LP {
                depth += 1;
                want_name = depth == 1;
            } else if x.0 == TK_RP {
                depth -= 1;
                if depth == 0 {
                    k += 1;
                    break;
                }
            } else if x.0 == TK_COMMA && depth == 1 {
                want_name = true;
            } else if want_name {
                names.push(crate::util::sqlite3_dequote(&z[x.1..x.1 + x.2]));
                want_name = false;
            }
            k += 1;
        }
    }
    if !t.get(k).is_some_and(|x| x.0 == TK_AS) {
        p.not_supported("view whose CREATE VIEW text could not be parsed");
        return false;
    }
    let text = z[t[k].1 + t[k].2..].to_owned();
    let sel = parse_schema_select(p, &text);
    if sel.is_null() {
        p.not_supported("view whose CREATE VIEW text could not be parsed");
        return false;
    }
    // sqlite3FixInit(&sFix, pParse, iDb, "view", pName); sqlite3FixSelect(&sFix, pSelect)
    let i_db = p.tab(tab).i_db;
    let mut w = Walker {
        x_expr_callback: Some(fix_expr_cb),
        x_select_callback: Some(fix_select_cb),
        n: i_db,
        b1: i_db == 1,
        ..Walker::default()
    };
    crate::walker::sqlite3_walk_select(p, &mut w, sel);
    // pSelect->selFlags |= SF_View
    p.sm(sel).sel_flags |= SF_View;
    // p->u.view.pSelect = sqlite3SelectDup(db, pSelect, EXPRDUP_REDUCE)
    let dup = crate::expr::sqlite3_select_dup(p, sel, EXPRDUP_REDUCE);
    // p->pCheck = sqlite3ExprListDup(db, pCNames, EXPRDUP_REDUCE): the names of eidlist_opt
    let mut check = ExprListId::NULL;
    for n in names {
        check = crate::expr::sqlite3_expr_list_append(p, check, ExprId::NULL);
        p.lm(check).a.last_mut().expect("item").z_e_name = Some(n);
    }
    let t = p.tabm(tab);
    t.p_select = dup;
    t.p_check = check;
    true
}
