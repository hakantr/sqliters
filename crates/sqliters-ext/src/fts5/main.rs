//! Cursor side of `ext/fts5/fts5_main.c`: `Fts5Cursor` (here `CsrCore`), the query plans of
//! `fts5FilterMethod`, `fts5NextMethod`, the sorter of `ORDER BY rank`, the `rank` column
//! and the extension API (`Fts5ExtensionApi`) with its per-row caches. Function names are
//! the C names in snake case.
#![forbid(unsafe_code)]

use std::any::Any;
use std::collections::VecDeque;

use super::expr::{Fts5Expr, PoslistPopulator};
use super::*;
use crate::fts5_index::{PoslistReader, get_varint32, rc_only};

pub(super) const FTS5_PLAN_MATCH: i32 = 1;
pub(super) const FTS5_PLAN_SOURCE: i32 = 2;
pub(super) const FTS5_PLAN_SPECIAL: i32 = 3;
pub(super) const FTS5_PLAN_SORTED_MATCH: i32 = 4;
pub(super) const FTS5_PLAN_SCAN: i32 = 5;
pub(super) const FTS5_PLAN_ROWID: i32 = 6;

const FTS5CSR_EOF: i32 = 0x01;
const FTS5CSR_REQUIRE_CONTENT: i32 = 0x02;
const FTS5CSR_REQUIRE_DOCSIZE: i32 = 0x04;
const FTS5CSR_REQUIRE_INST: i32 = 0x08;
const FTS5CSR_REQUIRE_RESEEK: i32 = 0x20;
const FTS5CSR_REQUIRE_POSLIST: i32 = 0x40;

const FTS5_DEFAULT_RANK: &str = "bm25";

/// The expression of a cursor. An `ORDER BY rank` cursor and the cursor of its sorter
/// statement (`FTS5_PLAN_SOURCE`) use the same object.
pub(super) type ExprSlot = Arc<Mutex<Option<Fts5Expr>>>;

/// `Fts5FullTable.pSortCsr` as the source cursor sees it.
#[derive(Debug, Clone)]
pub(super) struct SortCsr {
    pub(super) expr: ExprSlot,
    pub(super) b_desc: bool,
    pub(super) i_first_rowid: i64,
    pub(super) i_last_rowid: i64,
}

/// `Fts5Sorter`: the rows of `SELECT rowid, rank FROM ft ORDER BY <rank>(ft)` still to
/// come and the current one.
#[derive(Debug, Default)]
pub(super) struct Sorter {
    rows: VecDeque<(i64, Vec<u8>)>,
    i_rowid: i64,
    /// `aPoslist`: the position lists of the current row (the blob after its varints).
    a_poslist: Vec<u8>,
    /// `aIdx[]`: the end offset of each phrase's list within `a_poslist`.
    a_idx: Vec<i32>,
    /// Only for the replay of a row saved from a live cursor (never set on the sorter of
    /// ORDER BY rank): per phrase, whether `sqlite3Fts5ExprClearPoslists(pExpr, 1)` marked
    /// it `bMiss` on that row, so that detail=col / detail=none lists are rebuilt for the
    /// phrases the live cursor had, not for every phrase found in the text.
    live_miss: Option<Vec<bool>>,
}

/// `pCsr->pStmt`: the statement that reads the content source. A row has the layout of
/// `zContentExprlist`: rowid, one value per column, then the locale columns.
#[derive(Debug)]
pub(super) enum ContentStmt {
    /// `FTS5_STMT_LOOKUP`: the row of the last successful step, if any.
    Lookup(Option<Vec<Value>>),
    /// `FTS5_STMT_SCAN_ASC` / `_DESC`.
    Scan {
        rows: VecDeque<Vec<Value>>,
        cur: Option<Vec<Value>>,
    },
}

/// `Fts5Auxdata`.
struct Auxdata {
    aux: &'static str,
    ptr: Option<Box<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for Auxdata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auxdata").field("aux", &self.aux).finish()
    }
}

/// `Fts5Cursor`.
#[derive(Debug)]
pub(super) struct CsrCore {
    pub(super) cfg: Arc<Fts5Config>,
    pub(super) state: Arc<Mutex<Fts5SharedState>>,
    pub(super) aux_registry: Arc<Fts5AuxRegistry>,
    pub(super) db_name: String,
    pub(super) tbl_name: String,
    /// `aColumnSize[]`.
    a_column_size: Vec<i32>,
    pub(super) i_csr_id: i64,
    // Zero from this point onwards on cursor reset
    pub(super) e_plan: i32,
    b_desc: bool,
    i_first_rowid: i64,
    i_last_rowid: i64,
    stmt: Option<ContentStmt>,
    pub(super) expr: Option<ExprSlot>,
    sorter: Option<Sorter>,
    csrflags: i32,
    i_special: i64,
    z_rank: Option<String>,
    z_rank_args: Option<String>,
    /// `pRank` (the name of the auxiliary function) and `apRankArg[]`.
    p_rank: Option<&'static str>,
    ap_rank_arg: Vec<Value>,
    /// `pAux`: the auxiliary function being executed.
    p_aux: Option<&'static str>,
    auxdata: Vec<Auxdata>,
    /// `nInstCount` / `aInst[]` (three integers per instance).
    n_inst_count: i32,
    a_inst: Vec<i32>,
    /// A position-list snapshot of a row being replayed (the sorter the functions see).
    replay: Option<Sorter>,
    /// `%_docsize` of the table and where the host found it last time.
    docsize_tbl: String,
    docsize_hint: sqliters_types::ShadowHint,
}

fn csr_err(rc: i32) -> SqliteError {
    let e = SqliteError::with_extended(rc, "");
    rc_only(e)
}

fn range_err() -> SqliteError {
    SqliteError::new(ErrorCode::Range, ErrorCode::Range.as_str())
}

fn corrupt_err() -> SqliteError {
    SqliteError::with_extended(FTS5_CORRUPT, "database disk image is malformed")
}

/// `sqlite3Fts5ConfigParseRank`: (zRank, zRankArgs).
pub(super) fn config_parse_rank(z: &[u8]) -> Option<(String, Option<String>)> {
    let p = cfg_skip_ws(z);
    let n = cfg_bareword_len(p)?;
    let z_rank = String::from_utf8_lossy(&p[..n]).into_owned();
    let p = cfg_skip_ws(&p[n..]);
    if p.first() != Some(&b'(') {
        return None;
    }
    let p = cfg_skip_ws(&p[1..]);
    if p.first() == Some(&b')') {
        return Some((z_rank, None));
    }
    // fts5ConfigSkipArgs
    let p_args = p;
    let mut q = p;
    loop {
        q = cfg_skip_ws(q);
        let n = cfg_literal_len(q)?;
        q = cfg_skip_ws(&q[n..]);
        match q.first() {
            Some(b')') => break,
            Some(b',') => q = &q[1..],
            _ => return None,
        }
    }
    let n_args = p_args.len() - q.len();
    Some((
        z_rank,
        Some(String::from_utf8_lossy(&p_args[..n_args]).into_owned()),
    ))
}

/// The auxiliary functions FTS5 registers (`sqlite3Fts5AuxInit`).
pub(super) fn find_auxiliary(name: &str) -> Option<&'static str> {
    ["snippet", "highlight", "bm25", "fts5_get_locale"]
        .into_iter()
        .find(|f| f.eq_ignore_ascii_case(name))
}

impl CsrCore {
    pub(super) fn new(
        cfg: Arc<Fts5Config>,
        state: Arc<Mutex<Fts5SharedState>>,
        aux_registry: Arc<Fts5AuxRegistry>,
        i_csr_id: i64,
    ) -> Self {
        let n_col = cfg.columns.len();
        Self {
            cfg,
            state,
            aux_registry,
            db_name: String::new(),
            tbl_name: String::new(),
            a_column_size: vec![0; n_col],
            i_csr_id,
            e_plan: 0,
            b_desc: false,
            i_first_rowid: 0,
            i_last_rowid: 0,
            stmt: None,
            expr: None,
            sorter: None,
            csrflags: 0,
            i_special: 0,
            z_rank: None,
            z_rank_args: None,
            p_rank: None,
            ap_rank_arg: Vec::new(),
            p_aux: None,
            auxdata: Vec::new(),
            n_inst_count: 0,
            a_inst: Vec::new(),
            replay: None,
            docsize_tbl: String::new(),
            docsize_hint: sqliters_types::ShadowHint::NONE,
        }
    }

    #[inline]
    fn flag_test(&self, f: i32) -> bool {
        (self.csrflags & f) != 0
    }

    /// `fts5EofMethod`.
    #[inline]
    pub(super) fn eof(&self) -> bool {
        self.flag_test(FTS5CSR_EOF)
    }

    /// `fts5CsrNewrow`.
    fn csr_newrow(&mut self) {
        self.csrflags |= FTS5CSR_REQUIRE_CONTENT
            | FTS5CSR_REQUIRE_DOCSIZE
            | FTS5CSR_REQUIRE_INST
            | FTS5CSR_REQUIRE_POSLIST;
    }

    /// `fts5FreeCursorComponents`.
    pub(super) fn free_cursor_components(&mut self) {
        self.a_inst = Vec::new();
        self.n_inst_count = 0;
        self.stmt = None;
        self.sorter = None;
        self.replay = None;
        // (`ePlan!=FTS5_PLAN_SOURCE`: the expression of a source cursor belongs to the
        // cursor whose sorter statement it runs)
        self.expr = None;
        self.auxdata.clear();
        self.ap_rank_arg = Vec::new();
        self.z_rank = None;
        self.z_rank_args = None;
        self.p_rank = None;
        self.e_plan = 0;
        self.b_desc = false;
        self.i_first_rowid = 0;
        self.i_last_rowid = 0;
        self.csrflags = 0;
        self.i_special = 0;
        self.p_aux = None;
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, Fts5SharedState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` on the expression and the index of the table.
    fn with_expr<T>(
        &self,
        f: impl FnOnce(&mut Fts5Expr, &mut Fts5Index) -> Result<T>,
    ) -> Result<T> {
        let Some(slot) = self.expr.as_ref() else {
            return Err(csr_err(1));
        };
        let mut eg = slot.lock().unwrap_or_else(|e| e.into_inner());
        let Some(expr) = eg.as_mut() else {
            return Err(csr_err(1));
        };
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f(expr, &mut st.index)
    }

    /// Reads from the expression (`None` when the cursor has none).
    fn expr_ref<T>(&self, f: impl FnOnce(&Fts5Expr) -> T) -> Option<T> {
        let slot = self.expr.as_ref()?;
        let eg = slot.lock().unwrap_or_else(|e| e.into_inner());
        eg.as_ref().map(f)
    }

    fn expr_phrase_count(&self) -> i32 {
        self.expr_ref(|e| e.phrase_count() as i32).unwrap_or(0)
    }

    /// The sorter the API reads the current row from: the real one of an `ORDER BY rank`
    /// cursor or the snapshot being replayed.
    fn cur_sorter(&self) -> Option<&Sorter> {
        self.replay.as_ref().or(self.sorter.as_ref())
    }

    // -----------------------------------------------------------------------------------
    // Sorter
    // -----------------------------------------------------------------------------------

    /// `fts5SorterNext`.
    fn sorter_next(&mut self) {
        let Some(s) = self.sorter.as_mut() else {
            return;
        };
        match s.rows.pop_front() {
            None => {
                self.csrflags |= FTS5CSR_EOF | FTS5CSR_REQUIRE_CONTENT;
            }
            Some((rowid, blob)) => {
                s.i_rowid = rowid;
                sorter_set_blob(s, &blob);
                self.csr_newrow();
            }
        }
    }

    /// `fts5CursorFirstSorted`.
    fn cursor_first_sorted(&mut self, ctx: &mut dyn VTabHostContext, b_desc: bool) -> Result<()> {
        let n_phrase = self.expr_phrase_count();
        let z_rank = self.z_rank.clone().unwrap_or_else(|| FTS5_DEFAULT_RANK.to_owned());
        let sql = format!(
            "SELECT rowid, rank FROM {}.{} ORDER BY {}(\"{}\"{}{}) {}",
            quote_single(&self.db_name),
            quote_single(&self.tbl_name),
            z_rank,
            self.tbl_name.replace('"', "\"\""),
            if self.z_rank_args.is_some() { ", " } else { "" },
            self.z_rank_args.as_deref().unwrap_or(""),
            if b_desc { "DESC" } else { "ASC" }
        );
        // (`fts5PrepareStatement`: a statement that calls an unknown function cannot be
        // prepared)
        if find_auxiliary(&z_rank).is_none() && !ctx.sql_function_exists(&z_rank) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such function: {z_rank}"),
            ));
        }
        let mut sorter = Sorter {
            a_idx: vec![0; n_phrase.max(0) as usize],
            ..Sorter::default()
        };
        {
            let mut st = self.lock_state();
            st.p_sort_csr = Some(SortCsr {
                expr: self.expr.clone().unwrap_or_default(),
                b_desc: self.b_desc,
                i_first_rowid: self.i_first_rowid,
                i_last_rowid: self.i_last_rowid,
            });
        }
        sqliters_types::dtrace!("TR|f5q|sorter|desc={}", i32::from(b_desc));
        let res = ctx.query_sql(&sql, &[]);
        self.lock_state().p_sort_csr = None;
        let rows = match res {
            Ok(r) => r,
            Err(e) => {
                // A statement that cannot be prepared reports its message
                // (`fts5PrepareStatement`); a failing `sqlite3_step` hands back the
                // primary result code only.
                if e.message.starts_with("no such function") {
                    return Err(e);
                }
                return Err(SqliteError::new(e.code, e.code.as_str()));
            }
        };
        for r in rows {
            let rowid = r.first().map_or(0, Value::to_integer_lossy);
            let blob = match r.into_iter().nth(1) {
                Some(Value::Blob(b)) => b,
                Some(Value::Text(t)) => t.into_bytes(),
                _ => Vec::new(),
            };
            sorter.rows.push_back((rowid, blob));
        }
        self.sorter = Some(sorter);
        self.sorter_next();
        Ok(())
    }

    /// `fts5CursorFirst`.
    fn cursor_first(&mut self, ctx: &mut dyn VTabHostContext, b_desc: bool) -> Result<()> {
        let (i_first, i_last) = (self.i_first_rowid, self.i_last_rowid);
        let r = self.with_expr(|e, ix| {
            let r = e.first(ix, ctx, i_first, i_last, b_desc);
            Ok((r, e.eof()))
        });
        let (res, eof) = match r {
            Ok(v) => v,
            Err(e) => (Err(e), false),
        };
        if eof {
            self.csrflags |= FTS5CSR_EOF;
        }
        sqliters_types::dtrace!(
            "TR|f5q|first|eof={} rowid={}",
            i32::from(eof),
            if eof { 0 } else { self.expr_ref(Fts5Expr::rowid).unwrap_or(0) }
        );
        self.csr_newrow();
        res
    }

    /// `fts5CursorReseek`: `Ok(true)` when the caller is done (`*pbSkip`).
    fn cursor_reseek(&mut self, ctx: &mut dyn VTabHostContext) -> Result<bool> {
        let mut b_skip = false;
        if self.flag_test(FTS5CSR_REQUIRE_RESEEK) {
            let b_desc = self.b_desc;
            let i_last = self.i_last_rowid;
            let (res, changed, eof) = self.with_expr(|e, ix| {
                let i_rowid = e.rowid();
                sqliters_types::dtrace!("TR|f5q|reseek|rowid={}", i_rowid);
                let r = e.first(ix, ctx, i_rowid, i_last, b_desc);
                Ok((r, i_rowid != e.rowid(), e.eof()))
            })?;
            if res.is_ok() && changed {
                b_skip = true;
            }
            self.csrflags &= !FTS5CSR_REQUIRE_RESEEK;
            self.csr_newrow();
            if eof {
                self.csrflags |= FTS5CSR_EOF;
                b_skip = true;
            }
            res?;
        }
        Ok(b_skip)
    }

    /// `fts5NextMethod`.
    pub(super) fn next(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        // If this cursor uses FTS5_PLAN_MATCH and this is a tokendata=1 table, clear any
        // token mappings accumulated at the fts5_index.c level.
        if self.e_plan == FTS5_PLAN_MATCH && self.cfg.tokendata {
            let _ = self.with_expr(|e, ix| {
                e.clear_tokens(ix);
                Ok(())
            });
        }
        if self.e_plan < 3 {
            if self.cursor_reseek(ctx)? {
                return Ok(());
            }
            let i_last = self.i_last_rowid;
            let (res, eof) = self.with_expr(|e, ix| {
                let r = e.next(ix, ctx, i_last);
                Ok((r, e.eof()))
            })?;
            if eof {
                self.csrflags |= FTS5CSR_EOF;
            }
            sqliters_types::dtrace!(
                "TR|f5q|next|eof={} rowid={}",
                i32::from(eof),
                if eof { 0 } else { self.expr_ref(Fts5Expr::rowid).unwrap_or(0) }
            );
            self.csr_newrow();
            res
        } else {
            match self.e_plan {
                FTS5_PLAN_SPECIAL => {
                    self.csrflags |= FTS5CSR_EOF;
                    Ok(())
                }
                FTS5_PLAN_SORTED_MATCH => {
                    self.sorter_next();
                    Ok(())
                }
                _ => {
                    // sqlite3_step(pCsr->pStmt)
                    let has_row = match self.stmt.as_mut() {
                        Some(ContentStmt::Scan { rows, cur }) => {
                            *cur = rows.pop_front();
                            cur.is_some()
                        }
                        Some(ContentStmt::Lookup(row)) => {
                            // (a rowid lookup has one row: the second step ends it)
                            *row = None;
                            false
                        }
                        None => false,
                    };
                    if !has_row {
                        self.csrflags |= FTS5CSR_EOF;
                    } else {
                        self.csrflags |= FTS5CSR_REQUIRE_DOCSIZE;
                    }
                    Ok(())
                }
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // xFilter
    // -----------------------------------------------------------------------------------

    /// `fts5SpecialMatch`.
    fn special_match(&mut self, z_query: &[u8]) -> Result<()> {
        let mut z = z_query;
        while z.first() == Some(&b' ') {
            z = &z[1..];
        }
        let n = z.iter().position(|&b| b == 0 || b == b' ').unwrap_or(z.len());
        self.e_plan = FTS5_PLAN_SPECIAL;
        if n == 5 && z[..n].eq_ignore_ascii_case(b"reads") {
            let n_read = self.lock_state().index.n_read;
            self.i_special = n_read;
            Ok(())
        } else if n == 2 && z[..n].eq_ignore_ascii_case(b"id") {
            self.i_special = self.i_csr_id;
            Ok(())
        } else {
            // An unrecognized directive. Return an error message.
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("unknown special query: {}", String::from_utf8_lossy(&z[..n])),
            ))
        }
    }

    /// `fts5FindRankFunction`.
    fn find_rank_function(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let z_rank = self.z_rank.clone().unwrap_or_else(|| FTS5_DEFAULT_RANK.to_owned());
        if let Some(args) = self.z_rank_args.clone() {
            let rows = ctx.query_sql(&format!("SELECT {args}"), &[]).map_err(rc_only)?;
            if let Some(r) = rows.into_iter().next() {
                self.ap_rank_arg = r;
            }
        }
        sqliters_types::dtrace!(
            "TR|f5q|rankfn|name={} args={}",
            z_rank,
            i32::from(self.z_rank_args.is_some())
        );
        match find_auxiliary(&z_rank) {
            Some(f) => {
                self.p_rank = Some(f);
                Ok(())
            }
            None => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such function: {z_rank}"),
            )),
        }
    }

    /// `fts5CursorParseRank`.
    fn cursor_parse_rank(&mut self, p_rank: Option<&Value>) -> Result<()> {
        match p_rank {
            Some(v) => {
                let z = fts5_value_bytes(v);
                let parsed = match z.as_deref() {
                    None => {
                        // (`sqlite3_value_text()` is NULL only for an SQL NULL)
                        None
                    }
                    Some(z) => {
                        let z0 = &z[..z.iter().position(|&b| b == 0).unwrap_or(z.len())];
                        config_parse_rank(z0)
                    }
                };
                match parsed {
                    Some((r, a)) => {
                        self.z_rank = Some(r);
                        self.z_rank_args = a;
                        Ok(())
                    }
                    None => Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "parse error in rank function: {}",
                            z.as_deref().map(String::from_utf8_lossy).unwrap_or_default()
                        ),
                    )),
                }
            }
            None => {
                let cfg_rank = self.lock_state().index.config.rank.clone();
                match cfg_rank.as_deref().and_then(|z| config_parse_rank(z.as_bytes())) {
                    Some((r, a)) => {
                        self.z_rank = Some(r);
                        self.z_rank_args = a;
                    }
                    None => {
                        self.z_rank = Some(FTS5_DEFAULT_RANK.to_owned());
                        self.z_rank_args = None;
                    }
                }
                Ok(())
            }
        }
    }

    /// The text of `zContent` for messages, or `None` when the table has no content
    /// source (contentless without %_docsize).
    fn z_content(&self) -> Option<String> {
        let db_q = quote_single(&self.db_name);
        match &self.cfg.content_mode {
            ContentMode::External { table, .. } => Some(format!("{db_q}.{}", quote_single(table))),
            ContentMode::Normal => Some(format!(
                "{db_q}.'{}_content'",
                self.tbl_name.replace('\'', "''")
            )),
            ContentMode::Contentless => {
                if self.cfg.unindexed_content {
                    Some(format!("{db_q}.'{}_content'", self.tbl_name.replace('\'', "''")))
                } else if self.cfg.columnsize {
                    Some(format!("{db_q}.'{}_docsize'", self.tbl_name.replace('\'', "''")))
                } else {
                    None
                }
            }
        }
    }

    /// `SELECT <zContentExprlist> FROM <zContent> T WHERE T.<rowid>=?` (`FTS5_STMT_LOOKUP`).
    fn content_lookup(
        &self,
        ctx: &mut dyn VTabHostContext,
        rowid: &Value,
    ) -> Result<Option<Vec<Value>>> {
        let n_col = self.cfg.columns.len();
        let cfg = &self.cfg;
        match &cfg.content_mode {
            ContentMode::External { table, rowid_col } => {
                let db_q = quote_ident(&self.db_name);
                let ext_q = quote_ident(table);
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let mut cols = vec![format!("T.{rid_q}")];
                for c in &cfg.columns {
                    cols.push(format!("T.{}", quote_ident(c)));
                }
                let sql = format!(
                    "SELECT {} FROM {db_q}.{ext_q} T WHERE T.{rid_q}=?",
                    cols.join(", ")
                );
                let mut rows = ctx.query_sql(&sql, std::slice::from_ref(rowid))?;
                Ok(if rows.is_empty() { None } else { Some(rows.swap_remove(0)) })
            }
            _ => {
                // The rowid of a rowid table: only an integer (or a value that converts
                // to one without loss) can be equal to it.
                let Some(rid) = rowid_value_as_integer(rowid) else {
                    return Ok(None);
                };
                let shadow = {
                    let st = self.lock_state();
                    if cfg.has_content_table() {
                        st.shadows.content.clone()
                    } else {
                        st.shadows.docsize.clone()
                    }
                };
                let Some(raw) = ctx.shadow_get_rowid(&shadow, rid)? else {
                    return Ok(None);
                };
                let mut row: Vec<Value> = Vec::with_capacity(1 + n_col);
                row.push(Value::Integer(rid));
                if cfg.content_mode != ContentMode::Contentless || cfg.unindexed_content {
                    let mut it = raw.into_iter().skip(1);
                    for i in 0..n_col {
                        if cfg.content_mode == ContentMode::Normal || cfg.unindexed[i] {
                            row.push(it.next().unwrap_or(Value::Null));
                        } else {
                            row.push(Value::Null);
                        }
                    }
                    if cfg.content_mode == ContentMode::Normal && cfg.locale {
                        for i in 0..n_col {
                            if !cfg.unindexed[i] {
                                row.push(it.next().unwrap_or(Value::Null));
                            } else {
                                row.push(Value::Null);
                            }
                        }
                    }
                }
                Ok(Some(row))
            }
        }
    }

    /// `FTS5_STMT_SCAN_ASC` / `FTS5_STMT_SCAN_DESC` with its two bindings.
    fn content_scan(
        &self,
        ctx: &mut dyn VTabHostContext,
        b_desc: bool,
    ) -> Result<VecDeque<Vec<Value>>> {
        let n_col = self.cfg.columns.len();
        let cfg = &self.cfg;
        let db_q = quote_ident(&self.db_name);
        let (tbl_q, rid_q, mut cols): (String, String, Vec<String>) = match &cfg.content_mode {
            ContentMode::External { table, rowid_col } => {
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let mut cols = vec![format!("T.{rid_q}")];
                for c in &cfg.columns {
                    cols.push(format!("T.{}", quote_ident(c)));
                }
                (quote_ident(table), rid_q, cols)
            }
            _ => {
                let shadow = {
                    let st = self.lock_state();
                    if cfg.has_content_table() {
                        st.shadows.content.clone()
                    } else {
                        st.shadows.docsize.clone()
                    }
                };
                let mut cols = vec!["T.\"rowid\"".to_owned()];
                if cfg.content_mode != ContentMode::Contentless || cfg.unindexed_content {
                    for i in 0..n_col {
                        if cfg.content_mode == ContentMode::Normal || cfg.unindexed[i] {
                            cols.push(format!("T.c{i}"));
                        } else {
                            cols.push("NULL".to_owned());
                        }
                    }
                }
                (quote_ident(&shadow), "\"rowid\"".to_owned(), cols)
            }
        };
        if cfg.content_mode == ContentMode::Normal && cfg.locale {
            for i in 0..n_col {
                if !cfg.unindexed[i] {
                    cols.push(format!("T.l{i}"));
                } else {
                    cols.push("NULL".to_owned());
                }
            }
        }
        let sql = if b_desc {
            format!(
                "SELECT {} FROM {db_q}.{tbl_q} T WHERE T.{rid_q} <= ? AND T.{rid_q} >= ? ORDER BY T.{rid_q} DESC",
                cols.join(", ")
            )
        } else {
            format!(
                "SELECT {} FROM {db_q}.{tbl_q} T WHERE T.{rid_q} >= ? AND T.{rid_q} <= ? ORDER BY T.{rid_q} ASC",
                cols.join(", ")
            )
        };
        let rows = ctx.query_sql(
            &sql,
            &[
                Value::Integer(self.i_first_rowid),
                Value::Integer(self.i_last_rowid),
            ],
        )?;
        Ok(rows.into())
    }

    /// `fts5FilterMethod`. `p_sort_csr` is `pTab->pSortCsr` (the cursor whose sorter
    /// statement this cursor runs).
    pub(super) fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        ap_val: &[Value],
    ) -> Result<()> {
        let r = self.filter_inner(ctx, idx_num, idx_str, ap_val);
        sqliters_types::dtrace!(
            "TR|f5q|filter|plan={} desc={} err={} eof={}",
            self.e_plan,
            i32::from(self.b_desc),
            i32::from(r.is_err()),
            i32::from(self.eof())
        );
        r
    }

    fn filter_inner(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        ap_val: &[Value],
    ) -> Result<()> {
        if self.e_plan != 0 {
            self.free_cursor_components();
        }
        self.db_name = ctx.db_name().to_owned();
        self.tbl_name = ctx.table_name().to_owned();
        let cfg = Arc::clone(&self.cfg);
        let z = idx_str.unwrap_or("").as_bytes();
        let mut p_rank: Option<&Value> = None;
        let mut p_rowid_eq: Option<&Value> = None;
        let mut p_rowid_le: Option<&Value> = None;
        let mut p_rowid_ge: Option<&Value> = None;
        let mut i_idx_str = 0usize;
        let mut p_expr: Option<Fts5Expr> = None;

        // Decode the arguments passed through to this function.
        for val in ap_val {
            let c = z.get(i_idx_str).copied().unwrap_or(b'>');
            i_idx_str += 1;
            match c {
                b'r' => p_rank = Some(val),
                b'M' | b'L' | b'G' => {
                    let mut i_col = 0usize;
                    loop {
                        i_col = i_col * 10
                            + usize::from(z.get(i_idx_str).copied().unwrap_or(b'0') - b'0');
                        i_idx_str += 1;
                        if !z.get(i_idx_str).is_some_and(u8::is_ascii_digit) {
                            break;
                        }
                    }
                    if c == b'M' {
                        let z_text = fts5_value_bytes(val);
                        let z_text: &[u8] = z_text.as_deref().unwrap_or(b"");
                        let z_text = &z_text[..z_text.iter().position(|&b| b == 0).unwrap_or(z_text.len())];
                        if z_text.first() == Some(&b'*') {
                            // The user has issued a query of the form "MATCH '*...'". This
                            // indicates that the MATCH expression is not a full text
                            // query, but a request for an internal parameter.
                            return self.special_match(&z_text[1..]);
                        }
                        let text = String::from_utf8_lossy(z_text).into_owned();
                        let e = Fts5Expr::new(&cfg, false, i_col, &text)?;
                        p_expr = Fts5Expr::and(p_expr, Some(e));
                    } else {
                        let b_glob = c == b'G';
                        if let Some(z_text) = fts5_value_bytes(val) {
                            let zt = &z_text[..z_text.iter().position(|&b| b == 0).unwrap_or(z_text.len())];
                            let e = Fts5Expr::pattern(&cfg, b_glob, i_col, zt)?;
                            p_expr = Fts5Expr::and(p_expr, e);
                        }
                    }
                }
                b'=' => p_rowid_eq = Some(val),
                b'<' => p_rowid_le = Some(val),
                _ => p_rowid_ge = Some(val),
            }
        }
        let b_order_by_rank = (idx_num & FTS5_BI_ORDER_RANK) != 0;
        let b_desc = (idx_num & FTS5_BI_ORDER_DESC) != 0;
        self.b_desc = b_desc;

        // Set the cursor upper and lower rowid limits. Only some strategies actually use
        // them.
        if p_rowid_eq.is_some() {
            p_rowid_le = p_rowid_eq;
            p_rowid_ge = p_rowid_eq;
        }
        let lim = |v: Option<&Value>, dflt: i64| numeric_integer_value(v).unwrap_or(dflt);
        if b_desc {
            self.i_first_rowid = lim(p_rowid_le, i64::MAX);
            self.i_last_rowid = lim(p_rowid_ge, i64::MIN);
        } else {
            self.i_last_rowid = lim(p_rowid_le, i64::MAX);
            self.i_first_rowid = lim(p_rowid_ge, i64::MIN);
        }

        let p_sort_csr = {
            let mut st = self.lock_state();
            st.index.load_config(ctx)?;
            st.p_sort_csr.clone()
        };
        if let Some(sc) = p_sort_csr {
            // This call is being made as part of processing for a "... MATCH <expr> ORDER
            // BY rank" query (ePlan is set to FTS5_PLAN_SORTED_MATCH). pSortCsr is the
            // cursor that will return results to the user for this query. The current
            // cursor is used to execute the query issued by fts5CursorFirstSorted().
            if sc.b_desc {
                self.i_last_rowid = sc.i_first_rowid;
                self.i_first_rowid = sc.i_last_rowid;
            } else {
                self.i_last_rowid = sc.i_last_rowid;
                self.i_first_rowid = sc.i_first_rowid;
            }
            self.e_plan = FTS5_PLAN_SOURCE;
            self.expr = Some(sc.expr);
            self.cursor_first(ctx, b_desc)
        } else if let Some(e) = p_expr {
            self.expr = Some(Arc::new(Mutex::new(Some(e))));
            self.cursor_parse_rank(p_rank)?;
            if b_order_by_rank {
                self.e_plan = FTS5_PLAN_SORTED_MATCH;
                self.cursor_first_sorted(ctx, b_desc)
            } else {
                self.e_plan = FTS5_PLAN_MATCH;
                self.cursor_first(ctx, b_desc)
            }
        } else if self.z_content().is_none() {
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("{}: table does not support scanning", self.tbl_name),
            ))
        } else {
            // This is either a full-table scan (ePlan==FTS5_PLAN_SCAN) or a lookup by
            // rowid (ePlan==FTS5_PLAN_ROWID).
            self.e_plan = if p_rowid_eq.is_some() {
                FTS5_PLAN_ROWID
            } else {
                FTS5_PLAN_SCAN
            };
            if let Some(eq) = p_rowid_eq {
                let row = self.content_lookup(ctx, eq)?;
                let has_row = row.is_some();
                self.stmt = Some(ContentStmt::Lookup(row));
                if !has_row {
                    self.csrflags |= FTS5CSR_EOF;
                } else {
                    self.csrflags |= FTS5CSR_REQUIRE_DOCSIZE;
                }
                Ok(())
            } else {
                let rows = self.content_scan(ctx, b_desc)?;
                self.stmt = Some(ContentStmt::Scan { rows, cur: None });
                self.next(ctx)
            }
        }
    }

    /// `fts5CursorRowid`.
    pub(super) fn cursor_rowid(&self) -> i64 {
        if let Some(s) = self.cur_sorter() {
            s.i_rowid
        } else if self.e_plan >= FTS5_PLAN_SCAN {
            self.stmt_row()
                .and_then(|r| r.first())
                .map_or(0, Value::to_integer_lossy)
        } else {
            self.expr_ref(Fts5Expr::rowid).unwrap_or(0)
        }
    }

    /// `fts5RowidMethod`.
    pub(super) fn rowid(&self) -> i64 {
        if self.e_plan == FTS5_PLAN_SPECIAL {
            0
        } else {
            self.cursor_rowid()
        }
    }

    /// The current row of `pCsr->pStmt`.
    fn stmt_row(&self) -> Option<&Vec<Value>> {
        match self.stmt.as_ref()? {
            ContentStmt::Lookup(r) => r.as_ref(),
            ContentStmt::Scan { cur, .. } => cur.as_ref(),
        }
    }

    /// `fts5SeekCursor`: positions `pStmt` on the content row of the current rowid.
    fn seek_cursor(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        // If the cursor does not yet have a statement handle, obtain one now.
        if self.stmt.is_none() {
            self.stmt = Some(ContentStmt::Lookup(None));
        }
        if self.flag_test(FTS5CSR_REQUIRE_CONTENT) {
            let rowid = self.cursor_rowid();
            sqliters_types::dtrace!("TR|f5q|content|rowid={}", rowid);
            let row = self.content_lookup(ctx, &Value::Integer(rowid))?;
            match row {
                Some(r) => {
                    self.stmt = Some(ContentStmt::Lookup(Some(r)));
                    self.csrflags &= !FTS5CSR_REQUIRE_CONTENT;
                }
                None => {
                    self.stmt = Some(ContentStmt::Lookup(None));
                    return Err(SqliteError::with_extended(
                        FTS5_CORRUPT,
                        format!(
                            "fts5: missing row {rowid} from content table {}",
                            self.z_content().unwrap_or_default()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// `fts5TextFromStmt`: the text of column `i_col` of the current content row.
    fn text_from_stmt(&self, i_col: usize) -> Option<Vec<u8>> {
        let row = self.stmt_row()?;
        let v = row.get(i_col + 1)?;
        fts5_value_bytes(v).map(std::borrow::Cow::into_owned)
    }

    // -----------------------------------------------------------------------------------
    // xColumn
    // -----------------------------------------------------------------------------------

    /// `fts5PoslistBlob`: the position lists of every phrase for the current row.
    pub(super) fn poslist_blob(&self) -> Result<Vec<u8>> {
        let detail = self.cfg.detail;
        self.expr_ref(|e| {
            let n_phrase = e.phrase_count();
            let mut val: Vec<u8> = Vec::new();
            match detail {
                DetailMode::Full => {
                    // Append the varints
                    for i in 0..n_phrase.saturating_sub(1) {
                        fts5_put_varint(&mut val, e.poslist(i).len() as u64);
                    }
                    // Append the position lists
                    for i in 0..n_phrase {
                        val.extend_from_slice(e.poslist(i));
                    }
                }
                DetailMode::Col => {
                    for i in 0..n_phrase.saturating_sub(1) {
                        fts5_put_varint(&mut val, e.phrase_collist(i).len() as u64);
                    }
                    for i in 0..n_phrase {
                        val.extend_from_slice(&e.phrase_collist(i));
                    }
                }
                DetailMode::None => {}
            }
            val
        })
        .ok_or_else(|| csr_err(1))
    }

    /// `fts5ColumnMethod`.
    pub(super) fn column(&mut self, ctx: &mut dyn VTabHostContext, i_col: usize) -> Result<Value> {
        let n_col = self.cfg.columns.len();
        if self.e_plan == FTS5_PLAN_SPECIAL {
            if i_col == n_col {
                return Ok(Value::Integer(self.i_special));
            }
            Ok(Value::Null)
        } else if i_col == n_col {
            // User is requesting the value of the special column with the same name as
            // the table. Return the cursor integer id number.
            Ok(Value::Integer(self.i_csr_id))
        } else if i_col == n_col + 1 {
            // The value of the "rank" column.
            if self.e_plan == FTS5_PLAN_SOURCE {
                Ok(Value::Blob(self.poslist_blob()?))
            } else if self.e_plan == FTS5_PLAN_MATCH || self.e_plan == FTS5_PLAN_SORTED_MATCH {
                if self.p_rank.is_none() {
                    self.find_rank_function(ctx)?;
                }
                let f = self.p_rank.unwrap_or(FTS5_DEFAULT_RANK);
                let args = self.ap_rank_arg.clone();
                self.api_invoke(ctx, f, &args)
            } else {
                Ok(Value::Null)
            }
        } else if self.cfg.content_mode != ContentMode::Contentless || self.cfg.unindexed_content
        {
            self.seek_cursor(ctx)?;
            Ok(self
                .stmt_row()
                .and_then(|r| r.get(i_col + 1))
                .cloned()
                .unwrap_or(Value::Null))
        } else {
            Ok(Value::Null)
        }
    }

    // -----------------------------------------------------------------------------------
    // Fts5ExtensionApi
    // -----------------------------------------------------------------------------------

    /// `fts5ApiInvoke`.
    pub(super) fn api_invoke(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        f: &'static str,
        args: &[Value],
    ) -> Result<Value> {
        self.p_aux = Some(f);
        sqliters_types::dtrace!("TR|f5q|aux|fn={} plan={}", f, self.e_plan);
        let r = super::auxfn::call_aux(self, ctx, f, args);
        self.p_aux = None;
        r
    }

    /// `fts5ApiColumnCount`.
    pub(super) fn api_column_count(&self) -> i32 {
        self.cfg.columns.len() as i32
    }

    /// `fts5StorageLoadTotals(p, 0)`: (nTotalRow, aTotalSize).
    fn storage_totals(&mut self, ctx: &mut dyn VTabHostContext) -> Result<(i64, Vec<i64>)> {
        let mut st = self.lock_state();
        if !st.b_totals_valid {
            let (n_row, sizes) = st.index.get_averages(ctx)?;
            st.n_total_row = n_row;
            st.a_total_size = sizes;
        }
        Ok((st.n_total_row, st.a_total_size.clone()))
    }

    /// `fts5ApiColumnTotalSize` (`sqlite3Fts5StorageSize`).
    pub(super) fn api_column_total_size(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_col: i32,
    ) -> Result<i64> {
        let (_, sizes) = self.storage_totals(ctx)?;
        if i_col < 0 {
            Ok(sizes.iter().fold(0i64, |a, &b| a.wrapping_add(b)))
        } else if (i_col as usize) < self.cfg.columns.len() {
            Ok(sizes.get(i_col as usize).copied().unwrap_or(0))
        } else {
            Err(range_err())
        }
    }

    /// `fts5ApiRowCount` (`sqlite3Fts5StorageRowCount`).
    pub(super) fn api_row_count(&mut self, ctx: &mut dyn VTabHostContext) -> Result<i64> {
        let (n_row, _) = self.storage_totals(ctx)?;
        if n_row <= 0 {
            return Err(corrupt_err());
        }
        Ok(n_row)
    }

    /// `fts5ApiPhraseCount`.
    pub(super) fn api_phrase_count(&self) -> i32 {
        self.expr_phrase_count()
    }

    /// `fts5ApiPhraseSize`.
    pub(super) fn api_phrase_size(&self, i_phrase: i32) -> i32 {
        self.expr_ref(|e| e.phrase_size(i_phrase)).unwrap_or(0)
    }

    /// `fts5IsContentless`.
    fn is_contentless(&self, b_include_unindexed: bool) -> bool {
        self.cfg.content_mode == ContentMode::Contentless
            && (b_include_unindexed || !self.cfg.unindexed_content)
    }

    /// `fts5ApiColumnText`: `None` for a NULL value or a contentless table.
    pub(super) fn api_column_text(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_col: i32,
    ) -> Result<Option<Vec<u8>>> {
        if i_col < 0 || i_col as usize >= self.cfg.columns.len() {
            Err(range_err())
        } else if self.is_contentless(false) {
            Ok(None)
        } else {
            self.seek_cursor(ctx).map_err(rc_only_keep_ext)?;
            Ok(self.text_from_stmt(i_col as usize))
        }
    }

    /// `fts5CsrPoslist`: makes the position list of every phrase available for the
    /// current row and returns that of phrase `i_phrase`.
    pub(super) fn csr_poslist(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_phrase: i32,
    ) -> Result<Vec<u8>> {
        let detail = self.cfg.detail;
        let b_live = self.cur_sorter().is_none();
        if i_phrase < 0 || i_phrase >= self.expr_phrase_count() {
            return Err(range_err());
        } else if detail != DetailMode::Full && self.is_contentless(true) {
            return Ok(Vec::new());
        } else if self.flag_test(FTS5CSR_REQUIRE_POSLIST) {
            let mut res: Result<()> = Ok(());
            if detail != DetailMode::Full {
                sqliters_types::dtrace!("TR|f5q|populate|live={}", i32::from(b_live));
                let live_miss = self.replay.as_ref().and_then(|s| s.live_miss.clone());
                let replay_rowid = self.replay.as_ref().map(|s| s.i_rowid);
                let mut a_populator: Vec<PoslistPopulator> =
                    self.with_expr(|e, _| Ok(e.clear_poslists(b_live)))?;
                if let (Some(m), Some(rowid)) = (live_miss.as_ref(), replay_rowid) {
                    self.with_expr(|e, _| {
                        e.set_replay_row(rowid, m, &mut a_populator);
                        Ok(())
                    })?;
                }
                res = self.seek_cursor(ctx).map_err(rc_only_keep_ext);
                if res.is_ok() {
                    let cfg = Arc::clone(&self.cfg);
                    for i in 0..cfg.columns.len() {
                        let z = self.text_from_stmt(i).unwrap_or_default();
                        let z = &z[..z.iter().position(|&b| b == 0).unwrap_or(z.len())];
                        self.with_expr(|e, ix| {
                            e.populate_poslists(&cfg, ix, &mut a_populator, i as i32, z);
                            Ok(())
                        })?;
                    }
                }
                if live_miss.is_none()
                    && let Some(rowid) = self.cur_sorter().map(|s| s.i_rowid)
                {
                    self.with_expr(|e, _| {
                        e.check_poslists(rowid);
                        Ok(())
                    })?;
                }
            }
            self.csrflags &= !FTS5CSR_REQUIRE_POSLIST;
            res?;
        }
        if let Some(s) = self.cur_sorter()
            && detail == DetailMode::Full
        {
            let i1 = if i_phrase == 0 { 0 } else { s.a_idx[(i_phrase - 1) as usize] };
            let i2 = s.a_idx[i_phrase as usize];
            let (a, b) = (i1.max(0) as usize, i2.max(0) as usize);
            let b = b.min(s.a_poslist.len());
            let a = a.min(b);
            Ok(s.a_poslist[a..b].to_vec())
        } else {
            Ok(self
                .expr_ref(|e| e.poslist(i_phrase as usize).to_vec())
                .unwrap_or_default())
        }
    }

    /// `fts5CacheInstArray`.
    fn cache_inst_array(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let n_col = self.cfg.columns.len() as i32;
        let n_iter = self.expr_phrase_count().max(0) as usize;
        let mut lists: Vec<Vec<u8>> = Vec::with_capacity(n_iter);
        let mut res: Result<()> = Ok(());
        // Initialize all iterators
        for i in 0..n_iter {
            match self.csr_poslist(ctx, i as i32) {
                Ok(a) => lists.push(a),
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
        }
        let mut n_inst = 0i32;
        if res.is_ok() {
            let mut a_iter: Vec<PoslistReader> = lists
                .iter()
                .map(|a| PoslistReader::init(a, a.len() as i32))
                .collect();
            self.a_inst.clear();
            loop {
                let mut i_best: i32 = -1;
                for (i, it) in a_iter.iter().enumerate() {
                    if !it.b_eof && (i_best < 0 || it.i_pos < a_iter[i_best as usize].i_pos) {
                        i_best = i as i32;
                    }
                }
                if i_best < 0 {
                    break;
                }
                n_inst += 1;
                let pos = a_iter[i_best as usize].i_pos;
                let c = ((pos >> 32) & 0x7FFF_FFFF) as i32;
                self.a_inst.push(i_best);
                self.a_inst.push(c);
                self.a_inst.push((pos & 0x7FFF_FFFF) as i32);
                if c >= n_col {
                    res = Err(corrupt_err());
                    break;
                }
                a_iter[i_best as usize].next(&lists[i_best as usize]);
            }
        }
        self.n_inst_count = n_inst;
        self.csrflags &= !FTS5CSR_REQUIRE_INST;
        sqliters_types::dtrace!("TR|f5q|inst|n={}", n_inst);
        res
    }

    /// `fts5ApiInstCount`.
    pub(super) fn api_inst_count(&mut self, ctx: &mut dyn VTabHostContext) -> Result<i32> {
        if self.flag_test(FTS5CSR_REQUIRE_INST) {
            self.cache_inst_array(ctx)?;
        }
        Ok(self.n_inst_count)
    }

    /// `fts5ApiInst`: (phrase, column, offset).
    pub(super) fn api_inst(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_idx: i32,
    ) -> Result<(i32, i32, i32)> {
        if self.flag_test(FTS5CSR_REQUIRE_INST) {
            self.cache_inst_array(ctx)?;
        }
        if i_idx < 0 || i_idx >= self.n_inst_count {
            return Err(range_err());
        }
        let k = (i_idx * 3) as usize;
        Ok((self.a_inst[k], self.a_inst[k + 1], self.a_inst[k + 2]))
    }

    /// `sqlite3Fts5StorageDocsize`.
    fn storage_docsize(&mut self, ctx: &mut dyn VTabHostContext, i_rowid: i64) -> Result<()> {
        let n_col = self.cfg.columns.len();
        if self.docsize_tbl.is_empty() {
            let name = self.lock_state().shadows.docsize.clone();
            self.docsize_tbl = name;
        }
        sqliters_types::dtrace!("TR|f5q|docsize|rowid={}", i_rowid);
        let row = ctx.shadow_blob_get(&self.docsize_tbl, &mut self.docsize_hint, i_rowid, 1)?;
        let mut b_corrupt = true;
        if let Some(r) = row {
            let blob: &[u8] = &r;
            // fts5StorageDecodeSizeArray
            let mut i_off = 0usize;
            let mut ok = true;
            for i in 0..n_col {
                if i_off >= blob.len() {
                    ok = false;
                    break;
                }
                let (v, n) = get_varint32(blob, i_off);
                i_off += n;
                self.a_column_size[i] = v;
            }
            if ok && i_off == blob.len() {
                b_corrupt = false;
            }
        }
        if b_corrupt {
            return Err(corrupt_err());
        }
        Ok(())
    }

    /// `fts5ApiColumnSize`.
    pub(super) fn api_column_size(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_col: i32,
    ) -> Result<i32> {
        let cfg = Arc::clone(&self.cfg);
        let n_col = cfg.columns.len();
        let mut res: Result<()> = Ok(());
        if self.flag_test(FTS5CSR_REQUIRE_DOCSIZE) {
            if cfg.columnsize {
                let i_rowid = self.cursor_rowid();
                res = self.storage_docsize(ctx, i_rowid);
            } else if self.z_content().is_none() || cfg.unindexed_content {
                for i in 0..n_col {
                    if !cfg.unindexed[i] {
                        self.a_column_size[i] = -1;
                    }
                }
            } else {
                res = self.seek_cursor(ctx).map_err(rc_only_keep_ext);
                if res.is_ok() {
                    for i in 0..n_col {
                        if !cfg.unindexed[i] {
                            self.a_column_size[i] = 0;
                            let z = self.text_from_stmt(i).unwrap_or_default();
                            let z = &z[..z.iter().position(|&b| b == 0).unwrap_or(z.len())];
                            let mut n = 0i32;
                            cfg.tokenizer.tok.tokenize(z, &mut |_, _, _| n += 1);
                            self.a_column_size[i] = n;
                        }
                    }
                }
            }
            self.csrflags &= !FTS5CSR_REQUIRE_DOCSIZE;
        }
        let n_token;
        if i_col < 0 {
            n_token = self
                .a_column_size
                .iter()
                .fold(0i32, |a, &b| a.wrapping_add(b));
        } else if (i_col as usize) < n_col {
            n_token = self.a_column_size[i_col as usize];
        } else {
            res?;
            return Err(range_err());
        }
        res?;
        Ok(n_token)
    }

    /// `fts5ApiSetAuxdata`.
    pub(super) fn api_set_auxdata(&mut self, ptr: Box<dyn Any + Send + Sync>) {
        let aux = self.p_aux.unwrap_or("");
        match self.auxdata.iter_mut().find(|d| d.aux == aux) {
            Some(d) => d.ptr = Some(ptr),
            None => self.auxdata.insert(
                0,
                Auxdata {
                    aux,
                    ptr: Some(ptr),
                },
            ),
        }
    }

    /// `fts5ApiGetAuxdata(pCtx, 1)`: takes the value out (the caller puts it back with
    /// [`Self::api_set_auxdata`]).
    pub(super) fn api_take_auxdata(&mut self) -> Option<Box<dyn Any + Send + Sync>> {
        let aux = self.p_aux.unwrap_or("");
        self.auxdata
            .iter_mut()
            .find(|d| d.aux == aux)
            .and_then(|d| d.ptr.take())
    }

    /// `fts5ApiInstToken` (no built-in auxiliary function calls it).
    #[allow(dead_code)]
    pub(super) fn api_inst_token(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_idx: i32,
        i_token: i32,
    ) -> Result<Option<Vec<u8>>> {
        if self.flag_test(FTS5CSR_REQUIRE_INST) {
            self.cache_inst_array(ctx)?;
        }
        if i_idx < 0 || i_idx >= self.n_inst_count {
            return Err(range_err());
        }
        let k = (i_idx * 3) as usize;
        let (i_phrase, i_col, i_off) = (self.a_inst[k], self.a_inst[k + 1], self.a_inst[k + 2]);
        let i_rowid = self.cursor_rowid();
        self.with_expr(|e, ix| e.inst_token(ix, ctx, i_rowid, i_phrase, i_col, i_off, i_token))
    }

    /// `fts5ApiColumnLocale`.
    pub(super) fn api_column_locale(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_col: i32,
    ) -> Result<Option<Vec<u8>>> {
        let n_col = self.cfg.columns.len();
        if i_col < 0 || i_col as usize >= n_col {
            return Err(range_err());
        }
        if !self.cfg.unindexed[i_col as usize] && !self.is_contentless(true) && self.cfg.locale {
            self.seek_cursor(ctx).map_err(rc_only_keep_ext)?;
            if self.cfg.content_mode == ContentMode::Normal {
                let loc = self
                    .stmt_row()
                    .and_then(|r| r.get(i_col as usize + 1 + n_col))
                    .and_then(|v| fts5_value_bytes(v).map(std::borrow::Cow::into_owned));
                return Ok(loc);
            }
        }
        Ok(None)
    }

    /// `fts5ApiQueryPhrase`: runs `cb` for every row phrase `i_phrase` matches on its own;
    /// the callback returns false to stop (SQLITE_DONE).
    pub(super) fn api_query_phrase(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        i_phrase: i32,
        cb: &mut dyn FnMut(&mut CsrCore, &mut dyn VTabHostContext) -> Result<bool>,
    ) -> Result<()> {
        // fts5OpenMethod
        let id = self.aux_registry.alloc_id();
        let mut new = CsrCore::new(
            Arc::clone(&self.cfg),
            Arc::clone(&self.state),
            Arc::clone(&self.aux_registry),
            id,
        );
        new.db_name = self.db_name.clone();
        new.tbl_name = self.tbl_name.clone();
        new.e_plan = FTS5_PLAN_MATCH;
        new.i_first_rowid = i64::MIN;
        new.i_last_rowid = i64::MAX;
        sqliters_types::dtrace!("TR|f5q|qphrase|i={}", i_phrase);
        let cloned = match self.expr_ref(|e| e.clone_phrase(i_phrase.max(-1) as usize)) {
            Some(r) => r?,
            None => return Err(range_err()),
        };
        if i_phrase < 0 {
            return Err(range_err());
        }
        new.expr = Some(Arc::new(Mutex::new(Some(cloned))));
        new.cursor_first(ctx, false)?;
        while !new.eof() {
            if !cb(&mut new, ctx)? {
                break;
            }
            new.next(ctx)?;
        }
        Ok(())
    }

    /// `fts5TripCursors` for this cursor.
    pub(super) fn trip(&mut self) {
        if self.e_plan == FTS5_PLAN_MATCH {
            self.csrflags |= FTS5CSR_REQUIRE_RESEEK;
        }
    }

    /// Positions the API on a snapshot row (`rowid`, position-list blob) taken earlier.
    pub(super) fn set_replay(&mut self, rowid: i64, blob: &[u8], live_miss: Option<&[bool]>) {
        let n_phrase = self.expr_phrase_count().max(0) as usize;
        let mut s = Sorter {
            a_idx: vec![0; n_phrase],
            i_rowid: rowid,
            live_miss: live_miss.map(<[bool]>::to_vec),
            ..Sorter::default()
        };
        sorter_set_blob(&mut s, blob);
        self.replay = Some(s);
        self.csr_newrow();
    }

    /// The snapshot of the current row: its rowid, its position-list blob and, for a live
    /// (not sorted) row, which phrases are not on the row.
    pub(super) fn snapshot(&self) -> SnapRow {
        let (rowid, blob) = self.snapshot_blob();
        let live_miss = if self.sorter.is_none() && self.cfg.detail != DetailMode::Full {
            self.expr_ref(|e| e.live_miss())
        } else {
            None
        };
        (rowid, blob, live_miss)
    }

    fn snapshot_blob(&self) -> (i64, Vec<u8>) {
        let rowid = self.cursor_rowid();
        if let Some(s) = self.sorter.as_ref() {
            // re-encode the sorter row
            let mut blob: Vec<u8> = Vec::new();
            let mut prev = 0i32;
            for (i, &end) in s.a_idx.iter().enumerate() {
                if i + 1 < s.a_idx.len() {
                    fts5_put_varint(&mut blob, (end - prev).max(0) as u64);
                }
                prev = end;
            }
            blob.extend_from_slice(&s.a_poslist);
            (rowid, blob)
        } else if self.expr.is_some() {
            (rowid, self.poslist_blob().unwrap_or_default())
        } else {
            (rowid, Vec::new())
        }
    }
}

/// The body of `fts5SorterNext` that decodes a position-list blob.
fn sorter_set_blob(s: &mut Sorter, blob: &[u8]) {
    let n_blob = blob.len();
    // nBlob==0 in detail=none mode.
    if n_blob > 0 {
        let n_idx = s.a_idx.len();
        let mut a = 0usize;
        let mut i_off = 0i32;
        let mut i = 0usize;
        while i + 1 < n_idx {
            let (v, n) = get_varint32(blob, a);
            a += n;
            i_off = i_off.wrapping_add(v);
            s.a_idx[i] = i_off;
            i += 1;
        }
        if n_idx > 0 {
            s.a_idx[i] = n_blob as i32 - a as i32;
        }
        s.a_poslist = blob.get(a..).unwrap_or(&[]).to_vec();
    }
}

/// An error an API function hands to an auxiliary function is a result code only (the
/// message stays in `sqlite3_vtab.zErrMsg`, which `fts5ApiCallback` discards).
fn rc_only_keep_ext(e: SqliteError) -> SqliteError {
    rc_only(e)
}

/// The integer a value is equal to when compared with a rowid (`OP_SeekRowid` /
/// `OP_MustBeInt` of `T.rowid=?`).
fn rowid_value_as_integer(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        Value::Real(f) => {
            let i = *f as i64;
            if (i as f64) == *f && f.is_finite() { Some(i) } else { None }
        }
        Value::Text(_) => numeric_integer_value(Some(v)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------
// The cursors of a connection (`Fts5Global.pCsr`, `iNextId`) and the auxiliary-function
// entry point (`fts5ApiCallback`)
// ---------------------------------------------------------------------------------------

const FTS5_AUX_RING_SLOTS: usize = 64;
/// Marks the value of the table-name column of a row that was read ahead of its use (a
/// statement the host does not step row by row): `flag | serial << 32 | row + 1`.
const FTS5_SNAP_FLAG: i64 = 1 << 62;

/// The rows of one scan whose auxiliary-function state was saved: the cursor they belong
/// to and, per row, the rowid and the position-list blob (`fts5PoslistBlob`).
#[derive(Debug)]
struct SnapSet {
    core: Arc<Mutex<CsrCore>>,
    rows: Vec<SnapRow>,
}

/// One saved row: rowid, position-list blob, `bMiss` per phrase (live rows of a
/// detail=col / detail=none table).
type SnapRow = (i64, Vec<u8>, Option<Vec<bool>>);

#[derive(Debug)]
struct RegInner {
    /// `Fts5Global.iNextId`.
    i_next_id: i64,
    /// `Fts5Global.pCsr`: the open cursors.
    cursors: Vec<(i64, Arc<Mutex<CsrCore>>)>,
    snap_serial: u32,
    snaps: Vec<Option<(u32, Arc<SnapSet>)>>,
    /// The connected FTS5 tables (`sqlite3Fts5TableFromCsrid` for fts5vocab).
    tables: Vec<(std::sync::Weak<Mutex<Fts5SharedState>>, Arc<Fts5Config>)>,
}

/// The FTS5 state of a connection the auxiliary functions reach cursors through
/// (`Fts5Global`).
#[derive(Debug)]
pub struct Fts5AuxRegistry {
    inner: Mutex<RegInner>,
}

impl Default for Fts5AuxRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(RegInner {
                i_next_id: 0,
                cursors: Vec::new(),
                snap_serial: 0,
                snaps: (0..FTS5_AUX_RING_SLOTS).map(|_| None).collect(),
                tables: Vec::new(),
            }),
        }
    }
}

/// The cursor (and row) an auxiliary function call refers to.
#[derive(Debug)]
pub struct Fts5AuxTarget {
    core: Arc<Mutex<CsrCore>>,
    snap: Option<(Arc<SnapSet>, usize)>,
    /// Schema and name of the FTS5 table of the cursor.
    pub db_name: String,
    pub table_name: String,
}

impl Fts5AuxRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RegInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `++pGlobal->iNextId`.
    pub(super) fn alloc_id(&self) -> i64 {
        let mut g = self.lock();
        g.i_next_id += 1;
        g.i_next_id
    }

    fn register(&self, id: i64, core: &Arc<Mutex<CsrCore>>) {
        self.lock().cursors.insert(0, (id, Arc::clone(core)));
    }

    fn unregister(&self, core: &Arc<Mutex<CsrCore>>) {
        self.lock().cursors.retain(|(_, c)| !Arc::ptr_eq(c, core));
    }

    fn publish_snaps(&self, core: &Arc<Mutex<CsrCore>>, rows: Vec<SnapRow>) -> u32 {
        let mut g = self.lock();
        g.snap_serial = if g.snap_serial >= 0x3FFF_FFFE { 1 } else { g.snap_serial + 1 };
        let serial = g.snap_serial;
        g.snaps[(serial as usize) % FTS5_AUX_RING_SLOTS] = Some((
            serial,
            Arc::new(SnapSet {
                core: Arc::clone(core),
                rows,
            }),
        ));
        serial
    }

    fn next_snap_serial(&self) -> u32 {
        let g = self.lock();
        if g.snap_serial >= 0x3FFF_FFFE { 1 } else { g.snap_serial + 1 }
    }

    /// Records a connected FTS5 table.
    pub(super) fn register_table(
        &self,
        state: &Arc<Mutex<Fts5SharedState>>,
        cfg: &Arc<Fts5Config>,
    ) {
        let mut g = self.lock();
        g.tables.retain(|(w, _)| w.strong_count() > 0);
        g.tables.push((Arc::downgrade(state), Arc::clone(cfg)));
    }

    /// The connected FTS5 table `db`.`tbl`.
    pub(super) fn find_table(
        &self,
        db: &str,
        tbl: &str,
    ) -> Option<(Arc<Mutex<Fts5SharedState>>, Arc<Fts5Config>)> {
        let tables: Vec<_> = self.lock().tables.clone();
        for (w, cfg) in tables.into_iter().rev() {
            if let Some(st) = w.upgrade() {
                let hit = {
                    let g = st.lock().unwrap_or_else(|e| e.into_inner());
                    g.shadows.table_name.eq_ignore_ascii_case(tbl)
                        && g.index.db_name().eq_ignore_ascii_case(db)
                };
                if hit {
                    return Some((st, cfg));
                }
            }
        }
        None
    }

    /// `fts5TripCursors`: every FTS5_PLAN_MATCH cursor open on the table must reseek.
    pub(super) fn trip_cursors(&self, state: &Arc<Mutex<Fts5SharedState>>) {
        let cores: Vec<Arc<Mutex<CsrCore>>> =
            self.lock().cursors.iter().map(|(_, c)| Arc::clone(c)).collect();
        for c in cores {
            if let Ok(mut core) = c.try_lock()
                && Arc::ptr_eq(&core.state, state)
            {
                core.trip();
            }
        }
    }

    /// True for the name of an auxiliary function of FTS5 (`fts5FindAuxiliary`).
    pub fn is_aux_function(name: &str) -> bool {
        find_auxiliary(name).is_some()
    }

    /// `fts5CursorFromCsrid(pGlobal, sqlite3_value_int64(argv[0]))`: the cursor the first
    /// argument of an auxiliary function names, or `None` ("no such cursor").
    pub fn resolve(&self, arg0: &Value) -> Option<Fts5AuxTarget> {
        let id = arg0.to_integer_lossy();
        let g = self.lock();
        if id > 0 && (id & FTS5_SNAP_FLAG) != 0 {
            let serial = ((id >> 32) & 0x3FFF_FFFF) as u32;
            let row = (id & 0xFFFF_FFFF) as usize;
            let (s, set) = g.snaps[(serial as usize) % FTS5_AUX_RING_SLOTS].as_ref()?;
            if *s != serial || row == 0 || row > set.rows.len() {
                return None;
            }
            let (db_name, table_name) = {
                let c = set.core.lock().unwrap_or_else(|e| e.into_inner());
                (c.db_name.clone(), c.tbl_name.clone())
            };
            return Some(Fts5AuxTarget {
                core: Arc::clone(&set.core),
                snap: Some((Arc::clone(set), row - 1)),
                db_name,
                table_name,
            });
        }
        let core = g.cursors.iter().find(|(i, _)| *i == id).map(|(_, c)| Arc::clone(c))?;
        drop(g);
        let (db_name, table_name, e_plan) = {
            let c = core.lock().unwrap_or_else(|e| e.into_inner());
            (c.db_name.clone(), c.tbl_name.clone(), c.e_plan)
        };
        if e_plan == 0 || e_plan == FTS5_PLAN_SPECIAL {
            return None;
        }
        Some(Fts5AuxTarget {
            core,
            snap: None,
            db_name,
            table_name,
        })
    }

    /// The error of `fts5ApiCallback` for a first argument that names no cursor.
    pub fn no_such_cursor(arg0: &Value) -> SqliteError {
        SqliteError::new(
            ErrorCode::Error,
            format!("no such cursor: {}", arg0.to_integer_lossy()),
        )
    }

    /// `fts5ApiCallback` -> `fts5ApiInvoke`: runs auxiliary function `func` with the
    /// trailing arguments `args` on the cursor of `target`.
    pub fn invoke(
        &self,
        ctx: &mut dyn VTabHostContext,
        target: &Fts5AuxTarget,
        func: &str,
        args: &[Value],
    ) -> Result<Value> {
        let Some(f) = find_auxiliary(func) else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such function: {func}"),
            ));
        };
        let mut core = target.core.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((set, row)) = target.snap.as_ref() {
            let (rowid, blob, miss) = &set.rows[*row];
            core.set_replay(*rowid, blob, miss.as_deref());
        }
        core.api_invoke(ctx, f, args)
    }

    pub fn eval_fts5_source_id() -> Value {
        Value::Text(FTS5_SOURCE_ID.to_owned())
    }

    pub fn eval_fts5_ptr_func(_arg: &Value) -> Value {
        Value::Null
    }
}

// ---------------------------------------------------------------------------------------
// The cursor object the host holds
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
struct Fts5CursorRow {
    rowid: i64,
    cols: Vec<Value>,
    tbl: Value,
    rank: Value,
}

/// `sqlite3_vtab_cursor` of an FTS5 table. A host that steps the cursor row by row
/// ([`VirtualCursor::set_lazy`]) drives the `Fts5Cursor` port directly; otherwise the scan
/// is run when it is filtered and its rows are kept.
#[derive(Debug)]
pub(super) struct Fts5Cursor {
    cfg: Arc<Fts5Config>,
    aux_registry: Arc<Fts5AuxRegistry>,
    state: Arc<Mutex<Fts5SharedState>>,
    core: Arc<Mutex<CsrCore>>,
    i_csr_id: i64,
    lazy: bool,
    b_eof: bool,
    /// `fts5NewTransaction` found no other cursor open: `sqlite3Fts5IndexReset` is due.
    need_reset: bool,
    /// `VTabIndexInfo::plan_hint` of the plan being filtered.
    plan_hint: u64,
    /// The host said which columns it can ask for ([`VirtualCursor::set_fetch_mask`]).
    mask_given: bool,
    rows: Vec<Fts5CursorRow>,
    pos: usize,
}

impl Fts5Cursor {
    /// `fts5OpenMethod`.
    pub(super) fn open(
        cfg: &Arc<Fts5Config>,
        state: &Arc<Mutex<Fts5SharedState>>,
        aux_registry: &Arc<Fts5AuxRegistry>,
    ) -> Self {
        let need_reset = {
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            let first = st.n_open_csr == 0;
            st.n_open_csr += 1;
            first
        };
        let id = aux_registry.alloc_id();
        let core = Arc::new(Mutex::new(CsrCore::new(
            Arc::clone(cfg),
            Arc::clone(state),
            Arc::clone(aux_registry),
            id,
        )));
        aux_registry.register(id, &core);
        Self {
            cfg: Arc::clone(cfg),
            aux_registry: Arc::clone(aux_registry),
            state: Arc::clone(state),
            core,
            i_csr_id: id,
            lazy: false,
            b_eof: true,
            need_reset,
            plan_hint: 0,
            mask_given: false,
            rows: Vec::new(),
            pos: 0,
        }
    }

    fn lock_core(&self) -> std::sync::MutexGuard<'_, CsrCore> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs the scan from the row the cursor is on to its end and keeps the rows.
    fn read_all(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        let mut core = self.core.lock().unwrap_or_else(|e| e.into_inner());
        let n_col = self.cfg.columns.len();
        let needs_content = (self.plan_hint & PLAN_HINT_NEEDS_CONTENT) != 0;
        let needs_rank = (self.plan_hint & PLAN_HINT_NEEDS_RANK_COL) != 0;
        let needs_tbl = (self.plan_hint & PLAN_HINT_NEEDS_TBL_COL) != 0;
        let special = core.e_plan == FTS5_PLAN_SPECIAL;
        let serial = self.aux_registry.next_snap_serial();
        let mut snaps: Vec<SnapRow> = Vec::new();
        while !core.eof() {
            let rowid = core.rowid();
            // before any column of the row is read: reading rank rebuilds the lists
            let snap = if needs_tbl && !special { Some(core.snapshot()) } else { None };
            let mut cols = Vec::new();
            if needs_content && !special {
                cols.reserve(n_col);
                for i in 0..n_col {
                    cols.push(core.column(ctx, i)?);
                }
            }
            let rank = if needs_rank && !special {
                core.column(ctx, n_col + 1)?
            } else {
                Value::Null
            };
            let tbl = if special {
                core.column(ctx, n_col)?
            } else if let Some(snap) = snap {
                snaps.push(snap);
                Value::Integer(
                    FTS5_SNAP_FLAG | (i64::from(serial) << 32) | (snaps.len() as i64),
                )
            } else {
                Value::Null
            };
            self.rows.push(Fts5CursorRow {
                rowid,
                cols,
                tbl,
                rank,
            });
            core.next(ctx)?;
        }
        drop(core);
        if !snaps.is_empty() {
            let s = self.aux_registry.publish_snaps(&self.core, snaps);
            debug_assert_eq!(s, serial);
        }
        self.b_eof = self.rows.is_empty();
        Ok(())
    }
}

impl Drop for Fts5Cursor {
    /// `fts5CloseMethod`.
    fn drop(&mut self) {
        self.aux_registry.unregister(&self.core);
        if let Ok(mut st) = self.state.lock() {
            st.n_open_csr = st.n_open_csr.saturating_sub(1);
        }
        if self.lazy
            && let Ok(mut core) = self.core.lock()
        {
            core.free_cursor_components();
        }
    }
}

impl VirtualCursor for Fts5Cursor {
    fn set_plan_hint(&mut self, hint: u64) {
        if !self.mask_given {
            self.plan_hint = hint;
        }
    }

    fn set_fetch_mask(&mut self, mask: u64) {
        let n_col = self.cfg.columns.len();
        let vis_mask = if n_col == 0 {
            0u64
        } else if n_col >= 64 {
            u64::MAX
        } else {
            (1u64 << n_col) - 1
        };
        let mut hint = 0u64;
        if (mask & vis_mask) != 0 {
            hint |= PLAN_HINT_NEEDS_CONTENT;
        }
        if (mask & (1u64 << n_col.min(63))) != 0 {
            hint |= PLAN_HINT_NEEDS_TBL_COL;
        }
        if (mask & (1u64 << (n_col + 1).min(63))) != 0 {
            hint |= PLAN_HINT_NEEDS_RANK_COL;
        }
        self.plan_hint = hint;
        self.mask_given = true;
    }

    fn set_lazy(&mut self, lazy: bool) -> bool {
        self.lazy = lazy;
        true
    }

    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        self.b_eof = true;
        if self.need_reset {
            // fts5NewTransaction -> sqlite3Fts5StorageReset
            self.need_reset = false;
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.index.reset(ctx)?;
        }
        if !self.lazy {
            // The rows of an earlier scan of this cursor may still be referred to: they
            // keep the cursor object they were read with.
            let detached = {
                let c = self.lock_core();
                c.e_plan != 0
            };
            if detached {
                self.aux_registry.unregister(&self.core);
                let core = Arc::new(Mutex::new(CsrCore::new(
                    Arc::clone(&self.cfg),
                    Arc::clone(&self.state),
                    Arc::clone(&self.aux_registry),
                    self.i_csr_id,
                )));
                self.aux_registry.register(self.i_csr_id, &core);
                self.core = core;
            }
        }
        {
            let mut core = self.core.lock().unwrap_or_else(|e| e.into_inner());
            core.filter(ctx, idx_num, idx_str, args)?;
            if self.lazy {
                self.b_eof = core.eof();
                return Ok(());
            }
        }
        self.read_all(ctx)
    }

    fn unlazy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if self.lazy {
            self.lazy = false;
            self.read_all(ctx)?;
        }
        Ok(())
    }


    fn next(&mut self) -> Result<()> {
        if self.pos < self.rows.len() {
            self.pos += 1;
        }
        self.b_eof = self.pos >= self.rows.len();
        Ok(())
    }

    fn next_ctx(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if !self.lazy {
            return self.next();
        }
        let mut core = self.core.lock().unwrap_or_else(|e| e.into_inner());
        let r = core.next(ctx);
        self.b_eof = core.eof() || r.is_err();
        r
    }

    fn eof(&self) -> bool {
        self.b_eof
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let Some(row) = self.rows.get(self.pos) else {
            return Ok(Value::Null);
        };
        let n_cols = self.cfg.columns.len();
        if col_idx < n_cols {
            Ok(row.cols.get(col_idx).cloned().unwrap_or(Value::Null))
        } else if col_idx == n_cols {
            Ok(row.tbl.clone())
        } else if col_idx == n_cols + 1 {
            Ok(row.rank.clone())
        } else {
            Ok(Value::Null)
        }
    }

    fn column_ctx(&mut self, ctx: &mut dyn VTabHostContext, col_idx: usize) -> Result<Value> {
        if !self.lazy {
            return self.column(col_idx);
        }
        let mut core = self.core.lock().unwrap_or_else(|e| e.into_inner());
        core.column(ctx, col_idx)
    }

    fn rowid(&self) -> Result<i64> {
        if self.lazy {
            return Ok(self.lock_core().rowid());
        }
        Ok(self.rows.get(self.pos).map_or(0, |r| r.rowid))
    }
}
