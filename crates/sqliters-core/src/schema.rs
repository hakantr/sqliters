//! Schema catalog (`sqlite_schema` / `sqlite_master` on Page 1), ported from `sqlite/src/build.c`
//! and `sqlite/src/prepare.c`.
#![forbid(unsafe_code)]

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use sqliters_btree::Btree;
use sqliters_syntax::FkAction;
use sqliters_syntax::{
    ColumnConstraint, ColumnRef, CommonTableExpr, ConflictAction, CreateIndexStmt, CreateTableStmt,
    Expr, FromClause, FunctionArgs, IndexedColumn, JoinClause, JoinConstraint, LimitClause,
    OrderByTerm, QualifiedName, ResultColumn, SelectBody, SelectCore, SelectStmt, Stmt,
    TableConstraint, TableOrSubquery, TriggerTiming, WindowFrame, WindowFrameBound, WindowRef,
    WindowSpec, parse_one,
};
use sqliters_types::{
    Collation, CustomCollation, ErrorCode, ExecutionServices, KeySortOrder, Pgno, Result,
    SqliteError, TypeAffinity, Value, decode_record_with_encoding, encode_record_with_encoding,
};
use sqliters_vdbe::P4;

use crate::execution_context::CollationRegistry;

use crate::trigger::{
    TriggerSchema, fix_permanent_trigger, is_reserved_trigger_or_system_name,
    reject_raise_outside_trigger_expr, replace_bind_params_with_null_in_trigger,
    validate_trigger_body_dml_targets,
};
use crate::window::{is_window_bound_constant, reject_window_in_ddl_expr};

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
    pub custom_collation: Option<String>,
    pub not_null: Option<ConflictAction>,
    pub default_expr: Option<Expr>,
    /// Source text of `default_expr` as written in the schema (see `ColumnDef::default_src`).
    pub default_src: Option<String>,
    pub generated: Option<(Expr, bool)>,
    pub is_primary_key: bool,
    /// `true` when the column has an inline `UNIQUE` column constraint (`COLFLAG_UNIQUE` in `sqliteInt.h`).
    pub is_unique: bool,
    /// 1-based position within PRIMARY KEY (`0` if not part of PK), matching `PRAGMA table_info`.
    pub pk_order: usize,
}

/// `valueFromExpr` (`vdbemem.c:1795`): the constant a DEFAULT expression stands for when a
/// row is shorter than the table (rows written before `ALTER TABLE ... ADD COLUMN`), with
/// `affinity` applied; `None` when the expression is not one of the forms C can fold — a
/// literal (number, string, blob, NULL, TRUE/FALSE) under any nesting of unary `+` / `-` and
/// `CAST` — in which case such rows read NULL.
fn default_value_from_expr(expr: &Expr, affinity: TypeAffinity) -> Option<Value> {
    use sqliters_syntax::UnaryOp;
    match expr {
        Expr::Unary {
            op: UnaryOp::Plus,
            expr,
        } => default_value_from_expr(expr, affinity),
        Expr::Cast { expr, type_name } => {
            let cast_affinity = TypeAffinity::from_declared_type(type_name);
            let inner = default_value_from_expr(expr, cast_affinity)?;
            Some(affinity.apply(sqliters_vdbe::eval_cast(inner, type_name)))
        }
        Expr::Unary {
            op: UnaryOp::Negate,
            expr: inner,
        } => match inner.as_ref() {
            // A negative numeric literal is folded in one step.
            Expr::Integer(i) => Some(affinity.apply(Value::Integer(i.wrapping_neg()))),
            Expr::Float(f) => Some(affinity.apply(Value::real(-*f))),
            // `-(-5)`, `-'12'`, `-x'31'`: evaluate, `sqlite3VdbeMemNumerify`, negate.
            _ => {
                let v = default_value_from_expr(inner, affinity)?;
                let negated = match sqliters_vdbe::eval_cast(v, "NUMERIC") {
                    Value::Null => Value::Null,
                    Value::Real(r) => Value::real(-r),
                    Value::Integer(i64::MIN) => Value::real(-(i64::MIN as f64)),
                    Value::Integer(i) => Value::Integer(-i),
                    other => other,
                };
                Some(affinity.apply(negated))
            }
        },
        Expr::Null => Some(Value::Null),
        Expr::Bool(b) => Some(affinity.apply(Value::Integer(i64::from(*b)))),
        Expr::Integer(i) => Some(affinity.apply(Value::Integer(*i))),
        Expr::Float(f) => Some(affinity.apply(Value::real(*f))),
        Expr::String(s) => Some(affinity.apply(Value::Text(s.clone()))),
        Expr::Blob(b) => Some(Value::Blob(b.clone())),
        _ => None,
    }
}

/// `valueFromExpr` for a float literal default (optionally with one minus sign) of a
/// TEXT-affinity column: C builds the value from the token text (`"%s%s", zNeg, zToken`) and
/// TEXT affinity leaves that text alone, so a short row reads `'5.'` or `'1e3'` rather than the
/// canonical rendering of the number.
fn default_float_token_text(
    expr: &Expr,
    src: Option<&str>,
    affinity: TypeAffinity,
) -> Option<Value> {
    use sqliters_syntax::UnaryOp;
    if affinity != TypeAffinity::Text {
        return None;
    }
    let mut e = expr;
    while let Expr::Unary {
        op: UnaryOp::Plus,
        expr: inner,
    } = e
    {
        e = inner;
    }
    let neg = match e {
        Expr::Float(_) => false,
        Expr::Unary {
            op: UnaryOp::Negate,
            expr: inner,
        } if matches!(inner.as_ref(), Expr::Float(_)) => true,
        _ => return None,
    };
    let mut t = src?.trim_matches(|c: char| c.is_ascii_whitespace());
    loop {
        let stripped = t
            .strip_prefix('+')
            .or_else(|| t.strip_prefix('('))
            .map(|r| r.trim_start_matches(|c: char| c.is_ascii_whitespace()));
        match stripped {
            Some(r) => t = r,
            None => break,
        }
    }
    if neg {
        t = t
            .strip_prefix('-')?
            .trim_start_matches(|c: char| c.is_ascii_whitespace());
    }
    let t = t.trim_end_matches(|c: char| c.is_ascii_whitespace() || c == ')');
    if t.is_empty()
        || !t
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        return None;
    }
    let tok: String = t.chars().filter(|&c| c != '_').collect();
    Some(Value::Text(if neg { format!("-{tok}") } else { tok }))
}

impl ColumnSchema {
    pub fn collation_name(&self) -> &str {
        if let Some(ref cname) = self.custom_collation {
            cname.as_str()
        } else {
            match self.collation {
                Collation::Binary => "BINARY",
                Collation::NoCase => "NOCASE",
                Collation::RTrim => "RTRIM",
            }
        }
    }

    pub fn key_sort_order(&self, desc: bool, nulls_last: bool) -> KeySortOrder {
        KeySortOrder {
            desc,
            nulls_last,
            collation: self.collation,
            custom_collation: self
                .custom_collation
                .as_deref()
                .map(CustomCollation::unbound),
        }
    }
    /// Extracts a constant `P4` fallback (with column affinity + `STRICT` `REAL` coercion) for
    /// `OP_Column` when reading rows written prior to `ALTER TABLE ... ADD COLUMN`.
    pub fn default_p4_for_table(&self, table_strict: bool) -> P4 {
        // `sqlite3ColumnDefault` → `sqlite3ValueFromExpr(db, pDflt, enc, pCol->affinity, &v)`.
        let Some(v) = self.default_expr.as_ref().and_then(|e| {
            default_float_token_text(e, self.default_src.as_deref(), self.affinity)
                .or_else(|| default_value_from_expr(e, self.affinity))
        }) else {
            return P4::None;
        };
        let mut coerced = v;
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

    /// Estimates column width (`pCol->szEst` in `sqlite/src/build.c:1559-1775`), where 1 unit = 4 bytes.
    pub fn estimated_width(&self) -> u32 {
        let trimmed = self.type_name.trim();
        if trimmed.is_empty() {
            return 1;
        }
        if matches!(self.affinity, TypeAffinity::Text | TypeAffinity::Blob) {
            let upper = trimmed.to_ascii_uppercase();
            if let Some(pos) = upper.find("CHAR").or_else(|| upper.find("BLOB")) {
                let tail = &upper[pos + 4..];
                if let Some(dpos) = tail.find(|c: char| c.is_ascii_digit()) {
                    let digits: String = tail[dpos..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(v) = digits.parse::<u32>() {
                        return (v / 4 + 1).clamp(1, 255);
                    }
                }
            }
            return 5;
        }
        1
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

/// Table-level `sqlite_stat1` statistics (`pTable->nRowLogEst`, `pTable->szTabRow`, `TF_HasStat1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableStat1 {
    pub n_rows: u64,
    pub n_row_log_est: i16,
    pub sz_tab_row: Option<i16>,
    pub has_stat1: bool,
}

impl Default for TableStat1 {
    fn default() -> Self {
        Self {
            n_rows: 1_048_576,
            n_row_log_est: 200,
            sz_tab_row: None,
            has_stat1: false,
        }
    }
}

/// Index-level `sqlite_stat1` statistics (`pIndex->aiRowLogEst`, `bUnordered`, `noSkipScan`, `szIdxRow`, `hasStat1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexStat1 {
    pub int_values: Vec<u64>,
    pub row_log_est: Vec<i16>,
    pub unordered: bool,
    pub no_skip_scan: bool,
    pub sz_idx_row: Option<i16>,
    pub has_stat1: bool,
}

/// Computes `10 * log2(x)` matching `sqlite3LogEst` (`sqlite/src/util.c:2082-2099`).
pub fn sqlite_log_est(mut x: u64) -> i16 {
    const A: [i16; 8] = [0, 2, 3, 5, 6, 7, 8, 9];
    let mut y: i16 = 40;
    if x < 8 {
        if x < 2 {
            return 0;
        }
        while x < 8 {
            y -= 10;
            x <<= 1;
        }
    } else {
        let i = 60i16 - (x.leading_zeros() as i16);
        y += i * 10;
        x >>= i as u32;
    }
    A[(x & 7) as usize] + y - 10
}

/// Log-space addition `10 * log2(2^(a/10) + 2^(b/10))` matching `sqlite3LogEstAdd` (`sqlite/src/util.c:2055-2076`).
pub fn sqlite_log_est_add(a: i16, b: i16) -> i16 {
    const X: [u8; 32] = [
        10, 10, 9, 9, 8, 8, 7, 7, 7, 6, 6, 6, 5, 5, 5, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2,
        2, 2,
    ];
    if a >= b {
        if a > b + 49 {
            return a;
        }
        if a > b + 31 {
            return a + 1;
        }
        a + i16::from(X[(a - b) as usize])
    } else {
        if b > a + 49 {
            return b;
        }
        if b > a + 31 {
            return b + 1;
        }
        b + i16::from(X[(b - a) as usize])
    }
}

/// Converts a `LogEst` back to an integer estimate (`sqlite3LogEstToInt` in `sqlite/src/util.c:2102-2115`).
pub fn sqlite_log_est_to_int(mut x: i16) -> u64 {
    let mut n = i64::from(x % 10);
    x /= 10;
    if n >= 5 {
        n -= 2;
    } else if n >= 1 {
        n -= 1;
    }
    if x > 60 {
        return i64::MAX as u64;
    }
    if x >= 3 {
        ((n + 8) as u64) << ((x - 3) as u32)
    } else {
        ((n + 8) as u64) >> ((3 - x) as u32)
    }
}

/// Builds default `aiRowLogEst` for an index without `sqlite_stat1` (`sqlite3DefaultRowEst` in `sqlite/src/build.c:4632-4670`).
pub fn default_index_row_log_est(
    table_row_log_est: i16,
    n_key_col: usize,
    unique: bool,
    is_partial: bool,
) -> Vec<i16> {
    const A_VAL: [i16; 5] = [33, 32, 30, 28, 26];
    let mut a = vec![0i16; n_key_col + 1];
    let mut x = table_row_log_est;
    if x < 99 {
        x = 99;
    }
    if is_partial {
        x -= 10;
    }
    a[0] = x;
    for i in 1..=n_key_col {
        if i <= 5 {
            a[i] = A_VAL[i - 1];
        } else {
            a[i] = 23;
        }
    }
    if unique && n_key_col > 0 {
        a[n_key_col] = 0;
    }
    a
}

/// Decodes a `sqlite_stat1.stat` string matching `decodeIntArray` (`sqlite/src/analyze.c:1520-1580`).
pub fn decode_stat1_string(stat_str: &str, n_out: usize) -> IndexStat1 {
    let bytes = stat_str.as_bytes();
    let mut pos = 0usize;
    let mut int_values = vec![0u64; n_out];
    let mut row_log_est = vec![0i16; n_out];
    let mut i = 0usize;

    while pos < bytes.len() {
        let mut v: u64 = 0;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            v = v
                .wrapping_mul(10)
                .wrapping_add(u64::from(bytes[pos] - b'0'));
            pos += 1;
        }
        if i < n_out {
            int_values[i] = v;
            row_log_est[i] = sqlite_log_est(v);
            i += 1;
        }
        if pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        } else {
            break;
        }
    }

    while i < n_out {
        if i > 0 {
            int_values[i] = int_values[i - 1];
            row_log_est[i] = row_log_est[i - 1];
        } else {
            int_values[i] = 0;
            row_log_est[i] = 0;
        }
        i += 1;
    }

    let mut unordered = false;
    let mut no_skip_scan = false;
    let mut sz_idx_row = None;

    while pos < bytes.len() {
        let rest = &stat_str[pos..];
        if rest.starts_with("unordered") {
            unordered = true;
        } else if let Some(after_sz) = rest.strip_prefix("sz=") {
            let after_bytes = after_sz.as_bytes();
            if !after_bytes.is_empty() && after_bytes[0].is_ascii_digit() {
                let mut sz: i64 = 0;
                let mut k = 0usize;
                while k < after_bytes.len() && after_bytes[k].is_ascii_digit() {
                    sz = sz
                        .saturating_mul(10)
                        .saturating_add(i64::from(after_bytes[k] - b'0'));
                    k += 1;
                }
                if sz < 2 {
                    sz = 2;
                }
                sz_idx_row = Some(sqlite_log_est(sz as u64));
            }
        } else if rest.starts_with("noskipscan") {
            no_skip_scan = true;
        }
        while pos < bytes.len() && bytes[pos] != b' ' {
            pos += 1;
        }
        while pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }
    }

    IndexStat1 {
        int_values,
        row_log_est,
        unordered,
        no_skip_scan,
        sz_idx_row,
        has_stat1: true,
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
    pub stat1: TableStat1,
    pub without_rowid_pk_stat1: Option<IndexStat1>,
}

impl TableSchema {
    /// Whether any column has REAL affinity (C stores integral values of such columns as
    /// integers in some statements and converts them back when the column is read).
    #[inline]
    pub fn has_real_col(&self) -> bool {
        self.columns
            .iter()
            .any(|c| c.affinity == TypeAffinity::Real)
    }

    /// Built-in descriptor for `sqlite_schema` / `sqlite_master` stored on root page 1.
    pub fn sqlite_schema_table() -> Self {
        let mk_col = |name: &str, ty: &str, aff: TypeAffinity| ColumnSchema {
            name: name.to_owned(),
            type_name: ty.to_owned(),
            affinity: aff,
            collation: Collation::Binary,
            custom_collation: None,
            not_null: None,
            default_expr: None,
            default_src: None,
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
            stat1: TableStat1::default(),
            without_rowid_pk_stat1: None,
        }
    }

    pub fn default_sz_tab_row(&self) -> i16 {
        let mut w_table: u64 = self
            .columns
            .iter()
            .map(|c| u64::from(c.estimated_width()))
            .sum();
        if self.rowid_alias_col.is_none() && !self.without_rowid {
            w_table += 1;
        }
        sqlite_log_est(w_table.max(1) * 4)
    }

    pub fn effective_sz_tab_row(&self) -> i16 {
        self.stat1
            .sz_tab_row
            .unwrap_or_else(|| self.default_sz_tab_row())
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
            .map(|ic| ic.sort_order.clone())
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
            stat1: self.without_rowid_pk_stat1.clone(),
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
    pub stat1: Option<IndexStat1>,
}

impl IndexSchema {
    pub fn effective_row_log_est(&self, tbl: &TableSchema) -> Vec<i16> {
        if let Some(ref s) = self.stat1
            && s.has_stat1
            && s.row_log_est.len() > self.columns.len()
        {
            return s.row_log_est.clone();
        }
        default_index_row_log_est(
            tbl.stat1.n_row_log_est,
            self.columns.len(),
            self.unique,
            self.where_clause.is_some(),
        )
    }

    pub fn is_unordered(&self) -> bool {
        self.stat1.as_ref().is_some_and(|s| s.unordered)
    }

    pub fn is_no_skip_scan(&self) -> bool {
        self.stat1.as_ref().is_some_and(|s| s.no_skip_scan)
    }

    pub fn default_sz_idx_row(&self, tbl: &TableSchema) -> i16 {
        if self.origin == "pk" && self.table_without_rowid && self.root_pgno == tbl.root_pgno {
            return tbl.default_sz_tab_row();
        }
        let mut w_index: u64 = 0;
        for ic in self.columns.iter().chain(self.trailing_pk_cols.iter()) {
            w_index += match ic.col_idx {
                Some(ci) => tbl
                    .columns
                    .get(ci)
                    .map_or(1, |c| u64::from(c.estimated_width())),
                None => 1,
            };
        }
        sqlite_log_est(w_index.max(1) * 4)
    }

    pub fn effective_sz_idx_row(&self, tbl: &TableSchema) -> i16 {
        self.stat1
            .as_ref()
            .and_then(|s| s.sz_idx_row)
            .unwrap_or_else(|| self.default_sz_idx_row(tbl))
    }
    /// Returns the full `KeySortOrder` slice for the on-disk Index B-Tree record:
    /// - Rowid table index: `[indexed_col_0, ..., indexed_col_{k-1}, trailing_rowid]`
    /// - `WITHOUT ROWID` index: `[indexed_col_0, ..., indexed_col_{k-1}, trailing_pk_cols...]`
    pub fn key_sort_orders(&self) -> Vec<KeySortOrder> {
        let mut orders: Vec<KeySortOrder> =
            self.columns.iter().map(|c| c.sort_order.clone()).collect();
        if self.table_without_rowid {
            orders.extend(self.trailing_pk_cols.iter().map(|c| c.sort_order.clone()));
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
            ic.col_idx == pk_col.col_idx
                && ic
                    .sort_order
                    .collation_name()
                    .eq_ignore_ascii_case(pk_col.sort_order.collation_name())
        }) {
            return Some(pos);
        }
        self.trailing_pk_cols
            .iter()
            .position(|tc| {
                tc.col_idx == pk_col.col_idx
                    && tc
                        .sort_order
                        .collation_name()
                        .eq_ignore_ascii_case(pk_col.sort_order.collation_name())
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

/// Virtual table catalog definition loaded lazily from `sqlite_schema` (`type = 'table'`, `rootpage = 0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualTableDef {
    pub schema_rowid: i64,
    pub name: String,
    pub module_name: String,
    pub args: Vec<String>,
    pub sql: String,
}

/// Complete in-memory catalog of tables, indexes, views, and triggers loaded from `sqlite_schema` (Page 1).
#[derive(Debug, Clone, Default)]
pub struct SchemaData {
    pub tables: Vec<TableSchema>,
    pub virtual_tables: Vec<VirtualTableDef>,
    pub indexes: Vec<IndexSchema>,
    pub views: Vec<ViewSchema>,
    pub triggers: Vec<TriggerSchema>,
    pub schema_cookie: u32,
    pub loaded: bool,
    pub had_pages: bool,
}

/// Copy-on-write (`Arc`-backed) schema handle so `Schema::clone` and `snapshot_open_schemas`
/// are O(1) atomic refcount increments on DML/query hot paths, cloning [`SchemaData`] only on DDL.
#[derive(Debug, Clone, Default)]
pub struct Schema(Arc<SchemaData>);

impl Deref for Schema {
    type Target = SchemaData;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Schema {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl Schema {
    #[inline]
    pub fn from_data(data: SchemaData) -> Self {
        Self(Arc::new(data))
    }
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

    pub fn find_virtual_table(&self, name: &str) -> Option<&VirtualTableDef> {
        self.virtual_tables
            .iter()
            .find(|vt| vt.name.eq_ignore_ascii_case(name))
    }

    pub fn find_virtual_table_mut(&mut self, name: &str) -> Option<&mut VirtualTableDef> {
        self.virtual_tables
            .iter_mut()
            .find(|vt| vt.name.eq_ignore_ascii_case(name))
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

    pub fn find_trigger(&self, name: &str) -> Option<&TriggerSchema> {
        self.triggers
            .iter()
            .find(|tr| tr.is_active() && tr.name.eq_ignore_ascii_case(name))
    }

    pub fn find_trigger_mut(&mut self, name: &str) -> Option<&mut TriggerSchema> {
        self.triggers
            .iter_mut()
            .find(|tr| tr.is_active() && tr.name.eq_ignore_ascii_case(name))
    }

    pub fn triggers_for_table(&self, table_name: &str) -> Vec<TriggerSchema> {
        self.triggers
            .iter()
            .filter(|tr| {
                tr.is_active()
                    && !tr.detached_to_temp
                    && tr.tbl_name.eq_ignore_ascii_case(table_name)
            })
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

    /// Re-binds all custom collation comparators on tables and indexes in this schema against `collations`.
    pub fn rebind_collations(&mut self, collations: &CollationRegistry) {
        let has_custom = self.tables.iter().any(|t| {
            t.without_rowid_pk_cols
                .iter()
                .any(|ic| ic.sort_order.custom_collation.is_some())
        }) || self.indexes.iter().any(|idx| {
            idx.columns
                .iter()
                .any(|ic| ic.sort_order.custom_collation.is_some())
                || idx
                    .trailing_pk_cols
                    .iter()
                    .any(|ic| ic.sort_order.custom_collation.is_some())
        });
        if !has_custom {
            return;
        }
        let data = Arc::make_mut(&mut self.0);
        for tbl in &mut data.tables {
            rebind_table_collations(tbl, collations);
        }
        for idx in &mut data.indexes {
            rebind_index_collations(idx, collations);
        }
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
            Ok(Self::from_data(SchemaData {
                tables: Vec::new(),
                virtual_tables: Vec::new(),
                indexes: Vec::new(),
                views: Vec::new(),
                triggers: Vec::new(),
                schema_cookie: btree.pager().header().schema_cookie,
                loaded: true,
                had_pages: false,
            }))
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
        let mut virtual_tables = Vec::new();
        let mut views = Vec::new();
        let mut pending_auto_by_table: Vec<(String, Vec<PendingAutoIndex>)> = Vec::new();
        let mut raw_index_rows: Vec<(i64, String, String, Pgno, Option<String>)> = Vec::new();
        let mut raw_trigger_rows: Vec<(i64, String, String, Option<String>)> = Vec::new();

        let corrupt_schema_ddl_err =
            |obj_name: &str, sql_text: &str, parse_err: Option<&str>| -> SqliteError {
                let disp = if obj_name.is_empty() { "?" } else { obj_name };
                let trimmed = sql_text.trim_start();
                if trimmed.len() >= 2
                    && trimmed.as_bytes()[..2].eq_ignore_ascii_case(b"cr")
                    && let Some(extra) = parse_err.filter(|s| !s.is_empty())
                {
                    SqliteError::corrupt(format!("malformed database schema ({disp}) - {extra}"))
                } else {
                    SqliteError::corrupt(format!("malformed database schema ({disp})"))
                }
            };

        for (rowid, payload) in raw_rows {
            let cols = decode_record_with_encoding(&payload, enc)?;
            if cols.len() < 5 {
                continue;
            }
            let obj_type = cols[0].as_str().unwrap_or("");
            let name = cols[1].as_str().unwrap_or("").to_owned();
            let tbl_name = cols[2].as_str().unwrap_or("").to_owned();
            // sqlite3InitCallback reads the root page from the text of the column
            // (`sqlite3GetUInt32(argv[3], ..)`), so a root page stored as TEXT still counts.
            let rootpage = match &cols[3] {
                Value::Text(t) => t.trim().parse::<u32>().unwrap_or(0),
                other => other.as_i64().unwrap_or(0) as Pgno,
            };
            let sql_opt = cols[4].as_str().map(str::to_owned);

            match obj_type {
                "table" => {
                    if let Some(sql_text) = sql_opt {
                        match parse_one(&sql_text) {
                            Ok((Some(Stmt::CreateTable(ct)), tail)) if tail.trim().is_empty() => {
                                let (mut tbl, autos) = build_table_schema_from_ast(&ct, rootpage)?;
                                tbl.schema_rowid = rowid;
                                tbl.name = name.clone();
                                tbl.raw_sql = sql_text;
                                pending_auto_by_table.push((name, autos));
                                tables.push(tbl);
                            }
                            Ok((
                                Some(Stmt::CreateVirtualTable {
                                    module_name, args, ..
                                }),
                                tail,
                            )) if tail.trim().is_empty() => {
                                virtual_tables.push(VirtualTableDef {
                                    schema_rowid: rowid,
                                    name,
                                    module_name,
                                    args,
                                    sql: sql_text,
                                });
                            }
                            Ok((Some(_), _)) => {
                                return Err(SqliteError::corrupt(
                                    "database disk image is malformed",
                                ));
                            }
                            Ok((None, _)) => {
                                return Err(corrupt_schema_ddl_err(&name, &sql_text, None));
                            }
                            Err(e) => {
                                return Err(corrupt_schema_ddl_err(
                                    &name,
                                    &sql_text,
                                    Some(&e.message),
                                ));
                            }
                        }
                    }
                }
                "index" => {
                    raw_index_rows.push((rowid, name, tbl_name, rootpage, sql_opt));
                }
                "view" => {
                    if let Some(sql_text) = sql_opt {
                        let parsed = parse_one(&sql_text);
                        let cv = match parsed {
                            Ok((Some(Stmt::CreateView(cv)), tail)) if tail.trim().is_empty() => cv,
                            Err(e) => {
                                if writable_schema {
                                    continue;
                                }
                                return Err(corrupt_schema_ddl_err(
                                    &name,
                                    &sql_text,
                                    Some(&e.message),
                                ));
                            }
                            _ => {
                                if writable_schema {
                                    continue;
                                }
                                return Err(corrupt_schema_ddl_err(&name, &sql_text, None));
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
                "trigger" => {
                    raw_trigger_rows.push((rowid, name, tbl_name, sql_opt));
                }
                _ => {}
            }
        }

        let is_temp_schema = db_schema_name.is_some_and(|s| s.eq_ignore_ascii_case("temp"));
        let mut triggers: Vec<TriggerSchema> = Vec::new();
        for (rowid, trig_name, tbl_name_col, sql_opt) in raw_trigger_rows {
            let Some(sql_text) = sql_opt else {
                continue;
            };
            let mut ct = match parse_one(&sql_text) {
                Ok((Some(Stmt::CreateTrigger(ct)), tail)) if tail.trim().is_empty() => ct,
                Err(e) => {
                    if writable_schema {
                        continue;
                    }
                    return Err(corrupt_schema_ddl_err(
                        &trig_name,
                        &sql_text,
                        Some(&e.message),
                    ));
                }
                _ => {
                    if writable_schema {
                        continue;
                    }
                    return Err(corrupt_schema_ddl_err(&trig_name, &sql_text, None));
                }
            };

            // `fixExprCb` (`attach.c:484-485`): bound variables in stored catalog triggers become `NULL`.
            replace_bind_params_with_null_in_trigger(&mut ct.when_clause, &mut ct.body);

            if let Err(e) = validate_trigger_body_dml_targets(&ct.body) {
                if writable_schema {
                    continue;
                }
                return Err(SqliteError::corrupt(format!(
                    "malformed database schema ({trig_name}) - {}",
                    e.message
                )));
            }

            if !writable_schema && is_reserved_trigger_or_system_name(&trig_name) {
                return Err(SqliteError::corrupt(format!(
                    "malformed database schema ({trig_name}) - object name reserved for internal use: {trig_name}"
                )));
            }

            if triggers
                .iter()
                .any(|t| t.is_active() && t.name.eq_ignore_ascii_case(&trig_name))
            {
                if writable_schema {
                    continue;
                }
                return Err(SqliteError::corrupt(format!(
                    "malformed database schema ({trig_name}) - trigger {} already exists",
                    ct.name_token_text
                )));
            }

            if !is_temp_schema {
                // `trigger.c:155-160`: ignore any schema prefix on `ON <schema>.<table>` when loading a permanent trigger.
                ct.on_table.schema = None;
                let sname_disp = db_schema_name.unwrap_or("main");
                let target_is_table = tables
                    .iter()
                    .any(|t| t.name.eq_ignore_ascii_case(&ct.on_table.name));
                let target_is_view = !target_is_table
                    && views
                        .iter()
                        .any(|v| v.name.eq_ignore_ascii_case(&ct.on_table.name));
                if !target_is_table && !target_is_view {
                    if writable_schema {
                        continue;
                    }
                    return Err(SqliteError::corrupt(format!(
                        "malformed database schema ({trig_name}) - no such table: {sname_disp}.{}",
                        ct.on_table.name
                    )));
                }
                if is_reserved_trigger_or_system_name(&ct.on_table.name) {
                    if writable_schema {
                        continue;
                    }
                    return Err(SqliteError::corrupt(format!(
                        "malformed database schema ({trig_name}) - cannot create trigger on system table"
                    )));
                }
                if target_is_view && ct.timing != TriggerTiming::InsteadOf {
                    if writable_schema {
                        continue;
                    }
                    let tm_str = if ct.timing == TriggerTiming::Before {
                        "BEFORE"
                    } else {
                        "AFTER"
                    };
                    return Err(SqliteError::corrupt(format!(
                        "malformed database schema ({trig_name}) - cannot create {tm_str} trigger on view: {}",
                        ct.on_table.name
                    )));
                }
                if target_is_table && ct.timing == TriggerTiming::InsteadOf {
                    if writable_schema {
                        continue;
                    }
                    return Err(SqliteError::corrupt(format!(
                        "malformed database schema ({trig_name}) - cannot create INSTEAD OF trigger on table: {}",
                        ct.on_table.name
                    )));
                }

                let (fixed_when, fixed_body) = if let Some(sname) = db_schema_name {
                    match fix_permanent_trigger(
                        &trig_name,
                        sname,
                        ct.when_clause.as_ref(),
                        &ct.body,
                    ) {
                        Ok(pair) => pair,
                        Err(e) => {
                            if writable_schema {
                                continue;
                            }
                            return Err(SqliteError::corrupt(format!(
                                "malformed database schema ({trig_name}) - {}",
                                e.message
                            )));
                        }
                    }
                } else {
                    (ct.when_clause, ct.body)
                };

                let effective_tbl = if tbl_name_col.is_empty() {
                    ct.on_table.name
                } else {
                    tbl_name_col
                };
                triggers.push(TriggerSchema {
                    schema_rowid: rowid,
                    name: trig_name,
                    tbl_name: effective_tbl,
                    on_table_schema: None,
                    target_schema_instance_id: None,
                    detached_to_temp: false,
                    is_orphan: false,
                    timing: ct.timing,
                    event: ct.event,
                    when_clause: fixed_when,
                    body: fixed_body,
                    raw_sql: sql_text,
                });
            } else {
                // `TEMP` trigger: target table/view may reside in `temp`, `main`, or an `attached` DB
                // (or may have been dropped by another connection -> `orphanTrigger`).
                let effective_tbl = if tbl_name_col.is_empty() {
                    ct.on_table.name
                } else {
                    tbl_name_col
                };
                triggers.push(TriggerSchema {
                    schema_rowid: rowid,
                    name: trig_name,
                    tbl_name: effective_tbl,
                    on_table_schema: ct.on_table.schema,
                    target_schema_instance_id: None,
                    detached_to_temp: false,
                    is_orphan: false,
                    timing: ct.timing,
                    event: ct.event,
                    when_clause: ct.when_clause,
                    body: ct.body,
                    raw_sql: sql_text,
                });
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
                let ci = match parse_one(&sql_text) {
                    Ok((Some(Stmt::CreateIndex(ci)), tail)) if tail.trim().is_empty() => ci,
                    Err(e) => {
                        return Err(corrupt_schema_ddl_err(
                            &idx_name,
                            &sql_text,
                            Some(&e.message),
                        ));
                    }
                    _ => {
                        return Err(corrupt_schema_ddl_err(&idx_name, &sql_text, None));
                    }
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
                        stat1: None,
                    });
                }
            }
        }

        let mut schema = Self::from_data(SchemaData {
            tables,
            virtual_tables,
            indexes,
            views,
            triggers,
            schema_cookie: btree.pager().header().schema_cookie,
            loaded: true,
            had_pages: true,
        });
        let _ = schema.reload_stat1(btree);
        Ok(schema)
    }

    /// Reloads `sqlite_stat1` statistics from disk into `self.tables` and `self.indexes`
    /// (`sqlite3AnalysisLoad` / `analysisLoader` in `sqlite/src/analyze.c:1593-1701`).
    pub fn reload_stat1(&mut self, btree: &mut Btree) -> Result<()> {
        let stat_tbl_opt = self
            .tables
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case("sqlite_stat1"))
            .cloned();
        let any_had_stat1 = self
            .tables
            .iter()
            .any(|t| t.stat1.has_stat1 || t.without_rowid_pk_stat1.is_some())
            || self.indexes.iter().any(|i| i.stat1.is_some());
        if stat_tbl_opt.is_none() && !any_had_stat1 {
            return Ok(());
        }

        let data = Arc::make_mut(&mut self.0);
        for tbl in &mut data.tables {
            tbl.stat1 = TableStat1::default();
            tbl.without_rowid_pk_stat1 = None;
        }
        for idx in &mut data.indexes {
            idx.stat1 = None;
        }

        let Some(stat_tbl) = stat_tbl_opt else {
            return Ok(());
        };
        if stat_tbl.root_pgno <= 1 {
            return Ok(());
        }
        let (Some((tbl_ci, _)), Some((idx_ci, _)), Some((stat_ci, _))) = (
            stat_tbl.find_column("tbl"),
            stat_tbl.find_column("idx"),
            stat_tbl.find_column("stat"),
        ) else {
            return Ok(());
        };
        let tbl_slot = stat_tbl.physical_record_slot(tbl_ci);
        let idx_slot = stat_tbl.physical_record_slot(idx_ci);
        let stat_slot = stat_tbl.physical_record_slot(stat_ci);

        let enc = btree.encoding();
        let payloads: Vec<Vec<u8>> = if stat_tbl.without_rowid {
            match btree.scan_index(stat_tbl.root_pgno) {
                Ok(keys) => keys,
                Err(_) => return Ok(()),
            }
        } else {
            match scan_table_incremental(btree, stat_tbl.root_pgno) {
                Ok(rows) => rows.into_iter().map(|(_, p)| p).collect(),
                Err(_) => return Ok(()),
            }
        };

        let val_to_stat_text = |v: &Value| -> Option<String> {
            match v {
                Value::Null => None,
                Value::Text(s) => Some(s.clone()),
                Value::Integer(i) => Some(i.to_string()),
                Value::Real(f) => Some(Value::Real(*f).to_text_lossy()),
                Value::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
            }
        };

        for payload in payloads {
            let Ok(cols) = decode_record_with_encoding(&payload, enc) else {
                continue;
            };
            let tbl_val = cols.get(tbl_slot).unwrap_or(&Value::Null);
            let idx_val = cols.get(idx_slot).unwrap_or(&Value::Null);
            let stat_val = cols.get(stat_slot).unwrap_or(&Value::Null);

            let (Some(tbl_str), Some(stat_str)) =
                (val_to_stat_text(tbl_val), val_to_stat_text(stat_val))
            else {
                continue;
            };
            let Some(tbl_pos) = self
                .tables
                .iter()
                .position(|t| t.name.eq_ignore_ascii_case(&tbl_str))
            else {
                continue;
            };

            let idx_opt = val_to_stat_text(idx_val);
            match idx_opt {
                None => {
                    let decoded = decode_stat1_string(&stat_str, 1);
                    self.tables[tbl_pos].stat1.n_rows = decoded.int_values[0];
                    self.tables[tbl_pos].stat1.n_row_log_est = decoded.row_log_est[0];
                    if let Some(sz) = decoded.sz_idx_row {
                        self.tables[tbl_pos].stat1.sz_tab_row = Some(sz);
                    }
                    self.tables[tbl_pos].stat1.has_stat1 = true;
                }
                Some(idx_str) => {
                    if tbl_str.eq_ignore_ascii_case(&idx_str) {
                        if self.tables[tbl_pos].without_rowid {
                            let n_out = self.tables[tbl_pos].without_rowid_pk_cols.len() + 1;
                            let decoded = decode_stat1_string(&stat_str, n_out);
                            self.tables[tbl_pos].stat1.n_rows = decoded.int_values[0];
                            self.tables[tbl_pos].stat1.n_row_log_est = decoded.row_log_est[0];
                            self.tables[tbl_pos].stat1.has_stat1 = true;
                            self.tables[tbl_pos].without_rowid_pk_stat1 = Some(decoded);
                        } else if let Some(pk_ix_pos) = self.indexes.iter().position(|ix| {
                            ix.table_name.eq_ignore_ascii_case(&tbl_str) && ix.origin == "pk"
                        }) {
                            let n_out = self.indexes[pk_ix_pos].columns.len() + 1;
                            let decoded = decode_stat1_string(&stat_str, n_out);
                            if self.indexes[pk_ix_pos].where_clause.is_none() {
                                self.tables[tbl_pos].stat1.n_rows = decoded.int_values[0];
                                self.tables[tbl_pos].stat1.n_row_log_est = decoded.row_log_est[0];
                                self.tables[tbl_pos].stat1.has_stat1 = true;
                            }
                            self.indexes[pk_ix_pos].stat1 = Some(decoded);
                        }
                    } else if let Some(ix_pos) = self
                        .indexes
                        .iter()
                        .position(|ix| ix.name.eq_ignore_ascii_case(&idx_str))
                    {
                        let n_out = self.indexes[ix_pos].columns.len() + 1;
                        let decoded = decode_stat1_string(&stat_str, n_out);
                        if self.indexes[ix_pos].where_clause.is_none() {
                            self.tables[tbl_pos].stat1.n_rows = decoded.int_values[0];
                            self.tables[tbl_pos].stat1.n_row_log_est = decoded.row_log_est[0];
                            self.tables[tbl_pos].stat1.has_stat1 = true;
                        }
                        self.indexes[ix_pos].stat1 = Some(decoded);
                    }
                }
            }
        }

        Ok(())
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
        let mut custom_collation: Option<String> = None;
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
                    reject_window_in_ddl_expr(expr)?;
                    default_expr = Some(expr.clone());
                }
                ColumnConstraint::Check {
                    name: cname,
                    expr_sql,
                    expr,
                } => {
                    reject_window_in_ddl_expr(expr)?;
                    checks.push(CheckSchema {
                        label: cname.clone().unwrap_or_else(|| expr_sql.clone()),
                        expr: expr.clone(),
                    });
                }
                ColumnConstraint::Generated { expr: e, stored: s } => {
                    reject_raise_outside_trigger_expr(e)?;
                    reject_window_in_ddl_expr(e)?;
                    generated = Some((e.clone(), *s));
                }
                ColumnConstraint::Collate(cname) => {
                    if let Some(c) = Collation::from_name(cname) {
                        collation = c;
                        custom_collation = None;
                    } else {
                        collation = Collation::Binary;
                        custom_collation = Some(cname.clone());
                    }
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
            custom_collation: custom_collation.clone(),
            not_null,
            default_src: if default_expr.is_some() {
                cdef.default_src.clone()
            } else {
                None
            },
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
                            custom_collation: custom_collation
                                .as_deref()
                                .map(CustomCollation::unbound),
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
                        custom_collation: custom_collation.as_deref().map(CustomCollation::unbound),
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
                reject_window_in_ddl_expr(expr)?;
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
                prev.col_idx == ic.col_idx
                    && prev
                        .sort_order
                        .collation_name()
                        .eq_ignore_ascii_case(ic.sort_order.collation_name())
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
                        && ic
                            .sort_order
                            .collation_name()
                            .eq_ignore_ascii_case(pk_ic.sort_order.collation_name())
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
        stat1: TableStat1::default(),
        without_rowid_pk_stat1: None,
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
                a.col_idx == b.col_idx
                    && a.sort_order
                        .collation_name()
                        .eq_ignore_ascii_case(b.sort_order.collation_name())
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
    for ic in &ci.columns {
        if let Some(ref ie) = ic.expr {
            reject_raise_outside_trigger_expr(ie)?;
            reject_window_in_ddl_expr(ie)?;
        }
    }
    if let Some(ref wc) = ci.where_clause {
        reject_raise_outside_trigger_expr(wc)?;
        reject_window_in_ddl_expr(wc)?;
    }
    let resolved = resolve_indexed_columns(&tbl.columns, &ci.columns, true)?;
    let mut trailing_pk_cols = Vec::new();
    if tbl.without_rowid {
        // Per `sqlite3CreateIndex` (`sqlite/src/build.c:4359-4372`): append non-duplicate PK columns
        // preserving `pPk->aSortOrder[j]`!
        for pk_ic in &tbl.without_rowid_pk_cols {
            let is_dup = resolved.iter().any(|ic| {
                ic.col_idx == pk_ic.col_idx
                    && ic
                        .sort_order
                        .collation_name()
                        .eq_ignore_ascii_case(pk_ic.sort_order.collation_name())
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
        where_clause: ci.where_clause.as_ref().map(strip_schema_from_expr),
        is_autoindex: false,
        origin: "c",
        raw_sql: Some(ci.catalog_sql.clone()),
        table_without_rowid: tbl.without_rowid,
        trailing_pk_cols,
        stat1: None,
    })
}

fn strip_schema_from_expr(expr: &Expr) -> Expr {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::OversizedHex(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => expr.clone(),
        Expr::Column(ColumnRef {
            table,
            column,
            double_quoted,
            ..
        }) => Expr::Column(ColumnRef {
            schema: None,
            table: table.clone(),
            column: column.clone(),
            double_quoted: *double_quoted,
        }),
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(strip_schema_from_expr(inner)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(strip_schema_from_expr(left)),
            op: *op,
            right: Box::new(strip_schema_from_expr(right)),
        },
        Expr::Between {
            expr: e,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(strip_schema_from_expr(e)),
            low: Box::new(strip_schema_from_expr(low)),
            high: Box::new(strip_schema_from_expr(high)),
            negated: *negated,
        },
        Expr::InList {
            expr: e,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(strip_schema_from_expr(e)),
            list: list.iter().map(strip_schema_from_expr).collect(),
            negated: *negated,
        },
        Expr::Like {
            expr: e,
            pattern,
            escape,
            op,
            negated,
        } => Expr::Like {
            expr: Box::new(strip_schema_from_expr(e)),
            pattern: Box::new(strip_schema_from_expr(pattern)),
            escape: escape
                .as_ref()
                .map(|esc| Box::new(strip_schema_from_expr(esc))),
            op: *op,
            negated: *negated,
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(strip_schema_from_expr(inner)),
            negated: *negated,
        },
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(strip_schema_from_expr(inner)),
            collation: collation.clone(),
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(strip_schema_from_expr(inner)),
            type_name: type_name.clone(),
        },
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => Expr::Case {
            operand: operand
                .as_ref()
                .map(|op| Box::new(strip_schema_from_expr(op))),
            branches: branches
                .iter()
                .map(|(w, t)| (strip_schema_from_expr(w), strip_schema_from_expr(t)))
                .collect(),
            else_expr: else_expr
                .as_ref()
                .map(|el| Box::new(strip_schema_from_expr(el))),
        },
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
            over,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => {
                    FunctionArgs::Exprs(es.iter().map(strip_schema_from_expr).collect())
                }
            },
            filter: filter.as_ref().map(|f| Box::new(strip_schema_from_expr(f))),
            over: over.clone(),
        },
        Expr::RowValue(es) => Expr::RowValue(es.iter().map(strip_schema_from_expr).collect()),
        _ => expr.clone(),
    }
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
            let (collation, custom_collation) = match &ic.collation {
                Some(cname) => match Collation::from_name(cname) {
                    Some(c) => (c, None),
                    None => (
                        Collation::Binary,
                        Some(CustomCollation::unbound(cname.as_str())),
                    ),
                },
                None => (Collation::Binary, None),
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
                    custom_collation,
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
        let (collation, custom_collation) = match &ic.collation {
            Some(cname) => match Collation::from_name(cname) {
                Some(c) => (c, None),
                None => (
                    Collation::Binary,
                    Some(CustomCollation::unbound(cname.as_str())),
                ),
            },
            None => (
                col_schema.collation,
                col_schema
                    .custom_collation
                    .as_deref()
                    .map(CustomCollation::unbound),
            ),
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
                custom_collation,
            },
        });
    }
    Ok(out)
}

fn rebind_indexed_col_collation(ic: &mut IndexedColSchema, collations: &CollationRegistry) {
    if let Some(ref cc) = ic.sort_order.custom_collation {
        let name = cc.name.to_string();
        ic.sort_order = collations.resolve_key_sort_order_or_unbound(
            &name,
            ic.sort_order.desc,
            ic.sort_order.nulls_last,
        );
    }
}

pub fn rebind_table_collations(tbl: &mut TableSchema, collations: &CollationRegistry) {
    for ic in &mut tbl.without_rowid_pk_cols {
        rebind_indexed_col_collation(ic, collations);
    }
}

pub fn rebind_pending_autoindex_collations(
    auto: &mut PendingAutoIndex,
    collations: &CollationRegistry,
) {
    for ic in &mut auto.columns {
        rebind_indexed_col_collation(ic, collations);
    }
    for ic in &mut auto.trailing_pk_cols {
        rebind_indexed_col_collation(ic, collations);
    }
}

pub fn rebind_index_collations(idx: &mut IndexSchema, collations: &CollationRegistry) {
    for ic in &mut idx.columns {
        rebind_indexed_col_collation(ic, collations);
    }
    for ic in &mut idx.trailing_pk_cols {
        rebind_indexed_col_collation(ic, collations);
    }
}

pub fn validate_table_collations(
    tbl: &TableSchema,
    autos: &[PendingAutoIndex],
    collations: &CollationRegistry,
) -> Result<()> {
    for col in &tbl.columns {
        if let Some(ref cname) = col.custom_collation
            && !collations.has_collation(cname)
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {cname}"),
            ));
        }
    }
    for ic in &tbl.without_rowid_pk_cols {
        if let Some(ref cc) = ic.sort_order.custom_collation
            && !collations.has_collation(&cc.name)
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {}", cc.name),
            ));
        }
    }
    for auto in autos {
        for ic in auto.columns.iter().chain(auto.trailing_pk_cols.iter()) {
            if let Some(ref cc) = ic.sort_order.custom_collation
                && !collations.has_collation(&cc.name)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such collation sequence: {}", cc.name),
                ));
            }
        }
    }
    Ok(())
}

pub fn validate_index_collations(idx: &IndexSchema, collations: &CollationRegistry) -> Result<()> {
    for ic in idx.columns.iter().chain(idx.trailing_pk_cols.iter()) {
        if let Some(ref cc) = ic.sort_order.custom_collation
            && !collations.has_collation(&cc.name)
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {}", cc.name),
            ));
        }
    }
    Ok(())
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

/// `strHash` of `hash.c`: case-insensitive Knuth multiplicative hash of a schema object name.
fn c_str_hash(key: &str) -> u32 {
    let mut h: u32 = 0;
    for &b in key.as_bytes() {
        h = h.wrapping_add(u32::from(b & 0xdf));
        h = h.wrapping_mul(0x9e37_79b1);
    }
    h
}

/// Order in which C iterates a `Hash` (`hash.c`: `sqliteHashFirst` / `sqliteHashNext`) after
/// `keys` were inserted in the given order with `sqlite3HashInsert`. Returns indices into `keys`.
///
/// Without a bucket array (fewer than 5 keys) every insert goes to the front of the element
/// list; with one, a key is linked in front of the current head of its bucket, and every
/// `rehash` (when the count reaches 5 and exceeds twice the bucket count; new size `count * 3`,
/// at most `SQLITE_MALLOC_SOFT_LIMIT / sizeof(struct _ht)` = 64) relinks the whole list.
pub(crate) fn c_hash_iteration_order(keys: &[&str]) -> Vec<usize> {
    const NONE: usize = usize::MAX;
    const MAX_BUCKETS: usize = 1024 / 16;
    struct Elem {
        h: u32,
        prev: usize,
        next: usize,
    }
    struct Bucket {
        count: u32,
        chain: usize,
    }
    fn insert_element(
        elems: &mut [Elem],
        first: &mut usize,
        bucket: Option<&mut Bucket>,
        new: usize,
    ) {
        let head = match bucket {
            Some(b) => {
                let head = if b.count != 0 { b.chain } else { NONE };
                b.count += 1;
                b.chain = new;
                head
            }
            None => NONE,
        };
        if head != NONE {
            let head_prev = elems[head].prev;
            elems[new].next = head;
            elems[new].prev = head_prev;
            if head_prev != NONE {
                elems[head_prev].next = new;
            } else {
                *first = new;
            }
            elems[head].prev = new;
        } else {
            elems[new].next = *first;
            if *first != NONE {
                elems[*first].prev = new;
            }
            elems[new].prev = NONE;
            *first = new;
        }
    }

    let mut elems: Vec<Elem> = Vec::with_capacity(keys.len());
    let mut first = NONE;
    let mut ht: Vec<Bucket> = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        elems.push(Elem {
            h: c_str_hash(key),
            prev: NONE,
            next: NONE,
        });
        let count = i + 1;
        if count >= 5 && count > 2 * ht.len() {
            let new_size = (count * 3).min(MAX_BUCKETS);
            if new_size != ht.len() {
                ht = (0..new_size)
                    .map(|_| Bucket {
                        count: 0,
                        chain: NONE,
                    })
                    .collect();
                let mut elem = first;
                first = NONE;
                while elem != NONE {
                    let next = elems[elem].next;
                    let slot = elems[elem].h as usize % new_size;
                    insert_element(&mut elems, &mut first, Some(&mut ht[slot]), elem);
                    elem = next;
                }
            }
        }
        let bucket = if ht.is_empty() {
            None
        } else {
            let slot = elems[i].h as usize % ht.len();
            Some(&mut ht[slot])
        };
        insert_element(&mut elems, &mut first, bucket, i);
    }
    let mut out = Vec::with_capacity(keys.len());
    let mut elem = first;
    while elem != NONE {
        out.push(elem);
        elem = elems[elem].next;
    }
    out
}

/// The rowid tables of `schema` in the order C's `analyzeDatabase` visits them: the iteration
/// order of the schema's table hash, which `sqlite3InitOne` fills with the schema table itself
/// and then every table, view and virtual table in `sqlite_schema` rowid order.
/// `include_stat1 == false` leaves out an `sqlite_stat1` that the running `ANALYZE` created.
///
/// The hash of a C connection also remembers its history (tables created and dropped since the
/// schema was last loaded); this reproduces a freshly loaded schema, which is what a connection
/// has unless it dropped tables since.
pub(crate) fn c_table_hash_order(
    schema: &Schema,
    is_temp: bool,
    include_stat1: bool,
) -> Vec<TableSchema> {
    let mut objs: Vec<(i64, &str, Option<&TableSchema>)> = Vec::new();
    for t in &schema.tables {
        if !include_stat1 && t.name.eq_ignore_ascii_case("sqlite_stat1") {
            continue;
        }
        objs.push((t.schema_rowid, t.name.as_str(), Some(t)));
    }
    for v in &schema.views {
        objs.push((v.schema_rowid, v.name.as_str(), None));
    }
    for v in &schema.virtual_tables {
        objs.push((v.schema_rowid, v.name.as_str(), None));
    }
    objs.sort_by_key(|o| o.0);
    let mut keys: Vec<&str> = Vec::with_capacity(objs.len() + 1);
    keys.push(if is_temp {
        "sqlite_temp_master"
    } else {
        "sqlite_master"
    });
    keys.extend(objs.iter().map(|o| o.1));
    c_hash_iteration_order(&keys)
        .into_iter()
        .filter_map(|i| i.checked_sub(1).and_then(|j| objs[j].2).cloned())
        .collect()
}

/// The placeholder row `sqlite3StartTable` writes for a table being created (`OP_Blob 6
/// nullRow`: a record of five NULLs); [`update_schema_entry`] replaces it with the real row.
/// Does not touch the schema cookie.
pub fn insert_schema_placeholder(btree: &mut Btree) -> Result<i64> {
    let rowid = btree.next_rowid(1)?;
    btree.insert_table(1, rowid, &[6, 0, 0, 0, 0, 0])?;
    Ok(rowid)
}

/// [`insert_schema_entry`] the way C writes the row of a view or virtual table: the placeholder
/// of `sqlite3StartTable` first, then the real row over it (`sqlite3EndTable` /
/// `sqlite3VtabFinishParse`).
pub fn insert_schema_entry_over_placeholder(
    btree: &mut Btree,
    obj_type: &str,
    name: &str,
    tbl_name: &str,
    rootpage: Pgno,
    sql: Option<&str>,
) -> Result<i64> {
    let rowid = insert_schema_placeholder(btree)?;
    update_schema_entry(btree, rowid, obj_type, name, tbl_name, rootpage, sql)?;
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

/// Deletes a catalogue row without changing the schema cookie (the caller changes it where C
/// codes its `OP_SetCookie`).
pub fn delete_schema_row(btree: &mut Btree, schema_rowid: i64) -> Result<()> {
    btree.delete_table(1, schema_rowid)?;
    Ok(())
}

pub fn delete_schema_entry(btree: &mut Btree, schema_rowid: i64) -> Result<()> {
    btree.delete_table(1, schema_rowid)?;
    bump_schema_cookie(btree)
}

pub fn bump_schema_cookie(btree: &mut Btree) -> Result<()> {
    btree.change_schema_cookie()?;
    let hdr = btree.pager_mut().header_mut()?;
    // The first schema row of a database is a table, view or virtual table: this is where C
    // records the schema format and the text encoding in the header (`sqlite3StartTable`).
    hdr.set_file_format_for_first_table();
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
            windows,
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
            for (_, ws) in windows {
                validate_window_spec_no_params(ws)?;
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
    match tos {
        TableOrSubquery::Table {
            table_args: Some(args),
            ..
        }
        | TableOrSubquery::TableFunction { args, .. } => {
            for arg in args {
                validate_expr_no_params(arg)?;
            }
        }
        TableOrSubquery::Table {
            table_args: None, ..
        } => {}
        TableOrSubquery::Subquery { query, .. } => {
            validate_view_no_parameters(query)?;
        }
    }
    Ok(())
}

fn validate_window_spec_no_params(ws: &WindowSpec) -> Result<()> {
    for pe in &ws.partition_by {
        validate_expr_no_params(pe)?;
    }
    for ob in &ws.order_by {
        validate_expr_no_params(&ob.expr)?;
    }
    if let Some(ref wf) = ws.frame {
        for b in [&wf.start, &wf.end] {
            if let WindowFrameBound::Preceding(e) | WindowFrameBound::Following(e) = b
                && is_window_bound_constant(e)
            {
                validate_expr_no_params(e)?;
            }
        }
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
        | Expr::OversizedHex(_)
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
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    validate_expr_no_params(e)?;
                }
            }
            if let Some(f) = filter {
                validate_expr_no_params(f)?;
            }
            if let Some(WindowRef::Spec(ws)) = over {
                validate_window_spec_no_params(ws)?;
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
            windows,
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
                    ResultColumn::Expr {
                        expr,
                        alias,
                        span_text,
                    } => {
                        fixed_proj.push(ResultColumn::Expr {
                            expr: fix_expr_with_ctes(
                                view_name,
                                db_schema_name,
                                expr,
                                in_scope_ctes,
                            )?,
                            alias: alias.clone(),
                            span_text: span_text.clone(),
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
            let mut fixed_windows = Vec::with_capacity(windows.len());
            for (wname, wspec) in windows {
                fixed_windows.push((
                    wname.clone(),
                    fix_window_spec_with_ctes(view_name, db_schema_name, wspec, in_scope_ctes)?,
                ));
            }
            Ok(SelectCore::Select {
                distinct: *distinct,
                projection: fixed_proj,
                from: fixed_from,
                selection: fixed_sel,
                group_by: fixed_gb,
                having: fixed_hav,
                windows: fixed_windows,
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
            table_args,
            alias,
            indexed_by,
        } => {
            let fixed_args = match table_args {
                Some(args) => {
                    let mut out = Vec::with_capacity(args.len());
                    for e in args {
                        out.push(fix_expr_with_ctes(
                            view_name,
                            db_schema_name,
                            e,
                            in_scope_ctes,
                        )?);
                    }
                    Some(out)
                }
                None => None,
            };
            Ok(TableOrSubquery::Table {
                name: fix_qualified_relation(view_name, db_schema_name, name, in_scope_ctes)?,
                table_args: fixed_args,
                alias: alias.clone(),
                indexed_by: indexed_by.clone(),
            })
        }
        TableOrSubquery::TableFunction {
            schema,
            name,
            args,
            alias,
        } => {
            if let Some(s) = schema {
                check_allowed_schema(view_name, db_schema_name, s)?;
            }
            let mut fixed_args = Vec::with_capacity(args.len());
            for e in args {
                fixed_args.push(fix_expr_with_ctes(
                    view_name,
                    db_schema_name,
                    e,
                    in_scope_ctes,
                )?);
            }
            Ok(TableOrSubquery::TableFunction {
                schema: Some(db_schema_name.to_owned()),
                name: name.clone(),
                args: fixed_args,
                alias: alias.clone(),
            })
        }
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
        | Expr::OversizedHex(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => Ok(expr.clone()),
        Expr::Column(ColumnRef {
            schema,
            table,
            column,
            double_quoted,
        }) => {
            if let Some(s) = schema {
                check_allowed_schema(view_name, db_schema_name, s)?;
            }
            Ok(Expr::Column(ColumnRef {
                schema: schema.clone(),
                table: table.clone(),
                column: column.clone(),
                double_quoted: *double_quoted,
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
            over,
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
            let fixed_over = match over {
                Some(WindowRef::Name(wn)) => Some(WindowRef::Name(wn.clone())),
                Some(WindowRef::Spec(ws)) => Some(WindowRef::Spec(Box::new(
                    fix_window_spec_with_ctes(view_name, db_schema_name, ws, in_scope_ctes)?,
                ))),
                None => None,
            };
            Ok(Expr::FunctionCall {
                name: name.clone(),
                distinct: *distinct,
                args: fixed_args,
                filter: fixed_filter,
                over: fixed_over,
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

fn fix_window_bound_with_ctes(
    view_name: &str,
    db_schema_name: &str,
    bound: &WindowFrameBound,
    in_scope_ctes: &[String],
) -> Result<WindowFrameBound> {
    match bound {
        WindowFrameBound::UnboundedPreceding => Ok(WindowFrameBound::UnboundedPreceding),
        WindowFrameBound::CurrentRow => Ok(WindowFrameBound::CurrentRow),
        WindowFrameBound::UnboundedFollowing => Ok(WindowFrameBound::UnboundedFollowing),
        WindowFrameBound::Preceding(e) => Ok(WindowFrameBound::Preceding(Box::new(
            fix_expr_with_ctes(view_name, db_schema_name, e, in_scope_ctes)?,
        ))),
        WindowFrameBound::Following(e) => Ok(WindowFrameBound::Following(Box::new(
            fix_expr_with_ctes(view_name, db_schema_name, e, in_scope_ctes)?,
        ))),
    }
}

fn fix_window_spec_with_ctes(
    view_name: &str,
    db_schema_name: &str,
    ws: &WindowSpec,
    in_scope_ctes: &[String],
) -> Result<WindowSpec> {
    let mut part = Vec::with_capacity(ws.partition_by.len());
    for pe in &ws.partition_by {
        part.push(fix_expr_with_ctes(
            view_name,
            db_schema_name,
            pe,
            in_scope_ctes,
        )?);
    }
    let mut ob_out = Vec::with_capacity(ws.order_by.len());
    for ob in &ws.order_by {
        ob_out.push(OrderByTerm {
            expr: fix_expr_with_ctes(view_name, db_schema_name, &ob.expr, in_scope_ctes)?,
            desc: ob.desc,
            nulls_last: ob.nulls_last,
        });
    }
    let frame = match &ws.frame {
        Some(f) => Some(WindowFrame {
            units: f.units,
            start: fix_window_bound_with_ctes(view_name, db_schema_name, &f.start, in_scope_ctes)?,
            end: fix_window_bound_with_ctes(view_name, db_schema_name, &f.end, in_scope_ctes)?,
            exclude: f.exclude,
        }),
        None => None,
    };
    Ok(WindowSpec {
        base_window: ws.base_window.clone(),
        partition_by: part,
        order_by: ob_out,
        frame,
    })
}
