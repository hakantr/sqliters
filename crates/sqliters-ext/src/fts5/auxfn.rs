//! Port of `ext/fts5/fts5_aux.c`: the built-in auxiliary functions `bm25()`, `highlight()`,
//! `snippet()` and `fts5_get_locale()`, written against the extension API of the cursor
//! (`CsrCore::api_*` = `Fts5ExtensionApi`).
#![forbid(unsafe_code)]

use super::main::CsrCore;
use super::*;
use crate::fts5_index::rc_only;

/// `sqlite3_result_error_code(pCtx, rc)`: the error an auxiliary function reports when an
/// API call fails is the result code with its standard text.
fn result_error_code(e: SqliteError) -> SqliteError {
    rc_only(e)
}

/// `CInstIter`: iterates the phrase instances of one column, treating overlapping
/// instances as a single match.
#[derive(Debug, Default, Clone, Copy)]
struct CInstIter {
    i_col: i32,
    i_inst: i32,
    n_inst: i32,
    i_start: i32,
    i_end: i32,
}

impl CInstIter {
    /// `fts5CInstIterNext`.
    fn next(&mut self, core: &mut CsrCore, ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.i_start = -1;
        self.i_end = -1;
        while self.i_inst < self.n_inst {
            let (ip, ic, io) = core.api_inst(ctx, self.i_inst)?;
            if ic == self.i_col {
                let i_end = io - 1 + core.api_phrase_size(ip);
                if self.i_start < 0 {
                    self.i_start = io;
                    self.i_end = i_end;
                } else if io <= self.i_end {
                    if i_end > self.i_end {
                        self.i_end = i_end;
                    }
                } else {
                    break;
                }
            }
            self.i_inst += 1;
        }
        Ok(())
    }

    /// `fts5CInstIterInit`.
    fn init(core: &mut CsrCore, ctx: &mut dyn VTabHostContext, i_col: i32) -> Result<Self> {
        let mut it = Self {
            i_col,
            ..Self::default()
        };
        it.n_inst = core.api_inst_count(ctx)?;
        it.next(core, ctx)?;
        Ok(it)
    }
}

/// `HighlightContext`.
struct HighlightContext<'a> {
    i_range_start: i32,
    i_range_end: i32,
    z_open: &'a [u8],
    z_close: &'a [u8],
    z_in: &'a [u8],
    iter: CInstIter,
    i_pos: i32,
    i_off: i32,
    b_open: bool,
    z_out: Vec<u8>,
    rc: Result<()>,
}

/// The bytes of `z` up to its first 0x00 (`strlen`).
fn cstr(z: &[u8]) -> &[u8] {
    &z[..z.iter().position(|&b| b == 0).unwrap_or(z.len())]
}

impl HighlightContext<'_> {
    /// `fts5HighlightAppend(&rc, p, z, -1)`.
    fn append_str(&mut self, z: &[u8]) {
        if self.rc.is_ok() {
            self.z_out.extend_from_slice(cstr(z));
        }
    }

    /// `fts5HighlightAppend(&rc, p, &p->zIn[iOff], n)`: `%.*s` copies at most `n` bytes
    /// and stops at a 0x00; a negative `n` means "up to the terminator".
    fn append_in(&mut self, i_off: i32, n: i32) {
        if self.rc.is_err() {
            return;
        }
        let start = (i_off.max(0) as usize).min(self.z_in.len());
        let rest = &self.z_in[start..];
        let lim = if n < 0 { rest.len() } else { (n as usize).min(rest.len()) };
        self.z_out.extend_from_slice(cstr(&rest[..lim]));
    }

    /// `fts5HighlightCb`.
    fn token(
        &mut self,
        core: &mut CsrCore,
        ctx: &mut dyn VTabHostContext,
        i_start_off: i32,
        i_end_off: i32,
    ) {
        if self.rc.is_err() {
            return;
        }
        let i_pos = self.i_pos;
        self.i_pos += 1;
        if self.i_range_end >= 0 {
            if i_pos < self.i_range_start || i_pos > self.i_range_end {
                return;
            }
            if self.i_range_start != 0 && i_pos == self.i_range_start {
                self.i_off = i_start_off;
            }
        }
        // If the parenthesis is open, and this token is not part of the current phrase,
        // and the starting byte offset of this token is past the point that has currently
        // been copied into the output buffer, close the parenthesis.
        if self.b_open
            && (i_pos <= self.iter.i_start || self.iter.i_start < 0)
            && i_start_off > self.i_off
        {
            let c = self.z_close;
            self.append_str(c);
            self.b_open = false;
        }
        // If this is the start of a new phrase, and the highlight is not open:
        //
        //   * copy text from the input up to the start of the phrase, and
        //   * open the highlight.
        if i_pos == self.iter.i_start && !self.b_open {
            self.append_in(self.i_off, i_start_off - self.i_off);
            let o = self.z_open;
            self.append_str(o);
            self.i_off = i_start_off;
            self.b_open = true;
        }
        if i_pos == self.iter.i_end {
            if !self.b_open {
                let o = self.z_open;
                self.append_str(o);
                self.b_open = true;
            }
            self.append_in(self.i_off, i_end_off - self.i_off);
            self.i_off = i_end_off;
            if self.rc.is_ok() {
                let mut it = self.iter;
                self.rc = it.next(core, ctx);
                self.iter = it;
            }
        }
        if i_pos == self.i_range_end {
            if self.b_open {
                if self.iter.i_start >= 0 && i_pos >= self.iter.i_start {
                    self.append_in(self.i_off, i_end_off - self.i_off);
                    self.i_off = i_end_off;
                }
                let c = self.z_close;
                self.append_str(c);
                self.b_open = false;
            }
            self.append_in(self.i_off, i_end_off - self.i_off);
            self.i_off = i_end_off;
        }
    }
}

/// `fts5ValueClampedInt`.
fn value_clamped_int(v: &Value) -> i32 {
    v.to_integer_lossy().clamp(-1, 0x7FFF_FFFF) as i32
}

/// `sqlite3_result_text(pCtx, z, -1, ...)`.
fn result_text(z: &[u8]) -> Value {
    Value::Text(String::from_utf8_lossy(cstr(z)).into_owned())
}

/// `fts5HighlightFunction`.
fn highlight_function(
    core: &mut CsrCore,
    ctx: &mut dyn VTabHostContext,
    ap_val: &[Value],
) -> Result<Value> {
    if ap_val.len() != 3 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "wrong number of arguments to function highlight()",
        ));
    }
    let i_col = value_clamped_int(&ap_val[0]);
    let z_open = fts5_value_bytes(&ap_val[1]);
    let z_close = fts5_value_bytes(&ap_val[2]);
    let z_in = match core.api_column_text(ctx, i_col) {
        Err(e) if e.code == ErrorCode::Range => return Ok(Value::Text(String::new())),
        Err(e) => return Err(result_error_code(e)),
        Ok(None) => return Ok(Value::Null),
        Ok(Some(z)) => z,
    };
    let cfg = Arc::clone(&core.cfg);
    let mut hc = HighlightContext {
        i_range_start: 0,
        i_range_end: -1,
        z_open: z_open.as_deref().unwrap_or(b""),
        z_close: z_close.as_deref().unwrap_or(b""),
        z_in: &z_in,
        iter: CInstIter::default(),
        i_pos: 0,
        i_off: 0,
        b_open: false,
        z_out: Vec::new(),
        rc: Ok(()),
    };
    match CInstIter::init(core, ctx, i_col) {
        Ok(it) => hc.iter = it,
        Err(e) => hc.rc = Err(e),
    }
    if hc.rc.is_ok() {
        hc.rc = core.api_column_locale(ctx, i_col).map(|_| ());
    }
    if hc.rc.is_ok() {
        cfg.tokenizer.tok.tokenize(cstr_all(&z_in), &mut |_, s, e| {
            hc.token(core, ctx, s as i32, e as i32);
        });
    }
    if hc.b_open {
        let c = hc.z_close;
        hc.append_str(c);
    }
    let n_in = z_in.len() as i32;
    hc.append_in(hc.i_off, n_in - hc.i_off);
    match hc.rc {
        Ok(()) => Ok(result_text(&hc.z_out)),
        Err(e) => Err(result_error_code(e)),
    }
}

/// The text handed to the tokenizer (`pText`, `nText`): all of it.
fn cstr_all(z: &[u8]) -> &[u8] {
    z
}

/// `fts5SnippetScore`: (score, adjusted start).
#[allow(clippy::too_many_arguments)]
fn snippet_score(
    core: &mut CsrCore,
    ctx: &mut dyn VTabHostContext,
    n_docsize: i32,
    a_seen: &mut [u8],
    i_col: i32,
    i_pos: i32,
    n_token: i32,
) -> Result<(i32, i32)> {
    let mut i_first: i32 = -1;
    let mut n_score: i32 = 0;
    let mut i_last: i32 = 0;
    let i_end = i64::from(i_pos) + i64::from(n_token);
    let n_inst = core.api_inst_count(ctx)?;
    for i in 0..n_inst {
        let (ip, ic, i_off) = core.api_inst(ctx, i)?;
        if ic == i_col && i_off >= i_pos && i64::from(i_off) < i_end {
            let seen = a_seen.get(ip as usize).copied().unwrap_or(0);
            n_score += if seen != 0 { 1 } else { 1000 };
            if let Some(s) = a_seen.get_mut(ip as usize) {
                *s = 1;
            }
            if i_first < 0 {
                i_first = i_off;
            }
            i_last = i_off + core.api_phrase_size(ip);
        }
    }
    let mut i_adj =
        i64::from(i_first) - (i64::from(n_token) - (i64::from(i_last) - i64::from(i_first))) / 2;
    if i_adj + i64::from(n_token) > i64::from(n_docsize) {
        i_adj = i64::from(n_docsize) - i64::from(n_token);
    }
    if i_adj < 0 {
        i_adj = 0;
    }
    Ok((n_score, i_adj as i32))
}

/// `fts5SnippetFunction`.
fn snippet_function(
    core: &mut CsrCore,
    ctx: &mut dyn VTabHostContext,
    ap_val: &[Value],
) -> Result<Value> {
    if ap_val.len() != 5 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "wrong number of arguments to function snippet()",
        ));
    }
    let cfg = Arc::clone(&core.cfg);
    let n_col = core.api_column_count();
    let i_col = value_clamped_int(&ap_val[0]);
    let z_open = fts5_value_bytes(&ap_val[1]).unwrap_or_default();
    let z_close = fts5_value_bytes(&ap_val[2]).unwrap_or_default();
    let z_ellips = fts5_value_bytes(&ap_val[3]).unwrap_or_default();
    let n_token = ap_val[4].to_integer_lossy().clamp(0, 64) as i32;
    let mut i_best_col = if i_col >= 0 { i_col } else { 0 };
    let n_phrase = core.api_phrase_count().max(0) as usize;
    let mut a_seen = vec![0u8; n_phrase];
    let mut i_best_start = 0i32;
    let mut n_best_score = 0i32;
    let mut n_col_size = 0i32;

    let mut rc: Result<()> = Ok(());
    let mut n_inst = 0i32;
    if n_phrase == 0 {
        // `aSeen = sqlite3_malloc64(nPhrase)` is NULL for a cursor without phrases (a scan
        // or a rowid lookup): `rc = SQLITE_NOMEM`, xInstCount is not called. The value
        // survives only when no column is visited below (a column number out of range).
        rc = Err(SqliteError::new(ErrorCode::NoMem, "out of memory"));
    } else {
        match core.api_inst_count(ctx) {
            Ok(n) => n_inst = n,
            Err(e) => rc = Err(e),
        }
    }

    // (the loop over the columns runs even when xInstCount failed: its first API call
    // overwrites rc)
    let mut i = 0i32;
    while i < n_col {
        if i_col < 0 || i_col == i {
            // Fts5SFinder
            let mut a_first: Vec<i32> = Vec::new();
            let z_doc = match core.api_column_text(ctx, i) {
                Ok(z) => z,
                Err(e) => {
                    rc = Err(e);
                    break;
                }
            };
            if let Err(e) = core.api_column_locale(ctx, i) {
                rc = Err(e);
                break;
            }
            rc = Ok(());
            if let Some(doc) = z_doc.as_deref() {
                let mut i_pos = 0i32;
                // fts5SentenceFinderCb
                cfg.tokenizer.tok.tokenize(doc, &mut |_, i_start_off, _| {
                    if i_pos > 0 {
                        let mut c = 0u8;
                        let mut k = i_start_off as i64 - 1;
                        while k >= 0 {
                            c = doc[k as usize];
                            if c != b' ' && c != b'\t' && c != b'\n' && c != b'\r' {
                                break;
                            }
                            k -= 1;
                        }
                        if k != i_start_off as i64 - 1 && (c == b'.' || c == b':') {
                            a_first.push(i_pos);
                        }
                    } else {
                        a_first.push(0);
                    }
                    i_pos += 1;
                });
            }
            let n_docsize = match core.api_column_size(ctx, i) {
                Ok(n) => n,
                Err(e) => {
                    rc = Err(e);
                    break;
                }
            };
            let mut ii = 0i32;
            while rc.is_ok() && ii < n_inst {
                let this = ii;
                ii += 1;
                let (_ip, ic, io) = match core.api_inst(ctx, this) {
                    Ok(v) => v,
                    Err(e) => {
                        rc = Err(e);
                        continue;
                    }
                };
                if ic != i {
                    continue;
                }
                if io > n_docsize {
                    rc = Err(SqliteError::with_extended(
                        FTS5_CORRUPT,
                        "database disk image is malformed",
                    ));
                    continue;
                }
                a_seen.fill(0);
                match snippet_score(core, ctx, n_docsize, &mut a_seen, i, io, n_token) {
                    Ok((n_score, i_adj)) => {
                        if n_score > n_best_score {
                            n_best_score = n_score;
                            i_best_col = i;
                            i_best_start = i_adj;
                            n_col_size = n_docsize;
                        }
                    }
                    Err(e) => rc = Err(e),
                }
                if rc.is_ok() && !a_first.is_empty() && n_docsize > n_token {
                    let mut jj = 0usize;
                    while jj + 1 < a_first.len() {
                        if a_first[jj + 1] > io {
                            break;
                        }
                        jj += 1;
                    }
                    if a_first[jj] < io {
                        a_seen.fill(0);
                        match snippet_score(
                            core, ctx, n_docsize, &mut a_seen, i, a_first[jj], n_token,
                        ) {
                            Ok((mut n_score, _)) => {
                                n_score += if a_first[jj] == 0 { 120 } else { 100 };
                                if n_score > n_best_score {
                                    n_best_score = n_score;
                                    i_best_col = i;
                                    i_best_start = a_first[jj];
                                    n_col_size = n_docsize;
                                }
                            }
                            Err(e) => rc = Err(e),
                        }
                    }
                }
            }
        }
        i += 1;
    }

    let mut z_in: Option<Vec<u8>> = None;
    if rc.is_ok() {
        match core.api_column_text(ctx, i_best_col) {
            Ok(z) => z_in = z,
            Err(e) => rc = Err(e),
        }
    }
    if rc.is_ok() && n_col_size == 0 {
        match core.api_column_size(ctx, i_best_col) {
            Ok(n) => n_col_size = n,
            Err(e) => rc = Err(e),
        }
    }
    let mut z_out: Vec<u8> = Vec::new();
    if let Some(zin) = z_in.as_deref() {
        let mut hc = HighlightContext {
            i_range_start: 0,
            i_range_end: -1,
            z_open: &z_open,
            z_close: &z_close,
            z_in: zin,
            iter: CInstIter::default(),
            i_pos: 0,
            i_off: 0,
            b_open: false,
            z_out: Vec::new(),
            rc,
        };
        if hc.rc.is_ok() {
            match CInstIter::init(core, ctx, i_best_col) {
                Ok(it) => hc.iter = it,
                Err(e) => hc.rc = Err(e),
            }
        }
        hc.i_range_start = i_best_start;
        hc.i_range_end = i_best_start + n_token - 1;
        if i_best_start > 0 {
            hc.append_str(&z_ellips);
        }
        // Advance iterator ctx.iter so that it points to the first coalesced phrase
        // instance at or following position iBestStart.
        while hc.iter.i_start >= 0 && hc.iter.i_start < i_best_start && hc.rc.is_ok() {
            let mut it = hc.iter;
            hc.rc = it.next(core, ctx);
            hc.iter = it;
        }
        if hc.rc.is_ok() {
            hc.rc = core.api_column_locale(ctx, i_best_col).map(|_| ());
        }
        if hc.rc.is_ok() {
            cfg.tokenizer.tok.tokenize(zin, &mut |_, s, e| {
                hc.token(core, ctx, s as i32, e as i32);
            });
        }
        if hc.b_open {
            let c = hc.z_close;
            hc.append_str(c);
        }
        if hc.i_range_end >= (n_col_size - 1) {
            let n_in = zin.len() as i32;
            hc.append_in(hc.i_off, n_in - hc.i_off);
        } else {
            hc.append_str(&z_ellips);
        }
        rc = hc.rc;
        z_out = hc.z_out;
        match rc {
            Ok(()) => return Ok(result_text(&z_out)),
            Err(e) => return Err(result_error_code(e)),
        }
    }
    match rc {
        // (`sqlite3_result_text(pCtx, 0, -1)`: NULL)
        Ok(()) => {
            let _ = z_out;
            Ok(Value::Null)
        }
        Err(e) => Err(result_error_code(e)),
    }
}

/// `Fts5Bm25Data`.
#[derive(Debug)]
struct Bm25Data {
    n_phrase: usize,
    avgdl: f64,
    a_idf: Vec<f64>,
    a_freq: Vec<f64>,
}

/// `fts5Bm25GetData`.
fn bm25_get_data(core: &mut CsrCore, ctx: &mut dyn VTabHostContext) -> Result<Box<Bm25Data>> {
    if let Some(p) = core.api_take_auxdata() {
        if let Ok(d) = p.downcast::<Bm25Data>() {
            return Ok(d);
        }
    }
    let n_phrase = core.api_phrase_count().max(0) as usize;
    let mut p = Box::new(Bm25Data {
        n_phrase,
        avgdl: 0.0,
        a_idf: vec![0.0; n_phrase],
        a_freq: vec![0.0; n_phrase],
    });
    // Calculate the average document length for this FTS5 table
    let n_row = core.api_row_count(ctx)?;
    let n_token = core.api_column_total_size(ctx, -1)?;
    p.avgdl = (n_token as f64) / (n_row as f64);
    // Calculate an IDF for each phrase in the query
    for i in 0..n_phrase {
        let n_hit: i64 = core.api_query_phrase_count(ctx, i as i32)?;
        // Calculate the IDF (Inverse Document Frequency) for phrase i. This is done using
        // the standard BM25 formula as found on wikipedia:
        //
        //   IDF = log( (N - nHit + 0.5) / (nHit + 0.5) )
        //
        // The minimum allowable IDF is (1e-6).
        let mut idf = (((n_row - n_hit) as f64 + 0.5) / (n_hit as f64 + 0.5)).ln();
        if idf <= 0.0 {
            idf = 1e-6;
        }
        p.a_idf[i] = idf;
    }
    Ok(p)
}

/// `fts5Bm25Function`.
fn bm25_function(
    core: &mut CsrCore,
    ctx: &mut dyn VTabHostContext,
    ap_val: &[Value],
) -> Result<Value> {
    const K1: f64 = 1.2; // Constant "k1" from BM25 formula
    const B: f64 = 0.75; // Constant "b" from BM25 formula
    let mut data = bm25_get_data(core, ctx).map_err(result_error_code)?;
    // Calculate the phrase frequency (symbol "f(qi,D)" in the documentation) for each
    // phrase in the query for the current row.
    let r = (|| -> Result<f64> {
        data.a_freq.fill(0.0);
        let n_inst = core.api_inst_count(ctx)?;
        for i in 0..n_inst {
            let (ip, ic, _io) = core.api_inst(ctx, i)?;
            let w = if (ap_val.len() as i64) > i64::from(ic) {
                ap_val[ic as usize].to_real_lossy()
            } else {
                1.0
            };
            if let Some(f) = data.a_freq.get_mut(ip as usize) {
                *f += w;
            }
        }
        // Figure out the total size of the current row in tokens.
        let n_tok = core.api_column_size(ctx, -1)?;
        let d = f64::from(n_tok);
        // Determine and return the BM25 score for the current row.
        let mut score = 0.0f64;
        for i in 0..data.n_phrase {
            score += data.a_idf[i]
                * ((data.a_freq[i] * (K1 + 1.0))
                    / (data.a_freq[i] + K1 * (1.0 - B + B * d / data.avgdl)));
        }
        Ok(-1.0 * score)
    })();
    core.api_set_auxdata(data);
    match r {
        Ok(score) => Ok(real_result(score)),
        Err(e) => Err(result_error_code(e)),
    }
}

/// `fts5GetLocaleFunction`.
fn get_locale_function(
    core: &mut CsrCore,
    ctx: &mut dyn VTabHostContext,
    ap_val: &[Value],
) -> Result<Value> {
    if ap_val.len() != 1 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "wrong number of arguments to function fts5_get_locale()",
        ));
    }
    if numeric_integer_value(Some(&ap_val[0])).is_none() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "non-integer argument passed to function fts5_get_locale()",
        ));
    }
    let i_col = value_clamped_int(&ap_val[0]);
    if i_col < 0 || i_col >= core.api_column_count() {
        return Err(SqliteError::new(ErrorCode::Range, ErrorCode::Range.as_str()));
    }
    match core.api_column_locale(ctx, i_col) {
        Ok(Some(z)) => Ok(Value::Text(String::from_utf8_lossy(&z).into_owned())),
        Ok(None) => Ok(Value::Null),
        Err(e) => Err(result_error_code(e)),
    }
}

/// `pAux->xFunc(&sFts5Api, pCsr, context, argc, argv)`.
pub(super) fn call_aux(
    core: &mut CsrCore,
    ctx: &mut dyn VTabHostContext,
    f: &str,
    args: &[Value],
) -> Result<Value> {
    match f {
        "bm25" => bm25_function(core, ctx, args),
        "highlight" => highlight_function(core, ctx, args),
        "snippet" => snippet_function(core, ctx, args),
        _ => get_locale_function(core, ctx, args),
    }
}
