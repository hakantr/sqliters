//! Cursor of `ext/fts5/fts5_vocab.c` (`fts5vocab` tables of type row / col / instance).
#![forbid(unsafe_code)]

use super::*;
use crate::fts5_index::{
    FTS5INDEX_QUERY_NOTOKENDATA, FTS5INDEX_QUERY_SCAN, MultiIter, get_varint32, poslist_next64,
};

const FTS5_VOCAB_TERM_EQ: i32 = 0x0100;
const FTS5_VOCAB_TERM_GE: i32 = 0x0200;
const FTS5_VOCAB_TERM_LE: i32 = 0x0400;
const FTS5_VOCAB_COLUSED_MASK: i32 = 0xFF;

/// `Fts5VocabCursor`.
#[derive(Debug)]
pub(super) struct Fts5VocabCursor {
    pub(super) aux_registry: Arc<Fts5AuxRegistry>,
    pub(super) target_db: Option<String>,
    pub(super) target_tbl: String,
    pub(super) vtype: VocabType,
    /// `pFts5`: the table the cursor reads (found when the cursor is first filtered).
    fts5: Option<(Arc<Mutex<Fts5SharedState>>, Arc<Fts5Config>)>,
    lazy: bool,
    b_eof: bool,
    iter: Option<MultiIter>,
    /// `pStruct`: the structure object of the index when the iterator was opened.
    struct_gen: u64,
    le_term: Option<Vec<u8>>,
    col_used: i32,
    i_col: usize,
    a_cnt: Vec<i64>,
    a_doc: Vec<i64>,
    rowid: i64,
    term: Vec<u8>,
    i_inst_pos: i64,
    i_inst_off: i32,
    rows: Vec<(i64, Vec<Value>)>,
    pos: usize,
}

impl Fts5VocabCursor {
    pub(super) fn new(
        aux_registry: Arc<Fts5AuxRegistry>,
        target_db: Option<String>,
        target_tbl: String,
        vtype: VocabType,
    ) -> Self {
        Self {
            aux_registry,
            target_db,
            target_tbl,
            vtype,
            fts5: None,
            lazy: false,
            b_eof: true,
            iter: None,
            struct_gen: 0,
            le_term: None,
            col_used: 0,
            i_col: 0,
            a_cnt: Vec::new(),
            a_doc: Vec::new(),
            rowid: 0,
            term: Vec::new(),
            i_inst_pos: 0,
            i_inst_off: 0,
            rows: Vec::new(),
            pos: 0,
        }
    }

    /// `fts5VocabOpenMethod`: finds the FTS5 table and flushes its pending data.
    fn open_fts5(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if self.fts5.is_some() {
            return Ok(());
        }
        let db = self
            .target_db
            .clone()
            .unwrap_or_else(|| ctx.db_name().to_owned());
        let tbl_sq = quote_single(&self.target_tbl);
        let sql = format!(
            "SELECT t.{tbl_sq} FROM {}.{tbl_sq} AS t WHERE t.{tbl_sq} MATCH '*id'",
            quote_single(&db)
        );
        let no_such = || {
            SqliteError::new(
                ErrorCode::Error,
                format!("no such fts5 table: {db}.{}", self.target_tbl),
            )
        };
        let found = match ctx.query_sql(&sql, &[]) {
            Ok(rows) => !rows.is_empty(),
            Err(e) if e.code == ErrorCode::Error => false,
            Err(e) => return Err(e),
        };
        let entry = if found {
            self.aux_registry.find_table(&db, &self.target_tbl)
        } else {
            None
        };
        let Some(entry) = entry else {
            return Err(no_such());
        };
        // sqlite3Fts5FlushToDisk
        self.aux_registry.trip_cursors(&entry.0);
        ctx.flush_vtab_pending(Some(&db), &self.target_tbl)?;
        let n_col = entry.1.columns.len();
        self.a_cnt = vec![0; n_col];
        self.a_doc = vec![0; n_col];
        self.fts5 = Some(entry);
        Ok(())
    }

    /// `fts5VocabResetCursor`.
    fn reset_cursor(&mut self) {
        self.rowid = 0;
        self.iter = None;
        self.le_term = None;
        self.b_eof = false;
        self.i_col = 0;
        self.i_inst_pos = 0;
        self.i_inst_off = 0;
        self.col_used = 0;
        self.a_cnt.fill(0);
        self.a_doc.fill(0);
    }

    /// The `nLeTerm>=0` test of `fts5VocabInstanceNewTerm` / `fts5VocabNextMethod`.
    fn past_le_term(&self, term: &[u8]) -> bool {
        if let Some(le) = self.le_term.as_deref() {
            let n_cmp = term.len().min(le.len());
            let b_cmp = le[..n_cmp].cmp(&term[..n_cmp]);
            if b_cmp == std::cmp::Ordering::Less
                || (b_cmp == std::cmp::Ordering::Equal && le.len() < term.len())
            {
                return true;
            }
        }
        false
    }

    /// `fts5VocabInstanceNewTerm`.
    fn instance_new_term(&mut self) {
        let Some(it) = self.iter.as_ref() else {
            self.b_eof = true;
            return;
        };
        if it.eof() {
            self.b_eof = true;
        } else {
            let term = it.term().to_vec();
            if self.past_le_term(&term) {
                self.b_eof = true;
            }
            self.term = term;
        }
    }

    /// `fts5VocabInstanceNext`.
    fn instance_next(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let Some((state, cfg)) = self.fts5.clone() else {
            return Ok(());
        };
        let detail = cfg.detail;
        loop {
            let more = detail == DetailMode::None || {
                let Some(it) = self.iter.as_ref() else {
                    return Ok(());
                };
                poslist_next64(it.data(), it.n_data, &mut self.i_inst_off, &mut self.i_inst_pos)
            };
            if !more {
                break;
            }
            self.i_inst_pos = 0;
            self.i_inst_off = 0;
            let r = {
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                match self.iter.as_mut() {
                    Some(it) => st.index.iter_next_scan(ctx, it),
                    None => Ok(()),
                }
            };
            match r {
                Ok(()) => {
                    self.instance_new_term();
                    if self.b_eof || detail == DetailMode::None {
                        break;
                    }
                }
                Err(e) => {
                    self.b_eof = true;
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// `fts5VocabNextMethod`.
    fn next_row(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let Some((state, cfg)) = self.fts5.clone() else {
            self.b_eof = true;
            return Ok(());
        };
        let n_col = cfg.columns.len();
        // sqlite3Fts5StructureTest
        if state.lock().unwrap_or_else(|e| e.into_inner()).index.struct_gen() != self.struct_gen {
            return Err(SqliteError::new(ErrorCode::Abort, ErrorCode::Abort.as_str()));
        }
        self.rowid += 1;
        if self.vtype == VocabType::Instance {
            return self.instance_next(ctx);
        }
        if self.vtype == VocabType::Col {
            self.i_col += 1;
            while self.i_col < n_col {
                if self.a_doc[self.i_col] != 0 {
                    break;
                }
                self.i_col += 1;
            }
        }
        if self.vtype != VocabType::Col || self.i_col >= n_col {
            let at_eof = self.iter.as_ref().is_none_or(MultiIter::eof);
            if at_eof {
                self.b_eof = true;
            } else {
                let term = self.iter.as_ref().map(|it| it.term().to_vec()).unwrap_or_default();
                if self.past_le_term(&term) {
                    self.b_eof = true;
                    return Ok(());
                }
                self.term = term;
                self.a_cnt.fill(0);
                self.a_doc.fill(0);
                self.i_col = 0;
                let detail = cfg.detail;
                // (the index is taken once for all the rows of the term)
                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    {
                        let Some(it) = self.iter.as_ref() else {
                            break;
                        };
                        let p_pos = it.data();
                        let n_pos = it.n_data;
                        match self.vtype {
                            VocabType::Row => {
                                // Do not bother counting the number of instances if the
                                // "cnt" column is not being read (according to colUsed).
                                if detail == DetailMode::Full && (self.col_used & 0x04) != 0 {
                                    let mut i_pos = 0usize;
                                    while (i_pos as i64) < i64::from(n_pos) {
                                        let (ii, n) = get_varint32(p_pos, i_pos);
                                        i_pos += n;
                                        if ii == 1 {
                                            // New column in the position list
                                            let (_, n) = get_varint32(p_pos, i_pos);
                                            i_pos += n;
                                        } else {
                                            // An instance - increment pCsr->aCnt[]
                                            self.a_cnt[0] += 1;
                                        }
                                    }
                                }
                                self.a_doc[0] += 1;
                            }
                            VocabType::Col => {
                                let mut i_pos: i64 = 0;
                                let mut i_off: i32 = 0;
                                if detail == DetailMode::Full {
                                    let mut i_c: i32 = -1;
                                    while !poslist_next64(p_pos, n_pos, &mut i_off, &mut i_pos) {
                                        let ii = ((i_pos >> 32) & 0x7FFF_FFFF) as i32;
                                        if i_c != ii {
                                            if ii as usize >= n_col {
                                                return Err(corrupt_vtab());
                                            }
                                            self.a_doc[ii as usize] += 1;
                                            i_c = ii;
                                        }
                                        self.a_cnt[ii as usize] += 1;
                                    }
                                } else if detail == DetailMode::Col {
                                    while !poslist_next64(p_pos, n_pos, &mut i_off, &mut i_pos) {
                                        if i_pos < 0 || i_pos >= n_col as i64 {
                                            return Err(corrupt_vtab());
                                        }
                                        self.a_doc[i_pos as usize] += 1;
                                    }
                                } else {
                                    self.a_doc[0] += 1;
                                }
                            }
                            VocabType::Instance => {}
                        }
                    }
                    if let Some(it) = self.iter.as_mut() {
                        st.index.iter_next_scan(ctx, it)?;
                    }
                    let Some(it) = self.iter.as_ref() else {
                        break;
                    };
                    if it.term() != self.term.as_slice() {
                        break;
                    }
                    if it.eof() {
                        break;
                    }
                }
            }
        }
        if !self.b_eof && self.vtype == VocabType::Col {
            while self.i_col < n_col && self.a_doc[self.i_col] == 0 {
                self.i_col += 1;
            }
            if self.i_col == n_col {
                return Err(corrupt_vtab());
            }
        }
        Ok(())
    }

    /// `fts5VocabFilterMethod`.
    fn filter_impl(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        ap_val: &[Value],
    ) -> Result<()> {
        self.open_fts5(ctx)?;
        let Some((state, cfg)) = self.fts5.clone() else {
            return Ok(());
        };
        let mut i_val = 0usize;
        let mut f = FTS5INDEX_QUERY_SCAN;
        let mut p_eq: Option<&Value> = None;
        let mut p_ge: Option<&Value> = None;
        let mut p_le: Option<&Value> = None;
        self.reset_cursor();
        fn text_only(v: Option<&Value>) -> Option<&Value> {
            v.filter(|v| matches!(v, Value::Text(_)))
        }
        if (idx_num & FTS5_VOCAB_TERM_EQ) != 0 {
            p_eq = text_only(ap_val.get(i_val));
            i_val += 1;
        }
        if (idx_num & FTS5_VOCAB_TERM_GE) != 0 {
            p_ge = text_only(ap_val.get(i_val));
            i_val += 1;
        }
        if (idx_num & FTS5_VOCAB_TERM_LE) != 0 {
            p_le = text_only(ap_val.get(i_val));
        }
        self.col_used = idx_num & FTS5_VOCAB_COLUSED_MASK;
        let as_bytes = |v: &Value| -> Vec<u8> {
            match v {
                Value::Text(s) => s.as_bytes().to_vec(),
                _ => Vec::new(),
            }
        };
        let mut z_term: Vec<u8> = Vec::new();
        if let Some(eq) = p_eq {
            z_term = as_bytes(eq);
            f = FTS5INDEX_QUERY_NOTOKENDATA;
        } else {
            if let Some(ge) = p_ge {
                z_term = as_bytes(ge);
            }
            if let Some(le) = p_le {
                self.le_term = Some(as_bytes(le));
            }
        }
        {
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            let it = st.index.query(ctx, &z_term, f, None)?;
            self.iter = Some(it);
            self.struct_gen = st.index.struct_gen();
        }
        sqliters_types::dtrace!(
            "TR|f5q|vfilter|type={} idx={}",
            match self.vtype {
                VocabType::Col => 0,
                VocabType::Row => 1,
                VocabType::Instance => 2,
            },
            idx_num
        );
        if self.vtype == VocabType::Instance {
            self.instance_new_term();
        }
        if !self.b_eof && (self.vtype != VocabType::Instance || cfg.detail != DetailMode::None) {
            self.next_row(ctx)?;
        }
        Ok(())
    }

    /// `fts5VocabColumnMethod`.
    fn column_value(&self, i_col: usize) -> Value {
        let Some((_, cfg)) = self.fts5.as_ref() else {
            return Value::Null;
        };
        let detail = cfg.detail;
        let mut i_val: i64 = 0;
        if i_col == 0 {
            return Value::Text(String::from_utf8_lossy(&self.term).into_owned());
        }
        match self.vtype {
            VocabType::Col => {
                if i_col == 1 {
                    if detail != DetailMode::None {
                        return cfg
                            .columns
                            .get(self.i_col)
                            .map_or(Value::Null, |c| Value::Text(c.clone()));
                    }
                } else if i_col == 2 {
                    i_val = self.a_doc.get(self.i_col).copied().unwrap_or(0);
                } else {
                    i_val = self.a_cnt.get(self.i_col).copied().unwrap_or(0);
                }
            }
            VocabType::Row => {
                if i_col == 1 {
                    i_val = self.a_doc.first().copied().unwrap_or(0);
                } else {
                    i_val = self.a_cnt.first().copied().unwrap_or(0);
                }
            }
            VocabType::Instance => match i_col {
                1 => {
                    return Value::Integer(self.iter.as_ref().map_or(0, |it| it.i_rowid));
                }
                2 => {
                    let mut ii: i64 = -1;
                    if detail == DetailMode::Full {
                        ii = (self.i_inst_pos >> 32) & 0x7FFF_FFFF;
                    } else if detail == DetailMode::Col {
                        ii = i64::from(self.i_inst_pos as i32);
                    }
                    if ii >= 0 && (ii as usize) < cfg.columns.len() {
                        return Value::Text(cfg.columns[ii as usize].clone());
                    }
                    return Value::Null;
                }
                _ => {
                    if detail == DetailMode::Full {
                        return Value::Integer(self.i_inst_pos & 0x7FFF_FFFF);
                    }
                    return Value::Null;
                }
            },
        }
        if i_val > 0 {
            Value::Integer(i_val)
        } else {
            Value::Null
        }
    }

    /// Runs the scan from the row the cursor is on to its end and keeps the rows.
    fn read_all(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        let n = self.n_columns();
        while !self.b_eof {
            let vals: Vec<Value> = (0..n).map(|i| self.column_value(i)).collect();
            self.rows.push((self.rowid, vals));
            self.next_row(ctx)?;
        }
        self.iter = None;
        self.b_eof = self.rows.is_empty();
        Ok(())
    }

    /// Runs `f` with a host context of the FTS5 table's schema: the index reads
    /// `%_data` / `%_idx` there, wherever the fts5vocab table itself is.
    fn in_target(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        f: impl FnOnce(&mut Self, &mut dyn VTabHostContext) -> Result<()>,
    ) -> Result<()> {
        let same = self
            .target_db
            .as_deref()
            .is_none_or(|db| db.eq_ignore_ascii_case(ctx.db_name()));
        if same {
            return f(self, ctx);
        }
        let db = self.target_db.clone().unwrap_or_default();
        let tbl = self.target_tbl.clone();
        let mut f = Some(f);
        ctx.with_table(&db, &tbl, &mut |c| match f.take() {
            Some(f) => f(self, c),
            None => Ok(()),
        })
    }

    /// The index reports corruption through `pConfig->pzErrmsg`, which is only set while a
    /// method of the FTS5 table itself runs: from an fts5vocab cursor the error has the
    /// result code alone.
    fn vocab_err(e: SqliteError) -> SqliteError {
        if e.code == ErrorCode::Corrupt {
            // (the code stays what it was: SQLITE_CORRUPT_VTAB from the index, plain
            // SQLITE_CORRUPT from a transaction that already failed)
            let mut e = e;
            e.message = "database disk image is malformed".to_owned();
            return e;
        }
        e
    }

    fn n_columns(&self) -> usize {
        match self.vtype {
            VocabType::Row => 3,
            _ => 4,
        }
    }
}

fn corrupt_vtab() -> SqliteError {
    SqliteError::with_extended(FTS5_CORRUPT, "database disk image is malformed")
}

impl VirtualCursor for Fts5VocabCursor {
    fn set_lazy(&mut self, lazy: bool) -> bool {
        self.lazy = lazy;
        true
    }

    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        self.b_eof = true;
        self.in_target(ctx, |me, c| {
            me.filter_impl(c, idx_num, args)?;
            if !me.lazy {
                me.read_all(c)?;
            }
            Ok(())
        })
        .map_err(Self::vocab_err)
    }

    fn unlazy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if self.lazy {
            self.lazy = false;
            self.in_target(ctx, |me, c| me.read_all(c))
                .map_err(Self::vocab_err)?;
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
        self.in_target(ctx, |me, c| me.next_row(c))
            .map_err(Self::vocab_err)
    }

    fn eof(&self) -> bool {
        self.b_eof
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        if self.lazy {
            return Ok(self.column_value(col_idx));
        }
        Ok(self
            .rows
            .get(self.pos)
            .and_then(|r| r.1.get(col_idx))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        if self.lazy {
            return Ok(self.rowid);
        }
        Ok(self.rows.get(self.pos).map_or(0, |r| r.0))
    }
}
