//! Pure safe Rust implementation of SQLite's `fts5` and `fts5vocab` full-text search
//! virtual table modules (`sqlite/ext/fts5/`).
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConstraintOp, VTabHostContext,
    VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor, VirtualTable,
    VirtualTableModule, get_varint, put_varint,
};

const FTS5_AVERAGES_ROWID: i64 = 1;
const FTS5_STRUCTURE_ROWID: i64 = 10;
const FTS5_LEAF_TARGET_PGSZ: usize = 4000;
const FTS5_MAX_EXPR_DEPTH: usize = 256;
const FTS5_AUX_MAX_ENTRIES: usize = 65_536;
const FTS5_SOURCE_ID: &str =
    "fts5: 2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76";

const SHADOW_SUFFIXES: &[&str] = &["data", "idx", "content", "docsize", "config"];

#[inline]
const fn fts5_segment_rowid(segid: u64, pgno: u64) -> i64 {
    ((segid as i64) << 37) + (pgno as i64)
}

#[inline]
const fn fts5_tombstone_rowid(segid: u64, ipg: u64) -> i64 {
    (((segid + (1 << 16)) as i64) << 37) + (ipg as i64)
}

// ============================================================================
// Quote Helpers & SQLite FTS5 Varint Codec (`fts5_varint.c`)
// ============================================================================

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn quote_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn put_uvarint(buf: &mut Vec<u8>, v: u64) {
    let mut tmp = [0u8; 9];
    let n = put_varint(&mut tmp, v);
    buf.extend_from_slice(&tmp[..n]);
}

fn get_uvarint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    if *pos >= bytes.len() {
        return None;
    }
    let (v, n) = get_varint(&bytes[*pos..]).ok()?;
    *pos += n;
    Some(v)
}

fn encode_varint_list(nums: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nums.len() * 2);
    for &n in nums {
        put_uvarint(&mut out, n);
    }
    out
}

fn decode_varint_list(bytes: &[u8]) -> Option<Vec<u64>> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    while pos < bytes.len() {
        out.push(get_uvarint(bytes, &mut pos)?);
    }
    Some(out)
}

// ============================================================================
// Auxiliary Function Registry (`bm25`, `highlight`, `snippet`)
// ============================================================================

#[derive(Debug, Clone)]
struct PhraseInstance {
    phrase_idx: usize,
    col_idx: usize,
    token_offset: usize,
}

#[derive(Debug, Clone)]
struct AuxRowEntry {
    col_texts: Vec<Option<String>>,
    col_sizes: Vec<usize>,
    instances: Vec<PhraseInstance>,
    /// Used by trigram substring highlight/snippet when byte ranges are matched directly.
    trigram_byte_spans: Vec<Vec<(usize, usize)>>,
}

#[derive(Debug)]
struct AuxQueryContext {
    num_cols: usize,
    tokenizer: TokenizerSpec,
    is_trigram_substring: bool,
    phrase_lengths: Vec<usize>,
    phrase_doc_hits: Vec<usize>,
    total_rows: usize,
    total_tokens: usize,
    rows: Vec<AuxRowEntry>,
}

#[derive(Debug, Default)]
struct AuxRegistryInner {
    next_handle: i64,
    entries: HashMap<i64, (Arc<AuxQueryContext>, usize)>,
    order: VecDeque<i64>,
}

/// Connection-scoped registry mapping FTS5 hidden table-column row handles to active match state
/// for `bm25()`, `highlight()`, and `snippet()`.
#[derive(Debug, Default)]
pub struct Fts5AuxRegistry {
    inner: Mutex<AuxRegistryInner>,
}

impl Fts5AuxRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn register_query_rows(&self, qctx: Arc<AuxQueryContext>) -> Vec<i64> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut handles = Vec::with_capacity(qctx.rows.len());
        for row_idx in 0..qctx.rows.len() {
            guard.next_handle += 1;
            let h = guard.next_handle;
            while guard.order.len() >= FTS5_AUX_MAX_ENTRIES {
                if let Some(old) = guard.order.pop_front() {
                    guard.entries.remove(&old);
                }
            }
            guard.entries.insert(h, (Arc::clone(&qctx), row_idx));
            guard.order.push_back(h);
            handles.push(h);
        }
        handles
    }

    fn lookup(&self, val: &Value) -> Result<(Arc<AuxQueryContext>, usize)> {
        let Value::Integer(handle) = val else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            ));
        };
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.entries.get(handle).cloned().ok_or_else(|| {
            SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            )
        })
    }

    pub fn eval_bm25(&self, args: &[Value]) -> Result<Value> {
        let Some(first) = args.first() else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "wrong number of arguments to function bm25()",
            ));
        };
        let (qctx, row_idx) = self.lookup(first)?;
        let weights: Vec<f64> = args[1..].iter().map(Value::to_real_lossy).collect();
        let score = compute_bm25_for_row(&qctx, row_idx, &weights);
        Ok(Value::real(score))
    }

    pub fn eval_highlight(&self, args: &[Value]) -> Result<Value> {
        if args.len() != 4 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "wrong number of arguments to function highlight()",
            ));
        }
        let (qctx, row_idx) = self.lookup(&args[0])?;
        let col_i64 = args[1].to_integer_lossy().clamp(-1, 0x7fff_ffff);
        if col_i64 < 0 || (col_i64 as usize) >= qctx.num_cols {
            return Ok(Value::Text(String::new()));
        }
        let col_idx = col_i64 as usize;
        let open_tag = value_to_aux_text(&args[2]);
        let close_tag = value_to_aux_text(&args[3]);
        let row = &qctx.rows[row_idx];
        let Some(ref text) = row.col_texts[col_idx] else {
            return Ok(Value::Null);
        };
        if qctx.is_trigram_substring {
            return Ok(Value::Text(render_byte_spans_highlight(
                text,
                &row.trigram_byte_spans[col_idx],
                &open_tag,
                &close_tag,
            )));
        }
        Ok(Value::Text(render_token_highlight(
            &qctx, row, col_idx, text, &open_tag, &close_tag, None, "",
        )))
    }

    pub fn eval_snippet(&self, args: &[Value]) -> Result<Value> {
        if args.len() != 6 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "wrong number of arguments to function snippet()",
            ));
        }
        let (qctx, row_idx) = self.lookup(&args[0])?;
        let req_col = args[1].to_integer_lossy().clamp(-1, 0x7fff_ffff);
        let open_tag = value_to_aux_text(&args[2]);
        let close_tag = value_to_aux_text(&args[3]);
        let ellipsis = value_to_aux_text(&args[4]);
        let max_tokens = args[5].to_integer_lossy().clamp(0, 64) as usize;
        let row = &qctx.rows[row_idx];
        if req_col >= 0 && (req_col as usize) >= qctx.num_cols {
            return Ok(Value::Text(String::new()));
        }
        Ok(render_snippet(
            &qctx, row, req_col, &open_tag, &close_tag, &ellipsis, max_tokens,
        ))
    }

    pub fn eval_fts5_source_id() -> Value {
        Value::Text(FTS5_SOURCE_ID.to_owned())
    }

    pub fn eval_fts5_ptr_func(_arg: &Value) -> Value {
        Value::Null
    }
}

fn value_to_aux_text(v: &Value) -> String {
    if v.is_null() {
        String::new()
    } else {
        v.to_text_lossy()
    }
}

fn compute_bm25_for_row(qctx: &AuxQueryContext, row_idx: usize, weights: &[f64]) -> f64 {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let n_phrase = qctx.phrase_lengths.len();
    if n_phrase == 0 || qctx.total_rows == 0 {
        return 0.0;
    }
    let avgdl = if qctx.total_tokens == 0 {
        1.0
    } else {
        (qctx.total_tokens as f64) / (qctx.total_rows as f64)
    };
    let row = &qctx.rows[row_idx];
    let doc_len: f64 = row.col_sizes.iter().sum::<usize>() as f64;
    let mut freq = vec![0.0f64; n_phrase];
    for inst in &row.instances {
        if inst.phrase_idx < n_phrase {
            let w = weights.get(inst.col_idx).copied().unwrap_or(1.0);
            freq[inst.phrase_idx] += w;
        }
    }
    let n_row_f = qctx.total_rows as f64;
    let mut score = 0.0f64;
    for (i, &f) in freq.iter().enumerate() {
        let n_hit = qctx.phrase_doc_hits[i] as f64;
        let mut idf = ((n_row_f - n_hit + 0.5) / (n_hit + 0.5)).ln();
        if idf <= 0.0 {
            idf = 1.0e-6;
        }
        let denom = f + K1 * (1.0 - B + B * doc_len / avgdl);
        if denom > 0.0 {
            score += idf * ((f * (K1 + 1.0)) / denom);
        }
    }
    -score
}

fn coalesced_col_ranges(
    qctx: &AuxQueryContext,
    row: &AuxRowEntry,
    col_idx: usize,
) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for inst in &row.instances {
        if inst.col_idx != col_idx {
            continue;
        }
        let len = qctx
            .phrase_lengths
            .get(inst.phrase_idx)
            .copied()
            .unwrap_or(1);
        if len == 0 {
            continue;
        }
        let s = inst.token_offset;
        let e = s + len - 1;
        spans.push((s, e));
    }
    spans.sort_by_key(|&(s, e)| (s, e));
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in spans {
        if let Some(last) = merged.last_mut()
            && s <= last.1 + 1
        {
            if e > last.1 {
                last.1 = e;
            }
            continue;
        }
        merged.push((s, e));
    }
    merged
}

#[allow(clippy::too_many_arguments)]
fn render_token_highlight(
    qctx: &AuxQueryContext,
    row: &AuxRowEntry,
    col_idx: usize,
    text: &str,
    open_tag: &str,
    close_tag: &str,
    snippet_window: Option<(usize, usize, usize)>,
    ellipsis: &str,
) -> String {
    let tokens = qctx.tokenizer.tokenize(text, false);
    let ranges = coalesced_col_ranges(qctx, row, col_idx);
    let mut range_idx = 0usize;
    let mut out = String::new();
    let mut i_off = 0usize;
    let mut b_open = false;

    let (win_start, win_end, col_size) = match snippet_window {
        Some((ws, we, cs)) => {
            if ws > 0 {
                out.push_str(ellipsis);
            }
            while range_idx < ranges.len() && ranges[range_idx].0 < ws {
                range_idx += 1;
            }
            (Some(ws), Some(we), cs)
        }
        None => (None, None, tokens.len()),
    };

    for (i_pos, tok) in tokens.iter().enumerate() {
        if let (Some(ws), Some(we)) = (win_start, win_end) {
            if i_pos < ws || i_pos > we {
                continue;
            }
            if ws > 0 && i_pos == ws {
                i_off = tok.start_byte;
            }
        }

        let cur_start = ranges.get(range_idx).map(|r| r.0);
        let cur_end = ranges.get(range_idx).map(|r| r.1);

        if b_open && cur_start.is_none_or(|cs| i_pos <= cs) && tok.start_byte > i_off {
            out.push_str(close_tag);
            b_open = false;
        }

        if cur_start == Some(i_pos) && !b_open {
            out.push_str(&text[i_off..tok.start_byte]);
            out.push_str(open_tag);
            i_off = tok.start_byte;
            b_open = true;
        }

        if cur_end == Some(i_pos) {
            if !b_open {
                out.push_str(open_tag);
                b_open = true;
            }
            out.push_str(&text[i_off..tok.end_byte]);
            i_off = tok.end_byte;
            range_idx += 1;
        }

        if win_end == Some(i_pos) {
            if b_open {
                if cur_start.is_some_and(|cs| i_pos >= cs) {
                    out.push_str(&text[i_off..tok.end_byte]);
                    i_off = tok.end_byte;
                }
                out.push_str(close_tag);
                b_open = false;
            }
            out.push_str(&text[i_off..tok.end_byte]);
            i_off = tok.end_byte;
        }
    }

    if b_open {
        out.push_str(close_tag);
    }

    if let Some(we) = win_end {
        if col_size == 0 || we >= col_size.saturating_sub(1) {
            out.push_str(&text[i_off..]);
        } else {
            out.push_str(ellipsis);
        }
    } else {
        out.push_str(&text[i_off..]);
    }

    out
}

fn render_byte_spans_highlight(
    text: &str,
    spans: &[(usize, usize)],
    open_tag: &str,
    close_tag: &str,
) -> String {
    if spans.is_empty() {
        return text.to_owned();
    }
    let mut merged: Vec<(usize, usize)> = Vec::new();
    let mut sorted = spans.to_vec();
    sorted.sort_unstable();
    for (s, e) in sorted {
        if s >= e || e > text.len() {
            continue;
        }
        if let Some(last) = merged.last_mut()
            && s <= last.1
        {
            if e > last.1 {
                last.1 = e;
            }
            continue;
        }
        merged.push((s, e));
    }
    let mut out =
        String::with_capacity(text.len() + merged.len() * (open_tag.len() + close_tag.len()));
    let mut cur = 0usize;
    for (s, e) in merged {
        out.push_str(&text[cur..s]);
        out.push_str(open_tag);
        out.push_str(&text[s..e]);
        out.push_str(close_tag);
        cur = e;
    }
    out.push_str(&text[cur..]);
    out
}

fn render_snippet(
    qctx: &AuxQueryContext,
    row: &AuxRowEntry,
    req_col: i64,
    open_tag: &str,
    close_tag: &str,
    ellipsis: &str,
    max_tokens: usize,
) -> Value {
    let mut best_col = if req_col >= 0 { req_col as usize } else { 0 };
    let mut best_start = 0usize;
    let mut best_score = 0i64;

    if qctx.is_trigram_substring {
        for c in 0..qctx.num_cols {
            if req_col >= 0 && (req_col as usize) != c {
                continue;
            }
            if !row.trigram_byte_spans[c].is_empty() {
                best_col = c;
                break;
            }
        }
        let Some(ref text) = row.col_texts[best_col] else {
            return Value::Null;
        };
        return Value::Text(render_byte_spans_highlight(
            text,
            &row.trigram_byte_spans[best_col],
            open_tag,
            close_tag,
        ));
    }

    let n_phrase = qctx.phrase_lengths.len();
    for c in 0..qctx.num_cols {
        if req_col >= 0 && (req_col as usize) != c {
            continue;
        }
        let Some(ref col_text) = row.col_texts[c] else {
            continue;
        };
        let docsize = row.col_sizes[c];

        // Find sentence-start token positions matching `fts5SentenceFinderCb` (`fts5_aux.c`)
        let col_tokens = qctx.tokenizer.tokenize(col_text, false);
        let text_bytes = col_text.as_bytes();
        let mut s_starts: Vec<usize> = Vec::new();
        for (i_pos, tok) in col_tokens.iter().enumerate() {
            if i_pos == 0 {
                s_starts.push(0);
            } else {
                let mut bi = tok.start_byte as isize - 1;
                let mut prev_c = 0u8;
                while bi >= 0 {
                    prev_c = text_bytes[bi as usize];
                    if !matches!(prev_c, b' ' | b'\t' | b'\n' | b'\r') {
                        break;
                    }
                    bi -= 1;
                }
                if bi != tok.start_byte as isize - 1 && matches!(prev_c, b'.' | b':') {
                    s_starts.push(i_pos);
                }
            }
        }

        let score_window = |start_pos: usize| -> (i64, Option<usize>, usize) {
            let end = start_pos + max_tokens;
            let mut seen = vec![false; n_phrase];
            let mut sc = 0i64;
            let mut fo: Option<usize> = None;
            let mut lo = 0usize;
            for cand in &row.instances {
                if cand.col_idx == c && cand.token_offset >= start_pos && cand.token_offset < end {
                    sc += if seen[cand.phrase_idx] { 1 } else { 1000 };
                    seen[cand.phrase_idx] = true;
                    if fo.is_none() {
                        fo = Some(cand.token_offset);
                    }
                    lo = cand.token_offset + qctx.phrase_lengths[cand.phrase_idx];
                }
            }
            (sc, fo, lo)
        };

        for inst in &row.instances {
            if inst.col_idx != c {
                continue;
            }
            let pos = inst.token_offset;
            let (score, first_off, last_off) = score_window(pos);
            if score > best_score {
                best_score = score;
                best_col = c;
                if let Some(fo) = first_off {
                    let span = last_off.saturating_sub(fo);
                    let mut adj = (fo as i64) - ((max_tokens as i64 - span as i64) / 2);
                    if adj + (max_tokens as i64) > (docsize as i64) {
                        adj = (docsize as i64) - (max_tokens as i64);
                    }
                    if adj < 0 {
                        adj = 0;
                    }
                    best_start = adj as usize;
                }
            }

            if !s_starts.is_empty() && docsize > max_tokens {
                let mut jj = 0usize;
                while jj + 1 < s_starts.len() {
                    if s_starts[jj + 1] > pos {
                        break;
                    }
                    jj += 1;
                }
                if s_starts[jj] < pos {
                    let s_pos = s_starts[jj];
                    let (mut s_score, _, _) = score_window(s_pos);
                    s_score += if s_pos == 0 { 120 } else { 100 };
                    if s_score > best_score {
                        best_score = s_score;
                        best_col = c;
                        best_start = s_pos;
                    }
                }
            }
        }
    }

    let Some(ref text) = row.col_texts[best_col] else {
        return Value::Null;
    };
    if max_tokens == 0 {
        return Value::Text(String::new());
    }
    let col_size = row.col_sizes[best_col];
    let win_end = best_start + max_tokens - 1;
    Value::Text(render_token_highlight(
        qctx,
        row,
        best_col,
        text,
        open_tag,
        close_tag,
        Some((best_start, win_end, col_size)),
        ellipsis,
    ))
}

// ============================================================================
// Tokenizers (`unicode61`, `ascii`, `porter`, `trigram`)
// ============================================================================

#[derive(Debug, Clone)]
struct EmittedToken {
    term: String,
    start_byte: usize,
    end_byte: usize,
}

#[derive(Debug, Clone)]
enum TokenizerSpec {
    Unicode61 {
        remove_diacritics: u8,
        tokenchars: BTreeSet<char>,
        separators: BTreeSet<char>,
    },
    Ascii {
        tokenchars: BTreeSet<char>,
        separators: BTreeSet<char>,
    },
    Porter(Box<TokenizerSpec>),
    Trigram {
        case_sensitive: bool,
        remove_diacritics: u8,
    },
}

impl Default for TokenizerSpec {
    fn default() -> Self {
        Self::Unicode61 {
            remove_diacritics: 2,
            tokenchars: BTreeSet::new(),
            separators: BTreeSet::new(),
        }
    }
}

impl TokenizerSpec {
    fn parse(spec: &str) -> Result<Self> {
        let args = split_fts5_tokenize_args(spec)?;
        Self::parse_tokens(&args)
    }

    fn parse_tokens(args: &[String]) -> Result<Self> {
        let Some(head) = args.first() else {
            return Ok(Self::default());
        };
        match head.to_ascii_lowercase().as_str() {
            "unicode61" => {
                let mut remove_diacritics = 2u8;
                let mut tokenchars = BTreeSet::new();
                let mut separators = BTreeSet::new();
                let mut i = 1usize;
                while i < args.len() {
                    let opt = args[i].to_ascii_lowercase();
                    let Some(val) = args.get(i + 1) else {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "fts5: error creating tokenizer",
                        ));
                    };
                    match opt.as_str() {
                        "remove_diacritics" => {
                            remove_diacritics = match val.as_str() {
                                "0" => 0,
                                "1" => 1,
                                "2" => 2,
                                _ => {
                                    return Err(SqliteError::new(
                                        ErrorCode::Error,
                                        "fts5: error creating tokenizer",
                                    ));
                                }
                            };
                        }
                        "tokenchars" => {
                            for ch in val.chars() {
                                separators.remove(&ch);
                                tokenchars.insert(ch);
                            }
                        }
                        "separators" => {
                            for ch in val.chars() {
                                tokenchars.remove(&ch);
                                separators.insert(ch);
                            }
                        }
                        "categories" => {}
                        _ => {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("fts5: unrecognized option: \"{opt}\""),
                            ));
                        }
                    }
                    i += 2;
                }
                Ok(Self::Unicode61 {
                    remove_diacritics,
                    tokenchars,
                    separators,
                })
            }
            "ascii" => {
                let mut tokenchars = BTreeSet::new();
                let mut separators = BTreeSet::new();
                let mut i = 1usize;
                while i < args.len() {
                    let opt = args[i].to_ascii_lowercase();
                    let Some(val) = args.get(i + 1) else {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "fts5: error creating tokenizer",
                        ));
                    };
                    match opt.as_str() {
                        "tokenchars" => {
                            for ch in val.chars() {
                                separators.remove(&ch);
                                tokenchars.insert(ch);
                            }
                        }
                        "separators" => {
                            for ch in val.chars() {
                                tokenchars.remove(&ch);
                                separators.insert(ch);
                            }
                        }
                        _ => {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("fts5: unrecognized option: \"{opt}\""),
                            ));
                        }
                    }
                    i += 2;
                }
                Ok(Self::Ascii {
                    tokenchars,
                    separators,
                })
            }
            "porter" => {
                let inner = if args.len() > 1 {
                    Self::parse_tokens(&args[1..])?
                } else {
                    Self::default()
                };
                Ok(Self::Porter(Box::new(inner)))
            }
            "trigram" => {
                let mut case_sensitive = false;
                let mut remove_diacritics = 0u8;
                let mut i = 1usize;
                while i < args.len() {
                    let opt = args[i].to_ascii_lowercase();
                    let Some(val) = args.get(i + 1) else {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "fts5: error creating tokenizer",
                        ));
                    };
                    match opt.as_str() {
                        "case_sensitive" => {
                            case_sensitive = match val.as_str() {
                                "0" => false,
                                "1" => true,
                                _ => {
                                    return Err(SqliteError::new(
                                        ErrorCode::Error,
                                        "fts5: error creating tokenizer",
                                    ));
                                }
                            };
                        }
                        "remove_diacritics" => {
                            remove_diacritics = match val.as_str() {
                                "0" => 0,
                                "1" => 1,
                                "2" => 2,
                                _ => {
                                    return Err(SqliteError::new(
                                        ErrorCode::Error,
                                        "fts5: error creating tokenizer",
                                    ));
                                }
                            };
                        }
                        _ => {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("fts5: unrecognized option: \"{opt}\""),
                            ));
                        }
                    }
                    i += 2;
                }
                if case_sensitive && remove_diacritics != 0 {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "fts5: error creating tokenizer",
                    ));
                }
                Ok(Self::Trigram {
                    case_sensitive,
                    remove_diacritics,
                })
            }
            other => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such tokenizer: {other}"),
            )),
        }
    }

    fn is_trigram(&self) -> bool {
        matches!(self, Self::Trigram { .. })
    }

    fn trigram_params(&self) -> Option<(bool, u8)> {
        match self {
            Self::Trigram {
                case_sensitive,
                remove_diacritics,
            } => Some((*case_sensitive, *remove_diacritics)),
            _ => None,
        }
    }

    fn tokenize(&self, text: &str, _for_query: bool) -> Vec<EmittedToken> {
        match self {
            Self::Unicode61 {
                remove_diacritics,
                tokenchars,
                separators,
            } => tokenize_unicode61(text, *remove_diacritics, tokenchars, separators),
            Self::Ascii {
                tokenchars,
                separators,
            } => tokenize_ascii(text, tokenchars, separators),
            Self::Porter(inner) => {
                let mut toks = inner.tokenize(text, _for_query);
                for t in &mut toks {
                    if t.term.len() >= 3
                        && t.term.len() < 64
                        && t.term.bytes().all(|b| b.is_ascii_lowercase())
                    {
                        t.term = porter_stem(&t.term);
                    }
                }
                toks
            }
            Self::Trigram {
                case_sensitive,
                remove_diacritics,
            } => tokenize_trigram(text, *case_sensitive, *remove_diacritics),
        }
    }
}

fn split_fts5_tokenize_args(spec: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let bytes = spec.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let q = bytes[i];
        if q == b'\'' || q == b'"' {
            i += 1;
            let mut s = String::new();
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == q {
                    if i + 1 < bytes.len() && bytes[i + 1] == q {
                        s.push(q as char);
                        i += 2;
                    } else {
                        i += 1;
                        closed = true;
                        break;
                    }
                } else {
                    let ch = spec[i..].chars().next().unwrap_or('\0');
                    s.push(ch);
                    i += ch.len_utf8();
                }
            }
            if !closed {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "fts5: parse error in tokenize directive",
                ));
            }
            out.push(s);
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(spec[start..i].to_owned());
        }
    }
    Ok(out)
}

fn is_combining_mark(ch: char) -> bool {
    let u = ch as u32;
    matches!(
        u,
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0xFE20..=0xFE2F
    )
}

fn strip_latin_diacritic_folded(ch: char) -> Option<&'static str> {
    match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ȁ' | 'ȃ' | 'ȧ' | 'ạ' | 'ả'
        | 'ấ' | 'ầ' | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => {
            Some("a")
        }
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => Some("c"),
        'ď' | 'đ' => Some("d"),
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ȅ' | 'ȇ' | 'ȩ' | 'ẹ' | 'ẻ' | 'ẽ'
        | 'ế' | 'ề' | 'ể' | 'ễ' | 'ệ' => Some("e"),
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => Some("g"),
        'ĥ' | 'ħ' => Some("h"),
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' | 'ȉ' | 'ȋ' | 'ỉ' | 'ị' => {
            Some("i")
        }
        'ĵ' => Some("j"),
        'ķ' => Some("k"),
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => Some("l"),
        'ñ' | 'ń' | 'ņ' | 'ň' => Some("n"),
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ơ' | 'ǒ' | 'ǫ' | 'ǭ' | 'ȍ' | 'ȏ'
        | 'ọ' | 'ỏ' | 'ố' | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' => {
            Some("o")
        }
        'ŕ' | 'ŗ' | 'ř' => Some("r"),
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => Some("s"),
        'ţ' | 'ť' | 'ŧ' | 'ț' => Some("t"),
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ư' | 'ǔ' | 'ȕ' | 'ȗ' | 'ụ'
        | 'ủ' | 'ứ' | 'ừ' | 'ử' | 'ữ' | 'ự' => Some("u"),
        'ŵ' => Some("w"),
        'ý' | 'ÿ' | 'ŷ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => Some("y"),
        'ź' | 'ż' | 'ž' => Some("z"),
        _ => None,
    }
}

fn fold_unicode61_char(ch: char, remove_diacritics: u8, out: &mut String) {
    if remove_diacritics > 0 && is_combining_mark(ch) {
        return;
    }
    if ch == 'İ' {
        out.push('i');
        return;
    }
    for lc in ch.to_lowercase() {
        if remove_diacritics > 0 {
            if is_combining_mark(lc) {
                continue;
            }
            if let Some(base) = strip_latin_diacritic_folded(lc) {
                out.push_str(base);
                continue;
            }
        }
        out.push(lc);
    }
}

fn is_unicode61_token_char(
    ch: char,
    tokenchars: &BTreeSet<char>,
    separators: &BTreeSet<char>,
) -> bool {
    if separators.contains(&ch) {
        return false;
    }
    if tokenchars.contains(&ch) {
        return true;
    }
    ch.is_alphanumeric() || is_combining_mark(ch)
}

fn tokenize_unicode61(
    text: &str,
    remove_diacritics: u8,
    tokenchars: &BTreeSet<char>,
    separators: &BTreeSet<char>,
) -> Vec<EmittedToken> {
    let mut out = Vec::new();
    let mut cur_term = String::new();
    let mut cur_start: Option<usize> = None;
    let mut last_end = 0usize;

    for (idx, ch) in text.char_indices() {
        if is_unicode61_token_char(ch, tokenchars, separators) {
            if cur_start.is_none() {
                cur_start = Some(idx);
            }
            fold_unicode61_char(ch, remove_diacritics, &mut cur_term);
            last_end = idx + ch.len_utf8();
        } else if let Some(s) = cur_start.take() {
            if !cur_term.is_empty() {
                out.push(EmittedToken {
                    term: std::mem::take(&mut cur_term),
                    start_byte: s,
                    end_byte: last_end,
                });
            } else {
                cur_term.clear();
            }
        }
    }
    if let Some(s) = cur_start.take()
        && !cur_term.is_empty()
    {
        out.push(EmittedToken {
            term: cur_term,
            start_byte: s,
            end_byte: last_end,
        });
    }
    out
}

fn tokenize_ascii(
    text: &str,
    tokenchars: &BTreeSet<char>,
    separators: &BTreeSet<char>,
) -> Vec<EmittedToken> {
    let mut out = Vec::new();
    let mut cur_term = String::new();
    let mut cur_start: Option<usize> = None;
    let mut last_end = 0usize;

    for (idx, ch) in text.char_indices() {
        let is_tok = if separators.contains(&ch) {
            false
        } else if tokenchars.contains(&ch) {
            true
        } else {
            ch.is_ascii_alphanumeric() || (ch as u32) >= 0x80
        };
        if is_tok {
            if cur_start.is_none() {
                cur_start = Some(idx);
            }
            if ch.is_ascii_uppercase() {
                cur_term.push(ch.to_ascii_lowercase());
            } else {
                cur_term.push(ch);
            }
            last_end = idx + ch.len_utf8();
        } else if let Some(s) = cur_start.take()
            && !cur_term.is_empty()
        {
            out.push(EmittedToken {
                term: std::mem::take(&mut cur_term),
                start_byte: s,
                end_byte: last_end,
            });
        }
    }
    if let Some(s) = cur_start.take()
        && !cur_term.is_empty()
    {
        out.push(EmittedToken {
            term: cur_term,
            start_byte: s,
            end_byte: last_end,
        });
    }
    out
}

fn fold_trigram_text_with_map(
    text: &str,
    case_sensitive: bool,
    remove_diacritics: u8,
) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut chars = Vec::new();
    let mut spans = Vec::new();
    for (idx, ch) in text.char_indices() {
        let end = idx + ch.len_utf8();
        if !case_sensitive {
            let mut folded = String::new();
            fold_unicode61_char(ch, remove_diacritics, &mut folded);
            for fc in folded.chars() {
                chars.push(fc);
                spans.push((idx, end));
            }
        } else {
            chars.push(ch);
            spans.push((idx, end));
        }
    }
    (chars, spans)
}

fn tokenize_trigram(text: &str, case_sensitive: bool, remove_diacritics: u8) -> Vec<EmittedToken> {
    let (chars, spans) = fold_trigram_text_with_map(text, case_sensitive, remove_diacritics);
    if chars.len() < 3 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(chars.len() - 2);
    for i in 0..=(chars.len() - 3) {
        let mut s = String::with_capacity(6);
        s.push(chars[i]);
        s.push(chars[i + 1]);
        s.push(chars[i + 2]);
        out.push(EmittedToken {
            term: s,
            start_byte: spans[i].0,
            end_byte: spans[i + 2].1,
        });
    }
    out
}

// ============================================================================
// Martin Porter English Stemmer (`fts5_tokenize.c`)
// ============================================================================

fn is_porter_consonant(b: &[u8], i: usize) -> bool {
    match b[i] {
        b'a' | b'e' | b'i' | b'o' | b'u' => false,
        b'y' => {
            if i == 0 {
                true
            } else {
                !is_porter_consonant(b, i - 1)
            }
        }
        _ => true,
    }
}

fn porter_measure(b: &[u8]) -> usize {
    let n = b.len();
    let mut i = 0usize;
    while i < n && is_porter_consonant(b, i) {
        i += 1;
    }
    let mut m = 0usize;
    while i < n {
        while i < n && !is_porter_consonant(b, i) {
            i += 1;
        }
        if i >= n {
            break;
        }
        m += 1;
        while i < n && is_porter_consonant(b, i) {
            i += 1;
        }
    }
    m
}

fn porter_has_vowel(b: &[u8]) -> bool {
    (0..b.len()).any(|i| !is_porter_consonant(b, i))
}

fn porter_double_consonant(b: &[u8]) -> bool {
    let n = b.len();
    n >= 2 && b[n - 1] == b[n - 2] && is_porter_consonant(b, n - 1)
}

fn porter_cvc(b: &[u8]) -> bool {
    let n = b.len();
    if n < 3 {
        return false;
    }
    if !is_porter_consonant(b, n - 1)
        || is_porter_consonant(b, n - 2)
        || !is_porter_consonant(b, n - 3)
    {
        return false;
    }
    !matches!(b[n - 1], b'w' | b'x' | b'y')
}

fn porter_replace_if_m_gt(w: &mut Vec<u8>, suffix: &[u8], repl: &[u8], min_m: usize) -> bool {
    if !w.ends_with(suffix) {
        return false;
    }
    let stem_len = w.len() - suffix.len();
    if porter_measure(&w[..stem_len]) > min_m {
        w.truncate(stem_len);
        w.extend_from_slice(repl);
    }
    true
}

fn porter_stem(word: &str) -> String {
    let mut w = word.as_bytes().to_vec();
    if w.len() <= 2 {
        return word.to_owned();
    }

    // Step 1a
    if w.ends_with(b"sses") || w.ends_with(b"ies") {
        w.truncate(w.len() - 2);
    } else if !w.ends_with(b"ss") && w.ends_with(b"s") {
        w.pop();
    }

    // Step 1b
    let mut step1b_extra = false;
    if w.ends_with(b"eed") {
        let stem_len = w.len() - 3;
        if porter_measure(&w[..stem_len]) > 0 {
            w.pop();
        }
    } else if w.ends_with(b"ed") {
        let stem_len = w.len() - 2;
        if porter_has_vowel(&w[..stem_len]) {
            w.truncate(stem_len);
            step1b_extra = true;
        }
    } else if w.ends_with(b"ing") {
        let stem_len = w.len() - 3;
        if porter_has_vowel(&w[..stem_len]) {
            w.truncate(stem_len);
            step1b_extra = true;
        }
    }

    if step1b_extra {
        if w.ends_with(b"at") || w.ends_with(b"bl") || w.ends_with(b"iz") {
            w.push(b'e');
        } else if porter_double_consonant(&w)
            && !matches!(w.last().copied(), Some(b'l' | b's' | b'z'))
        {
            w.pop();
        } else if porter_measure(&w) == 1 && porter_cvc(&w) {
            w.push(b'e');
        }
    }

    // Step 1c
    if w.ends_with(b"y") && porter_has_vowel(&w[..w.len() - 1]) {
        let last = w.len() - 1;
        w[last] = b'i';
    }

    // Step 2
    let step2: &[(&[u8], &[u8])] = &[
        (b"ational", b"ate"),
        (b"tional", b"tion"),
        (b"enci", b"ence"),
        (b"anci", b"ance"),
        (b"izer", b"ize"),
        (b"bli", b"ble"),
        (b"alli", b"al"),
        (b"entli", b"ent"),
        (b"eli", b"e"),
        (b"ousli", b"ous"),
        (b"ization", b"ize"),
        (b"ation", b"ate"),
        (b"ator", b"ate"),
        (b"alism", b"al"),
        (b"iveness", b"ive"),
        (b"fulness", b"ful"),
        (b"ousness", b"ous"),
        (b"aliti", b"al"),
        (b"iviti", b"ive"),
        (b"biliti", b"ble"),
        (b"logi", b"log"),
    ];
    for &(suf, rep) in step2 {
        if porter_replace_if_m_gt(&mut w, suf, rep, 0) {
            break;
        }
    }

    // Step 3
    let step3: &[(&[u8], &[u8])] = &[
        (b"icate", b"ic"),
        (b"ative", b""),
        (b"alize", b"al"),
        (b"iciti", b"ic"),
        (b"ical", b"ic"),
        (b"ful", b""),
        (b"ness", b""),
    ];
    for &(suf, rep) in step3 {
        if porter_replace_if_m_gt(&mut w, suf, rep, 0) {
            break;
        }
    }

    // Step 4
    let step4: &[&[u8]] = &[
        b"al", b"ance", b"ence", b"er", b"ic", b"able", b"ible", b"ant", b"ement", b"ment", b"ent",
        b"ion", b"ou", b"ism", b"ate", b"iti", b"ous", b"ive", b"ize",
    ];
    for &suf in step4 {
        if w.ends_with(suf) {
            let stem_len = w.len() - suf.len();
            if suf == b"ion" {
                if stem_len > 0
                    && matches!(w[stem_len - 1], b's' | b't')
                    && porter_measure(&w[..stem_len]) > 1
                {
                    w.truncate(stem_len);
                }
            } else if porter_measure(&w[..stem_len]) > 1 {
                w.truncate(stem_len);
            }
            break;
        }
    }

    // Step 5a
    if w.ends_with(b"e") {
        let stem_len = w.len() - 1;
        let m = porter_measure(&w[..stem_len]);
        if m > 1 || (m == 1 && !porter_cvc(&w[..stem_len])) {
            w.pop();
        }
    }

    // Step 5b
    if w.ends_with(b"ll") && porter_measure(&w) > 1 {
        w.pop();
    }

    String::from_utf8(w).unwrap_or_else(|_| word.to_owned())
}

// ============================================================================
// FTS5 Table Schema & Options
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailMode {
    Full,
    Col,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContentMode {
    Normal,
    Contentless,
    External {
        table: String,
        rowid_col: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct Fts5Config {
    columns: Vec<String>,
    unindexed: Vec<bool>,
    tokenizer: TokenizerSpec,
    prefixes: Vec<usize>,
    content_mode: ContentMode,
    columnsize: bool,
    detail: DetailMode,
    contentless_delete: bool,
}

impl Fts5Config {
    fn parse(table_name: &str, raw_args: &[String]) -> Result<Self> {
        let mut columns = Vec::new();
        let mut unindexed = Vec::new();
        let mut tokenize_str: Option<String> = None;
        let mut prefixes = Vec::new();
        let mut content_opt: Option<String> = None;
        let mut content_rowid_opt: Option<String> = None;
        let mut columnsize = true;
        let mut detail = DetailMode::Full;
        let mut contentless_delete = false;

        for raw in raw_args {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "fts5: parse error in CREATE VIRTUAL TABLE",
                ));
            }
            if let Some((k, v)) = split_option_assignment(trimmed)? {
                let key_lc = k.to_ascii_lowercase();
                match key_lc.as_str() {
                    "tokenize" => tokenize_str = Some(v),
                    "prefix" => {
                        for part in v.split(|c: char| c.is_ascii_whitespace() || c == ',') {
                            if part.is_empty() {
                                continue;
                            }
                            let n = part.parse::<usize>().map_err(|_| {
                                SqliteError::new(ErrorCode::Error, "malformed prefix=... directive")
                            })?;
                            if n == 0 || n > 1000 {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    "malformed prefix=... directive",
                                ));
                            }
                            if !prefixes.contains(&n) {
                                prefixes.push(n);
                            }
                        }
                    }
                    "content" => content_opt = Some(v),
                    "content_rowid" => content_rowid_opt = Some(v),
                    "columnsize" => {
                        columnsize = match v.as_str() {
                            "0" => false,
                            "1" => true,
                            _ => {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    "malformed columnsize=... directive",
                                ));
                            }
                        };
                    }
                    "detail" => {
                        detail = match v.to_ascii_lowercase().as_str() {
                            "full" => DetailMode::Full,
                            "col" => DetailMode::Col,
                            "none" => DetailMode::None,
                            _ => {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    "malformed detail=... directive",
                                ));
                            }
                        };
                    }
                    "tokendata" => {
                        if !matches!(v.as_str(), "0" | "1") {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                "malformed tokendata=... directive",
                            ));
                        }
                    }
                    "contentless_delete" => {
                        contentless_delete = match v.as_str() {
                            "0" => false,
                            "1" => true,
                            _ => {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    "malformed contentless_delete=... directive",
                                ));
                            }
                        };
                    }
                    "contentless_unindexed" => {
                        if !matches!(v.as_str(), "0" | "1") {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                "malformed contentless_unindexed=... directive",
                            ));
                        }
                    }
                    other => {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("unrecognized option: \"{other}\""),
                        ));
                    }
                }
            } else {
                let (col_name, is_unindexed) = parse_column_arg(trimmed)?;
                if col_name.eq_ignore_ascii_case("rowid")
                    || col_name.eq_ignore_ascii_case("rank")
                    || col_name.eq_ignore_ascii_case(table_name)
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("fts5: column name may not be {col_name}"),
                    ));
                }
                if columns
                    .iter()
                    .any(|c: &String| c.eq_ignore_ascii_case(&col_name))
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("duplicate column name: {col_name}"),
                    ));
                }
                columns.push(col_name);
                unindexed.push(is_unindexed);
            }
        }

        if columns.is_empty() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "fts5: need at least one column",
            ));
        }

        if contentless_delete && !columnsize {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "contentless_delete=1 is incompatible with columnsize=0",
            ));
        }

        let tokenizer = match tokenize_str {
            Some(ref s) => TokenizerSpec::parse(s)?,
            None => TokenizerSpec::default(),
        };

        let content_mode = match content_opt {
            None => ContentMode::Normal,
            Some(s) if s.is_empty() => ContentMode::Contentless,
            Some(tbl) => ContentMode::External {
                table: tbl,
                rowid_col: content_rowid_opt,
            },
        };

        Ok(Self {
            columns,
            unindexed,
            tokenizer,
            prefixes,
            content_mode,
            columnsize,
            detail,
            contentless_delete,
        })
    }

    fn declare_sql(&self, table_name: &str) -> String {
        let mut parts = Vec::with_capacity(self.columns.len() + 2);
        for c in &self.columns {
            parts.push(quote_ident(c));
        }
        parts.push(format!("{} HIDDEN", quote_ident(table_name)));
        parts.push("rank HIDDEN".to_owned());
        format!("CREATE TABLE x({})", parts.join(", "))
    }
}

fn split_option_assignment(s: &str) -> Result<Option<(String, String)>> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return Ok(None);
    }
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    let key = &s[..i];
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'=' {
        return Ok(None);
    }
    i += 1;
    let val_raw = s[i..].trim();
    if val_raw.is_empty() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "fts5: parse error in option",
        ));
    }
    let val = unquote_sql_token(val_raw)?;
    Ok(Some((key.to_owned(), val)))
}

fn unquote_sql_token(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let q = bytes[0];
        if (q == b'\'' || q == b'"' || q == b'`') && bytes[bytes.len() - 1] == q {
            let inner = &s[1..s.len() - 1];
            let qch = q as char;
            let dbl = format!("{qch}{qch}");
            return Ok(inner.replace(&dbl, &qch.to_string()));
        }
        if q == b'[' && bytes[bytes.len() - 1] == b']' {
            return Ok(s[1..s.len() - 1].to_owned());
        }
    }
    Ok(s.to_owned())
}

fn parse_column_arg(s: &str) -> Result<(String, bool)> {
    let bytes = s.as_bytes();
    let (name, rest) = if matches!(bytes[0], b'"' | b'\'' | b'`' | b'[') {
        let close = if bytes[0] == b'[' { b']' } else { bytes[0] };
        let mut i = 1usize;
        let mut out = String::new();
        let mut closed = false;
        while i < bytes.len() {
            if bytes[i] == close {
                if close != b']' && i + 1 < bytes.len() && bytes[i + 1] == close {
                    out.push(close as char);
                    i += 2;
                } else {
                    i += 1;
                    closed = true;
                    break;
                }
            } else {
                let ch = s[i..].chars().next().unwrap_or('\0');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        if !closed {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "fts5: unmatched quote in column definition",
            ));
        }
        (out, s[i..].trim())
    } else {
        let mut i = 0usize;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        (s[..i].to_owned(), s[i..].trim())
    };

    if rest.is_empty() {
        return Ok((name, false));
    }
    let mut is_unindexed = false;
    for tok in rest.split_ascii_whitespace() {
        if tok.eq_ignore_ascii_case("UNINDEXED") {
            is_unindexed = true;
        } else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("fts5: syntax error near \"{tok}\""),
            ));
        }
    }
    Ok((name, is_unindexed))
}

// ============================================================================
// C SQLite 3.54.1 FTS5 Segment B-Tree Wire Codec (`sqlite/ext/fts5/fts5_index.c`)
// ============================================================================

/// Posting entry for one `(rowid, col_idx, token_offsets)` or tombstone (`deleted = true`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocPosting {
    rowid: i64,
    deleted: bool,
    /// Map of `col_idx -> sorted token offsets` (`DetailMode::None` uses empty vec on col `0`).
    col_hits: BTreeMap<usize, Vec<usize>>,
}

type MergedIndexMap = BTreeMap<Vec<u8>, BTreeMap<i64, BTreeMap<usize, Vec<usize>>>>;

fn encode_empty_structure_record(v2: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(11);
    out.extend_from_slice(&[0, 0, 0, 0]);
    if v2 {
        out.extend_from_slice(b"\xff\x00\x00\x01");
    }
    out.extend_from_slice(&[0, 0, 0]);
    out
}

fn encode_single_seg_structure_record(v2: bool, segid: u64, pgno_last: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&[0, 0, 0, 0]);
    if v2 {
        out.extend_from_slice(b"\xff\x00\x00\x01");
    }
    put_uvarint(&mut out, 1); // nLevel = 1
    put_uvarint(&mut out, 1); // nSegment = 1
    put_uvarint(&mut out, pgno_last); // nWriteCounter
    put_uvarint(&mut out, 0); // nMerge = 0
    put_uvarint(&mut out, 1); // nSeg in level 0 = 1
    put_uvarint(&mut out, segid);
    put_uvarint(&mut out, 1); // pgnoFirst = 1
    put_uvarint(&mut out, pgno_last);
    if v2 {
        put_uvarint(&mut out, 1); // iOrigin1
        put_uvarint(&mut out, 1); // iOrigin2
        put_uvarint(&mut out, 0); // nPgTombstone = 0
    }
    out
}

fn common_prefix_len(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

fn encode_poslist(detail: DetailMode, col_hits: &BTreeMap<usize, Vec<usize>>) -> Vec<u8> {
    match detail {
        DetailMode::None => Vec::new(),
        DetailMode::Col => {
            let mut body = Vec::new();
            let mut prev_col = 0usize;
            for (idx, &c) in col_hits.keys().enumerate() {
                if idx == 0 {
                    put_uvarint(&mut body, (c + 2) as u64);
                } else {
                    put_uvarint(&mut body, (c - prev_col + 2) as u64);
                }
                prev_col = c;
            }
            let mut out = Vec::with_capacity(body.len() + 2);
            put_uvarint(&mut out, (body.len() as u64) << 1);
            out.extend_from_slice(&body);
            out
        }
        DetailMode::Full => {
            let mut body = Vec::new();
            for (&c, offs) in col_hits {
                if c > 0 {
                    body.push(0x01);
                    put_uvarint(&mut body, c as u64);
                }
                let mut prev_off = 0usize;
                for (k, &o) in offs.iter().enumerate() {
                    if k == 0 {
                        put_uvarint(&mut body, (o + 2) as u64);
                    } else {
                        put_uvarint(&mut body, (o - prev_off + 2) as u64);
                    }
                    prev_off = o;
                }
            }
            let mut out = Vec::with_capacity(body.len() + 2);
            put_uvarint(&mut out, (body.len() as u64) << 1);
            out.extend_from_slice(&body);
            out
        }
    }
}

fn decode_poslist_body(detail: DetailMode, body: &[u8]) -> Result<BTreeMap<usize, Vec<usize>>> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let mut out: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    match detail {
        DetailMode::None => {
            out.insert(0, Vec::new());
        }
        DetailMode::Col => {
            let mut pos = 0usize;
            let mut cur_col = 0usize;
            let mut first = true;
            while pos < body.len() {
                let v = get_uvarint(body, &mut pos).ok_or_else(err_corrupt)? as usize;
                if v < 2 {
                    return Err(err_corrupt());
                }
                if first {
                    cur_col = v - 2;
                    first = false;
                } else {
                    cur_col = cur_col.checked_add(v - 2).ok_or_else(err_corrupt)?;
                }
                out.insert(cur_col, Vec::new());
            }
        }
        DetailMode::Full => {
            let mut pos = 0usize;
            let mut cur_col = 0usize;
            let mut cur_off = 0usize;
            let mut first_in_col = true;
            while pos < body.len() {
                let v = get_uvarint(body, &mut pos).ok_or_else(err_corrupt)? as usize;
                if v == 1 {
                    let next_col = get_uvarint(body, &mut pos).ok_or_else(err_corrupt)? as usize;
                    cur_col = next_col;
                    cur_off = 0;
                    first_in_col = true;
                    continue;
                }
                if v == 0 {
                    break;
                }
                if first_in_col {
                    cur_off = v - 2;
                    first_in_col = false;
                } else {
                    cur_off = cur_off.checked_add(v - 2).ok_or_else(err_corrupt)?;
                }
                out.entry(cur_col).or_default().push(cur_off);
            }
        }
    }
    Ok(out)
}

/// Writes `merged` into `<tbl>_data` and `<tbl>_idx` as a canonical C SQLite 3.54.1 FTS5 segment (`segid = 1`).
fn write_c_fts5_index(
    ctx: &mut dyn VTabHostContext,
    db_q: &str,
    tbl_name: &str,
    cfg: &Fts5Config,
    merged: &MergedIndexMap,
) -> Result<()> {
    let data_q = quote_ident(&format!("{tbl_name}_data"));
    let idx_q = quote_ident(&format!("{tbl_name}_idx"));
    ctx.execute_sql(
        &format!("DELETE FROM {db_q}.{data_q} WHERE id > {FTS5_STRUCTURE_ROWID}"),
        &[],
    )?;
    ctx.execute_sql(&format!("DELETE FROM {db_q}.{idx_q}"), &[])?;

    if merged.is_empty() {
        ctx.execute_sql(
            &format!("INSERT OR REPLACE INTO {db_q}.{data_q}(id, block) VALUES(?1, ?2)"),
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(encode_empty_structure_record(cfg.contentless_delete)),
            ],
        )?;
        return Ok(());
    }

    let segid = 1u64;
    let mut pgno = 1u64;
    let mut page_buf = vec![0u8; 4];
    let mut pgidx = Vec::new();
    let mut prev_pgidx_off = 0usize;
    let mut prev_term_on_page: Vec<u8> = Vec::new();
    let mut prev_term_in_seg: Vec<u8> = Vec::new();
    let mut first_term_on_page = true;
    let mut first_rowid_on_page = true;
    let mut btterm: Option<(u64, Vec<u8>)> = Some((1, Vec::new()));
    let mut leaves: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut idx_rows: Vec<(Vec<u8>, i64)> = Vec::new();

    let flush_leaf = |page_buf: &mut Vec<u8>,
                      pgidx: &mut Vec<u8>,
                      prev_pgidx_off: &mut usize,
                      prev_term_on_page: &mut Vec<u8>,
                      pgno: &mut u64,
                      first_term_on_page: &mut bool,
                      first_rowid_on_page: &mut bool,
                      leaves: &mut Vec<(u64, Vec<u8>)>| {
        let footer_off = page_buf.len() as u16;
        page_buf[2..4].copy_from_slice(&footer_off.to_be_bytes());
        if !*first_term_on_page {
            page_buf.extend_from_slice(pgidx);
        }
        let finished = std::mem::replace(page_buf, vec![0u8; 4]);
        leaves.push((*pgno, finished));
        pgidx.clear();
        *prev_pgidx_off = 0;
        prev_term_on_page.clear();
        *pgno += 1;
        *first_term_on_page = true;
        *first_rowid_on_page = true;
    };

    for (term, docs) in merged {
        if page_buf.len() + pgidx.len() + term.len() + 2 >= FTS5_LEAF_TARGET_PGSZ
            && page_buf.len() > 4
        {
            flush_leaf(
                &mut page_buf,
                &mut pgidx,
                &mut prev_pgidx_off,
                &mut prev_term_on_page,
                &mut pgno,
                &mut first_term_on_page,
                &mut first_rowid_on_page,
                &mut leaves,
            );
        }

        put_uvarint(&mut pgidx, (page_buf.len() - prev_pgidx_off) as u64);
        prev_pgidx_off = page_buf.len();

        if first_term_on_page {
            if pgno != 1 {
                if let Some((bt_pg, bt_key)) = btterm.take() {
                    idx_rows.push((bt_key, (bt_pg as i64) << 1));
                }
                let split_len = if prev_term_in_seg.is_empty() {
                    term.len()
                } else {
                    (1 + common_prefix_len(&prev_term_in_seg, term)).min(term.len())
                };
                btterm = Some((pgno, term[..split_len].to_vec()));
            }
            put_uvarint(&mut page_buf, term.len() as u64);
            page_buf.extend_from_slice(term);
        } else {
            let n_prefix = common_prefix_len(&prev_term_on_page, term);
            put_uvarint(&mut page_buf, n_prefix as u64);
            put_uvarint(&mut page_buf, (term.len() - n_prefix) as u64);
            page_buf.extend_from_slice(&term[n_prefix..]);
        }

        prev_term_on_page.clear();
        prev_term_on_page.extend_from_slice(term);
        prev_term_in_seg.clear();
        prev_term_in_seg.extend_from_slice(term);
        first_term_on_page = false;
        first_rowid_on_page = false;

        let mut first_rowid_in_doclist = true;
        let mut prev_rowid = 0i64;

        for (&rowid, col_hits) in docs {
            let poslist = encode_poslist(cfg.detail, col_hits);
            if ((!first_rowid_in_doclist && page_buf.len() + pgidx.len() >= FTS5_LEAF_TARGET_PGSZ)
                || page_buf.len() + pgidx.len() + 10 + poslist.len() >= 16_000)
                && page_buf.len() > 4
            {
                flush_leaf(
                    &mut page_buf,
                    &mut pgidx,
                    &mut prev_pgidx_off,
                    &mut prev_term_on_page,
                    &mut pgno,
                    &mut first_term_on_page,
                    &mut first_rowid_on_page,
                    &mut leaves,
                );
            }
            if first_rowid_on_page {
                let roff = page_buf.len() as u16;
                page_buf[0..2].copy_from_slice(&roff.to_be_bytes());
            }
            if first_rowid_in_doclist || first_rowid_on_page {
                put_uvarint(&mut page_buf, rowid as u64);
            } else {
                put_uvarint(
                    &mut page_buf,
                    (rowid as u64).wrapping_sub(prev_rowid as u64),
                );
            }
            prev_rowid = rowid;
            first_rowid_in_doclist = false;
            first_rowid_on_page = false;
            page_buf.extend_from_slice(&poslist);
        }
    }

    if page_buf.len() > 4 {
        flush_leaf(
            &mut page_buf,
            &mut pgidx,
            &mut prev_pgidx_off,
            &mut prev_term_on_page,
            &mut pgno,
            &mut first_term_on_page,
            &mut first_rowid_on_page,
            &mut leaves,
        );
    }
    let pgno_last = pgno - 1;
    if pgno_last >= 1
        && let Some((bt_pg, bt_key)) = btterm.take()
    {
        idx_rows.push((bt_key, (bt_pg as i64) << 1));
    }

    for (leaf_pgno, leaf_bytes) in leaves {
        ctx.execute_sql(
            &format!("INSERT INTO {db_q}.{data_q}(id, block) VALUES(?1, ?2)"),
            &[
                Value::Integer(fts5_segment_rowid(segid, leaf_pgno)),
                Value::Blob(leaf_bytes),
            ],
        )?;
    }
    for (bt_key, bt_pg_val) in idx_rows {
        ctx.execute_sql(
            &format!("INSERT INTO {db_q}.{idx_q}(segid, term, pgno) VALUES(?1, ?2, ?3)"),
            &[
                Value::Integer(segid as i64),
                Value::Blob(bt_key),
                Value::Integer(bt_pg_val),
            ],
        )?;
    }
    ctx.execute_sql(
        &format!("INSERT OR REPLACE INTO {db_q}.{data_q}(id, block) VALUES(?1, ?2)"),
        &[
            Value::Integer(FTS5_STRUCTURE_ROWID),
            Value::Blob(encode_single_seg_structure_record(
                cfg.contentless_delete,
                segid,
                pgno_last,
            )),
        ],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Fts5SegRecord {
    segid: u64,
    pgno_first: u64,
    pgno_last: u64,
    n_tombstone_pages: u64,
}

fn decode_structure_segments(blob: &[u8]) -> Result<Vec<Fts5SegRecord>> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    if blob.len() < 7 {
        return Err(err_corrupt());
    }
    let is_v2 = blob.len() >= 11 && &blob[4..8] == b"\xff\x00\x00\x01";
    let mut pos = if is_v2 { 8usize } else { 4usize };
    let n_level = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)? as usize;
    let _n_total_seg = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
    let _write_cntr = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
    if n_level > 128 {
        return Err(err_corrupt());
    }
    let mut levels: Vec<Vec<Fts5SegRecord>> = Vec::with_capacity(n_level);
    for _ in 0..n_level {
        let _n_merge = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
        let n_seg = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)? as usize;
        if n_seg > 65_536 {
            return Err(err_corrupt());
        }
        let mut lsegs = Vec::with_capacity(n_seg);
        for _ in 0..n_seg {
            let segid = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
            let pgno_first = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
            let pgno_last = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
            let n_tombstone_pages = if is_v2 {
                let _o1 = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                let _o2 = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?
            } else {
                0
            };
            lsegs.push(Fts5SegRecord {
                segid,
                pgno_first,
                pgno_last,
                n_tombstone_pages,
            });
        }
        levels.push(lsegs);
    }
    if pos != blob.len() {
        return Err(err_corrupt());
    }
    // Oldest level is `n_level - 1`, newest is `0`; within each level `aSeg[0]` is oldest.
    let mut chronological = Vec::new();
    for lvl in levels.into_iter().rev() {
        chronological.extend(lvl);
    }
    Ok(chronological)
}

fn load_segment_tombstones(
    ctx: &mut dyn VTabHostContext,
    db_q: &str,
    data_q: &str,
    segid: u64,
    n_pages: u64,
) -> Result<BTreeSet<i64>> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let mut out = BTreeSet::new();
    for ipg in 0..n_pages {
        let tid = fts5_tombstone_rowid(segid, ipg);
        let rows = ctx.query_sql(
            &format!("SELECT block FROM {db_q}.{data_q} WHERE id = ?1"),
            &[Value::Integer(tid)],
        )?;
        let Some(Value::Blob(b)) = rows.first().and_then(|r| r.first()) else {
            return Err(err_corrupt());
        };
        if b.len() < 8 {
            return Err(err_corrupt());
        }
        let key_sz = b[0] as usize;
        if ipg == 0 && b[1] != 0 {
            out.insert(0);
        }
        match key_sz {
            4 => {
                for chunk in b[8..].as_chunks::<4>().0 {
                    let v = u32::from_be_bytes(*chunk);
                    if v != 0 {
                        out.insert(i64::from(v));
                    }
                }
            }
            8 => {
                for chunk in b[8..].as_chunks::<8>().0 {
                    let v = u64::from_be_bytes(*chunk) as i64;
                    if v != 0 {
                        out.insert(v);
                    }
                }
            }
            _ => return Err(err_corrupt()),
        }
    }
    Ok(out)
}

/// Merges all active C SQLite 3.54.1 FTS5 segments in chronological order into `MergedIndexMap`.
fn load_merged_index(
    ctx: &mut dyn VTabHostContext,
    db_q: &str,
    tbl_name: &str,
    detail: DetailMode,
) -> Result<MergedIndexMap> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let data_q = quote_ident(&format!("{tbl_name}_data"));
    let srows = ctx.query_sql(
        &format!("SELECT block FROM {db_q}.{data_q} WHERE id = {FTS5_STRUCTURE_ROWID}"),
        &[],
    )?;
    let Some(Value::Blob(struct_blob)) = srows.first().and_then(|r| r.first()) else {
        return Err(err_corrupt());
    };
    let segments = decode_structure_segments(struct_blob)?;
    let mut merged: MergedIndexMap = BTreeMap::new();

    for seg in segments {
        if seg.pgno_first == 0 || seg.pgno_last < seg.pgno_first {
            continue;
        }
        let tombstones = if seg.n_tombstone_pages > 0 {
            load_segment_tombstones(ctx, db_q, &data_q, seg.segid, seg.n_tombstone_pages)?
        } else {
            BTreeSet::new()
        };

        let first_id = fts5_segment_rowid(seg.segid, seg.pgno_first);
        let last_id = fts5_segment_rowid(seg.segid, seg.pgno_last);
        let leaf_rows = ctx.query_sql(
            &format!(
                "SELECT id, block FROM {db_q}.{data_q} WHERE id >= ?1 AND id <= ?2 ORDER BY id ASC"
            ),
            &[Value::Integer(first_id), Value::Integer(last_id)],
        )?;
        let expected_leaves = (seg.pgno_last - seg.pgno_first + 1) as usize;
        if leaf_rows.len() != expected_leaves {
            return Err(err_corrupt());
        }

        let mut cur_term: Option<Vec<u8>> = None;
        let mut cur_rowid: i64 = 0;
        let mut first_in_doclist = true;
        let mut pending_poslist: Option<(bool, usize, Vec<u8>)> = None;

        let mut apply_posting = |term: &[u8], rid: i64, del: bool, body: &[u8]| -> Result<()> {
            let entry = merged.entry(term.to_vec()).or_default();
            if del || tombstones.contains(&rid) {
                entry.remove(&rid);
            } else {
                let hits = decode_poslist_body(detail, body)?;
                entry.insert(rid, hits);
            }
            Ok(())
        };

        for lr in leaf_rows {
            let Some(Value::Blob(block)) = lr.get(1) else {
                return Err(err_corrupt());
            };
            if block.len() < 4 {
                return Err(err_corrupt());
            }
            let first_rowid_off = u16::from_be_bytes([block[0], block[1]]) as usize;
            let footer_off = u16::from_be_bytes([block[2], block[3]]) as usize;
            if footer_off < 4 || footer_off > block.len() {
                return Err(err_corrupt());
            }

            let mut term_offsets = Vec::new();
            let mut fpos = footer_off;
            let mut acc_off = 0usize;
            while fpos < block.len() {
                let d = get_uvarint(block, &mut fpos).ok_or_else(err_corrupt)? as usize;
                acc_off = acc_off.checked_add(d).ok_or_else(err_corrupt)?;
                if acc_off < 4 || acc_off >= footer_off {
                    return Err(err_corrupt());
                }
                term_offsets.push(acc_off);
            }

            type PostingApplyFn<'a> = dyn FnMut(&[u8], i64, bool, &[u8]) -> Result<()> + 'a;
            let parse_doclist_slice = |slice_start: usize,
                                       slice_end: usize,
                                       continues_to_next_page: bool,
                                       cur_term: &Option<Vec<u8>>,
                                       cur_rowid: &mut i64,
                                       first_in_doclist: &mut bool,
                                       pending_poslist: &mut Option<(bool, usize, Vec<u8>)>,
                                       apply_posting: &mut PostingApplyFn<'_>|
             -> Result<()> {
                let mut pos = slice_start;
                if let Some((del, rem, mut pbuf)) = pending_poslist.take() {
                    let Some(t) = cur_term.as_deref() else {
                        return Err(err_corrupt());
                    };
                    let avail = slice_end.saturating_sub(pos);
                    let take = rem.min(avail);
                    pbuf.extend_from_slice(&block[pos..pos + take]);
                    pos += take;
                    if rem > take {
                        if !continues_to_next_page {
                            return Err(err_corrupt());
                        }
                        *pending_poslist = Some((del, rem - take, pbuf));
                        return Ok(());
                    }
                    apply_posting(t, *cur_rowid, del, &pbuf)?;
                }

                while pos < slice_end {
                    let Some(t) = cur_term.as_deref() else {
                        return Err(err_corrupt());
                    };
                    let is_page_first_rowid = first_rowid_off > 0 && pos == first_rowid_off;
                    let raw_rv = get_uvarint(block, &mut pos).ok_or_else(err_corrupt)?;
                    if *first_in_doclist || is_page_first_rowid {
                        *cur_rowid = raw_rv as i64;
                        *first_in_doclist = false;
                    } else {
                        *cur_rowid = (*cur_rowid).wrapping_add(raw_rv as i64);
                    }

                    match detail {
                        DetailMode::None => {
                            let mut del = false;
                            if pos < slice_end && block[pos] == 0 {
                                del = true;
                                pos += 1;
                                if pos < slice_end && block[pos] == 0 {
                                    pos += 1;
                                }
                            }
                            apply_posting(t, *cur_rowid, del, &[])?;
                        }
                        DetailMode::Col | DetailMode::Full => {
                            let sz_flags =
                                get_uvarint(block, &mut pos).ok_or_else(err_corrupt)? as usize;
                            let del = (sz_flags & 1) != 0;
                            let plen = sz_flags >> 1;
                            let avail = slice_end.saturating_sub(pos);
                            if plen <= avail {
                                let pbody = &block[pos..pos + plen];
                                pos += plen;
                                apply_posting(t, *cur_rowid, del, pbody)?;
                            } else {
                                if !continues_to_next_page {
                                    return Err(err_corrupt());
                                }
                                let mut pbuf = Vec::with_capacity(plen.min(65_536));
                                pbuf.extend_from_slice(&block[pos..slice_end]);
                                *pending_poslist = Some((del, plen - avail, pbuf));
                                pos = slice_end;
                            }
                        }
                    }
                }
                Ok(())
            };

            let first_term_end = term_offsets.first().copied().unwrap_or(footer_off);
            if first_term_end > 4 {
                parse_doclist_slice(
                    4,
                    first_term_end,
                    term_offsets.is_empty(),
                    &cur_term,
                    &mut cur_rowid,
                    &mut first_in_doclist,
                    &mut pending_poslist,
                    &mut apply_posting,
                )?;
            }

            let mut prev_page_term: Vec<u8> = Vec::new();
            for (k, &toff) in term_offsets.iter().enumerate() {
                if pending_poslist.is_some() {
                    return Err(err_corrupt());
                }
                let tend = term_offsets.get(k + 1).copied().unwrap_or(footer_off);
                let mut pos = toff;
                let term_bytes = if k == 0 {
                    let tlen = get_uvarint(block, &mut pos).ok_or_else(err_corrupt)? as usize;
                    if pos + tlen > tend {
                        return Err(err_corrupt());
                    }
                    let tb = block[pos..pos + tlen].to_vec();
                    pos += tlen;
                    tb
                } else {
                    let npref = get_uvarint(block, &mut pos).ok_or_else(err_corrupt)? as usize;
                    let nsuf = get_uvarint(block, &mut pos).ok_or_else(err_corrupt)? as usize;
                    if npref > prev_page_term.len() || pos + nsuf > tend {
                        return Err(err_corrupt());
                    }
                    let mut tb = Vec::with_capacity(npref + nsuf);
                    tb.extend_from_slice(&prev_page_term[..npref]);
                    tb.extend_from_slice(&block[pos..pos + nsuf]);
                    pos += nsuf;
                    tb
                };
                prev_page_term.clone_from(&term_bytes);
                cur_term = Some(term_bytes);
                first_in_doclist = true;
                parse_doclist_slice(
                    pos,
                    tend,
                    k + 1 == term_offsets.len(),
                    &cur_term,
                    &mut cur_rowid,
                    &mut first_in_doclist,
                    &mut pending_poslist,
                    &mut apply_posting,
                )?;
            }
        }

        if pending_poslist.is_some() {
            return Err(err_corrupt());
        }
    }

    merged.retain(|_, docs| !docs.is_empty());
    Ok(merged)
}

fn make_term_key(prefix_slot: u8, term: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + term.len());
    key.push(b'0' + prefix_slot);
    key.extend_from_slice(term.as_bytes());
    key
}

fn build_doc_term_postings(
    cfg: &Fts5Config,
    rowid: i64,
    col_vals: &[Value],
    deleted: bool,
) -> (BTreeMap<Vec<u8>, Vec<DocPosting>>, Vec<u64>) {
    let mut term_cols: BTreeMap<Vec<u8>, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
    let mut col_sizes = vec![0u64; cfg.columns.len()];

    for (col_idx, val) in col_vals.iter().enumerate().take(cfg.columns.len()) {
        if cfg.unindexed[col_idx] || val.is_null() {
            continue;
        }
        let text = val.to_text_lossy();
        let tokens = cfg.tokenizer.tokenize(&text, false);
        col_sizes[col_idx] = tokens.len() as u64;
        for (tok_pos, tok) in tokens.iter().enumerate() {
            let main_key = make_term_key(0, &tok.term);
            record_posting_hit(&mut term_cols, main_key, col_idx, tok_pos, cfg.detail);

            for (p_idx, &plen) in cfg.prefixes.iter().enumerate() {
                if tok.term.chars().count() >= plen {
                    let pref: String = tok.term.chars().take(plen).collect();
                    let p_key = make_term_key((p_idx + 1) as u8, &pref);
                    record_posting_hit(&mut term_cols, p_key, col_idx, tok_pos, cfg.detail);
                }
            }
        }
    }

    let mut out = BTreeMap::new();
    for (tkey, chits) in term_cols {
        out.insert(
            tkey,
            vec![DocPosting {
                rowid,
                deleted,
                col_hits: if deleted { BTreeMap::new() } else { chits },
            }],
        );
    }
    (out, col_sizes)
}

fn record_posting_hit(
    term_cols: &mut BTreeMap<Vec<u8>, BTreeMap<usize, Vec<usize>>>,
    key: Vec<u8>,
    col_idx: usize,
    tok_pos: usize,
    detail: DetailMode,
) {
    let by_col = term_cols.entry(key).or_default();
    match detail {
        DetailMode::Full => {
            by_col.entry(col_idx).or_default().push(tok_pos);
        }
        DetailMode::Col => {
            by_col.entry(col_idx).or_default();
        }
        DetailMode::None => {
            by_col.entry(0).or_default();
        }
    }
}

// ============================================================================
// FTS5 Query AST & Bounded-Depth Recursive-Descent Parser
// ============================================================================

#[derive(Debug, Clone)]
struct QueryPhraseToken {
    term: String,
    is_prefix: bool,
}

#[derive(Debug, Clone)]
struct QueryPhrase {
    tokens: Vec<QueryPhraseToken>,
    anchored_start: bool,
    /// Raw string for trigram substring verification.
    raw_text: String,
}

#[derive(Debug, Clone)]
enum ColFilterSpec {
    Include(BTreeSet<usize>),
    Exclude(BTreeSet<usize>),
}

#[derive(Debug, Clone)]
enum FtsQueryExpr {
    Phrase(usize),
    Near {
        phrase_indices: Vec<usize>,
        distance: usize,
    },
    ColFilter {
        filter: ColFilterSpec,
        inner: Box<FtsQueryExpr>,
    },
    And(Box<FtsQueryExpr>, Box<FtsQueryExpr>),
    Or(Box<FtsQueryExpr>, Box<FtsQueryExpr>),
    Not(Box<FtsQueryExpr>, Box<FtsQueryExpr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LexTok {
    Word { text: String, quoted: bool },
    Star,
    Plus,
    Caret,
    Colon,
    Minus,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
}

fn lex_fts5_query(src: &str) -> Result<Vec<LexTok>> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'*' => {
                out.push(LexTok::Star);
                i += 1;
            }
            b'+' => {
                out.push(LexTok::Plus);
                i += 1;
            }
            b'^' => {
                out.push(LexTok::Caret);
                i += 1;
            }
            b':' => {
                out.push(LexTok::Colon);
                i += 1;
            }
            b'-' => {
                out.push(LexTok::Minus);
                i += 1;
            }
            b'(' => {
                out.push(LexTok::LParen);
                i += 1;
            }
            b')' => {
                out.push(LexTok::RParen);
                i += 1;
            }
            b'{' => {
                out.push(LexTok::LBrace);
                i += 1;
            }
            b'}' => {
                out.push(LexTok::RBrace);
                i += 1;
            }
            b',' => {
                out.push(LexTok::Comma);
                i += 1;
            }
            b'"' => {
                i += 1;
                let mut s = String::new();
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                            s.push('"');
                            i += 2;
                        } else {
                            i += 1;
                            closed = true;
                            break;
                        }
                    } else {
                        let ch = src[i..].chars().next().unwrap_or('\0');
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if !closed {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "fts5: unterminated string in query",
                    ));
                }
                out.push(LexTok::Word {
                    text: s,
                    quoted: true,
                });
            }
            _ => {
                let start = i;
                while i < bytes.len() {
                    let c = bytes[i];
                    if c.is_ascii_whitespace()
                        || matches!(
                            c,
                            b'*' | b'+' | b'^' | b':' | b'(' | b')' | b'{' | b'}' | b',' | b'"'
                        )
                    {
                        break;
                    }
                    // Allow internal '-' in barewords only if configured in tokenchars? In FTS5 barewords are
                    // ASCII alnum, '_', UTF-8 >= 0x80, or 0x1a. Any other punctuation is a syntax error!
                    if c < 0x80 && !(c.is_ascii_alphanumeric() || c == b'_' || c == 0x1a) {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("fts5: syntax error near \"{}\"", c as char),
                        ));
                    }
                    let ch = src[i..].chars().next().unwrap_or('\0');
                    i += ch.len_utf8();
                }
                out.push(LexTok::Word {
                    text: src[start..i].to_owned(),
                    quoted: false,
                });
            }
        }
    }
    Ok(out)
}

struct QueryParser<'a> {
    toks: &'a [LexTok],
    pos: usize,
    cfg: &'a Fts5Config,
    phrases: Vec<QueryPhrase>,
}

impl<'a> QueryParser<'a> {
    fn parse_root(&mut self) -> Result<FtsQueryExpr> {
        if self.toks.is_empty() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "fts5: syntax error near \"\"",
            ));
        }
        let expr = self.parse_or(0)?;
        if self.pos < self.toks.len() {
            return Err(self.syntax_err_here());
        }
        Ok(expr)
    }

    fn syntax_err_here(&self) -> SqliteError {
        let near = match self.toks.get(self.pos) {
            Some(LexTok::Word { text, .. }) => text.as_str(),
            Some(LexTok::Star) => "*",
            Some(LexTok::Plus) => "+",
            Some(LexTok::Caret) => "^",
            Some(LexTok::Colon) => ":",
            Some(LexTok::Minus) => "-",
            Some(LexTok::LParen) => "(",
            Some(LexTok::RParen) => ")",
            Some(LexTok::LBrace) => "{",
            Some(LexTok::RBrace) => "}",
            Some(LexTok::Comma) => ",",
            None => "",
        };
        SqliteError::new(
            ErrorCode::Error,
            format!("fts5: syntax error near \"{near}\""),
        )
    }

    fn check_depth(depth: usize) -> Result<()> {
        if depth > FTS5_MAX_EXPR_DEPTH {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "fts5: query expression tree is too large",
            ));
        }
        Ok(())
    }

    fn peek(&self) -> Option<&LexTok> {
        self.toks.get(self.pos)
    }

    fn is_unquoted_kw(&self, kw: &str) -> bool {
        matches!(
            self.peek(),
            Some(LexTok::Word { text, quoted: false }) if text == kw
        )
    }

    fn parse_or(&mut self, depth: usize) -> Result<FtsQueryExpr> {
        Self::check_depth(depth)?;
        let mut lhs = self.parse_and(depth + 1)?;
        while self.is_unquoted_kw("OR") {
            self.pos += 1;
            let rhs = self.parse_and(depth + 1)?;
            lhs = FtsQueryExpr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn can_start_implicit_and(&self) -> bool {
        if self.is_unquoted_kw("OR") || self.is_unquoted_kw("AND") || self.is_unquoted_kw("NOT") {
            return false;
        }
        matches!(
            self.peek(),
            Some(
                LexTok::Word { .. }
                    | LexTok::Caret
                    | LexTok::Minus
                    | LexTok::LBrace
                    | LexTok::LParen
            )
        )
    }

    fn parse_and(&mut self, depth: usize) -> Result<FtsQueryExpr> {
        Self::check_depth(depth)?;
        let mut lhs = self.parse_not(depth + 1)?;
        loop {
            if self.is_unquoted_kw("AND") {
                self.pos += 1;
                let rhs = self.parse_not(depth + 1)?;
                lhs = FtsQueryExpr::And(Box::new(lhs), Box::new(rhs));
            } else if self.can_start_implicit_and() {
                let rhs = self.parse_not(depth + 1)?;
                lhs = FtsQueryExpr::And(Box::new(lhs), Box::new(rhs));
            } else {
                break;
            }
        }
        Ok(lhs)
    }

    fn parse_not(&mut self, depth: usize) -> Result<FtsQueryExpr> {
        Self::check_depth(depth)?;
        let mut lhs = self.parse_col_filter(depth + 1)?;
        while self.is_unquoted_kw("NOT") {
            self.pos += 1;
            let rhs = self.parse_col_filter(depth + 1)?;
            lhs = FtsQueryExpr::Not(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn resolve_col_name(&self, cname: &str) -> Result<usize> {
        self.cfg
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(cname))
            .ok_or_else(|| {
                SqliteError::new(ErrorCode::Error, format!("fts5: no such column: {cname}"))
            })
    }

    fn parse_col_filter(&mut self, depth: usize) -> Result<FtsQueryExpr> {
        Self::check_depth(depth)?;
        if self.is_unquoted_kw("OR") || self.is_unquoted_kw("AND") || self.is_unquoted_kw("NOT") {
            return Err(self.syntax_err_here());
        }

        // Check `[- ] (col | { col1 col2 }) : primary`
        let saved = self.pos;
        let negated = if self.peek() == Some(&LexTok::Minus) {
            self.pos += 1;
            true
        } else {
            false
        };

        if self.peek() == Some(&LexTok::LBrace) {
            self.pos += 1;
            let mut cols = BTreeSet::new();
            while let Some(LexTok::Word { text, .. }) = self.peek() {
                let idx = self.resolve_col_name(text)?;
                cols.insert(idx);
                self.pos += 1;
            }
            if self.peek() != Some(&LexTok::RBrace) {
                return Err(self.syntax_err_here());
            }
            self.pos += 1;
            if self.peek() != Some(&LexTok::Colon) {
                return Err(self.syntax_err_here());
            }
            self.pos += 1;
            let inner = self.parse_col_filter(depth + 1)?;
            let filter = if negated {
                ColFilterSpec::Exclude(cols)
            } else {
                ColFilterSpec::Include(cols)
            };
            return Ok(FtsQueryExpr::ColFilter {
                filter,
                inner: Box::new(inner),
            });
        }

        if let Some(LexTok::Word { text, .. }) = self.peek()
            && self.toks.get(self.pos + 1) == Some(&LexTok::Colon)
        {
            let cname = text.clone();
            let idx = self.resolve_col_name(&cname)?;
            self.pos += 2;
            let mut cols = BTreeSet::new();
            cols.insert(idx);
            let inner = self.parse_col_filter(depth + 1)?;
            let filter = if negated {
                ColFilterSpec::Exclude(cols)
            } else {
                ColFilterSpec::Include(cols)
            };
            return Ok(FtsQueryExpr::ColFilter {
                filter,
                inner: Box::new(inner),
            });
        }

        if negated {
            self.pos = saved;
            return Err(self.syntax_err_here());
        }

        self.parse_primary(depth + 1)
    }

    fn parse_primary(&mut self, depth: usize) -> Result<FtsQueryExpr> {
        Self::check_depth(depth)?;
        if self.peek() == Some(&LexTok::LParen) {
            self.pos += 1;
            let inner = self.parse_or(depth + 1)?;
            if self.peek() != Some(&LexTok::RParen) {
                return Err(self.syntax_err_here());
            }
            self.pos += 1;
            return Ok(inner);
        }

        // Check `NEAR(p1 p2 ..., N)`
        if matches!(
            self.peek(),
            Some(LexTok::Word { text, quoted: false }) if text == "NEAR"
        ) && self.toks.get(self.pos + 1) == Some(&LexTok::LParen)
        {
            self.pos += 2; // skip NEAR (
            let mut phrase_indices = Vec::new();
            while self.peek().is_some()
                && self.peek() != Some(&LexTok::Comma)
                && self.peek() != Some(&LexTok::RParen)
            {
                let pidx = self.parse_phrase()?;
                phrase_indices.push(pidx);
            }
            if phrase_indices.is_empty() {
                return Err(self.syntax_err_here());
            }
            let mut distance = 10usize;
            if self.peek() == Some(&LexTok::Comma) {
                self.pos += 1;
                let Some(LexTok::Word {
                    text,
                    quoted: false,
                }) = self.peek()
                else {
                    return Err(self.syntax_err_here());
                };
                distance = text.parse::<usize>().map_err(|_| self.syntax_err_here())?;
                self.pos += 1;
            }
            if self.peek() != Some(&LexTok::RParen) {
                return Err(self.syntax_err_here());
            }
            self.pos += 1;
            return Ok(FtsQueryExpr::Near {
                phrase_indices,
                distance,
            });
        }

        let pidx = self.parse_phrase()?;
        Ok(FtsQueryExpr::Phrase(pidx))
    }

    fn parse_phrase(&mut self) -> Result<usize> {
        let anchored_start = if self.peek() == Some(&LexTok::Caret) {
            self.pos += 1;
            true
        } else {
            false
        };

        let mut tokens: Vec<QueryPhraseToken> = Vec::new();
        let mut raw_parts: Vec<String> = Vec::new();

        loop {
            let Some(LexTok::Word { text, .. }) = self.peek().cloned() else {
                return Err(self.syntax_err_here());
            };
            self.pos += 1;
            let is_prefix = if self.peek() == Some(&LexTok::Star) {
                self.pos += 1;
                true
            } else {
                false
            };

            raw_parts.push(text.clone());
            let sub_toks = self.cfg.tokenizer.tokenize(&text, true);
            let n_sub = sub_toks.len();
            for (i, st) in sub_toks.into_iter().enumerate() {
                tokens.push(QueryPhraseToken {
                    term: st.term,
                    is_prefix: is_prefix && (i + 1 == n_sub) && !self.cfg.tokenizer.is_trigram(),
                });
            }

            if self.peek() == Some(&LexTok::Plus) {
                self.pos += 1;
            } else {
                break;
            }
        }

        let pidx = self.phrases.len();
        self.phrases.push(QueryPhrase {
            tokens,
            anchored_start,
            raw_text: raw_parts.join(" "),
        });
        Ok(pidx)
    }
}

// ============================================================================
// Postings Evaluation Engine
// ============================================================================

/// Per-document match map: `rowid -> Vec<PhraseInstance>`
type EvalDocSet = BTreeMap<i64, Vec<PhraseInstance>>;

fn eval_phrase_against_index(
    phrase_idx: usize,
    phrase: &QueryPhrase,
    cfg: &Fts5Config,
    merged_index: &MergedIndexMap,
) -> EvalDocSet {
    if phrase.tokens.is_empty() {
        return BTreeMap::new();
    }

    // Step 1: For each token in `phrase.tokens`, gather `rowid -> col -> BTreeSet<offset>`
    let mut token_hits: Vec<BTreeMap<i64, BTreeMap<usize, BTreeSet<usize>>>> =
        Vec::with_capacity(phrase.tokens.len());

    for ptok in &phrase.tokens {
        let mut doc_map: BTreeMap<i64, BTreeMap<usize, BTreeSet<usize>>> = BTreeMap::new();
        if ptok.is_prefix {
            // Check if configured prefix index matches exact char length, else range scan main index (`b'0'`)
            let char_len = ptok.term.chars().count();
            if let Some(p_pos) = cfg.prefixes.iter().position(|&n| n == char_len) {
                let p_key = make_term_key((p_pos + 1) as u8, &ptok.term);
                if let Some(docs) = merged_index.get(&p_key) {
                    merge_term_docs(&mut doc_map, docs);
                }
            } else {
                let start_key = make_term_key(0, &ptok.term);
                for (k, docs) in merged_index.range(start_key.clone()..) {
                    if !k.starts_with(&start_key) {
                        break;
                    }
                    merge_term_docs(&mut doc_map, docs);
                }
            }
        } else {
            let exact_key = make_term_key(0, &ptok.term);
            if let Some(docs) = merged_index.get(&exact_key) {
                merge_term_docs(&mut doc_map, docs);
            }
        }
        if doc_map.is_empty() {
            return BTreeMap::new();
        }
        token_hits.push(doc_map);
    }

    // Step 2: Intersect across tokens in the phrase
    let mut result = EvalDocSet::new();
    let first_docs = &token_hits[0];
    'doc_loop: for (&rowid, first_cols) in first_docs {
        for other in &token_hits[1..] {
            if !other.contains_key(&rowid) {
                continue 'doc_loop;
            }
        }

        let mut instances = Vec::new();
        for (&col_idx, first_offs) in first_cols {
            if cfg.detail == DetailMode::Full {
                for &start_off in first_offs {
                    if phrase.anchored_start && start_off != 0 {
                        continue;
                    }
                    let mut all_present = true;
                    for (delta, other) in token_hits.iter().enumerate().skip(1) {
                        let has = other
                            .get(&rowid)
                            .and_then(|cm| cm.get(&col_idx))
                            .is_some_and(|os| os.contains(&(start_off + delta)));
                        if !has {
                            all_present = false;
                            break;
                        }
                    }
                    if all_present {
                        instances.push(PhraseInstance {
                            phrase_idx,
                            col_idx,
                            token_offset: start_off,
                        });
                    }
                }
            } else {
                // `detail=col` or `detail=none` (for single-token queries)
                let mut all_present = !phrase.anchored_start && phrase.tokens.len() == 1;
                if phrase.tokens.len() > 1 {
                    all_present = token_hits[1..].iter().all(|other| {
                        other
                            .get(&rowid)
                            .is_some_and(|cm| cm.contains_key(&col_idx))
                    });
                }
                if all_present {
                    instances.push(PhraseInstance {
                        phrase_idx,
                        col_idx,
                        token_offset: 0,
                    });
                }
            }
        }

        if !instances.is_empty() {
            result.insert(rowid, instances);
        }
    }

    result
}

fn merge_term_docs(
    dst: &mut BTreeMap<i64, BTreeMap<usize, BTreeSet<usize>>>,
    src: &BTreeMap<i64, BTreeMap<usize, Vec<usize>>>,
) {
    for (&rowid, cols) in src {
        let dcols = dst.entry(rowid).or_default();
        for (&cidx, offs) in cols {
            let doffs = dcols.entry(cidx).or_default();
            for &o in offs {
                doffs.insert(o);
            }
        }
    }
}

fn eval_query_expr(
    expr: &FtsQueryExpr,
    phrases: &[QueryPhrase],
    cfg: &Fts5Config,
    merged_index: &MergedIndexMap,
    col_mask: Option<&BTreeSet<usize>>,
) -> EvalDocSet {
    match expr {
        FtsQueryExpr::Phrase(pidx) => {
            let mut docs = eval_phrase_against_index(*pidx, &phrases[*pidx], cfg, merged_index);
            if let Some(allowed) = col_mask {
                docs.retain(|_, insts| {
                    insts.retain(|inst| allowed.contains(&inst.col_idx));
                    !insts.is_empty()
                });
            }
            docs
        }
        FtsQueryExpr::Near {
            phrase_indices,
            distance,
        } => {
            let mut per_phrase: Vec<EvalDocSet> = phrase_indices
                .iter()
                .map(|&pi| eval_phrase_against_index(pi, &phrases[pi], cfg, merged_index))
                .collect();
            if per_phrase.iter().any(BTreeMap::is_empty) {
                return BTreeMap::new();
            }
            let first = per_phrase.remove(0);
            let mut out = EvalDocSet::new();
            for (rowid, first_insts) in first {
                if !per_phrase.iter().all(|m| m.contains_key(&rowid)) {
                    continue;
                }
                let mut combined_valid = Vec::new();
                for c in 0..cfg.columns.len() {
                    if let Some(allowed) = col_mask
                        && !allowed.contains(&c)
                    {
                        continue;
                    }
                    let mut groups: Vec<Vec<(PhraseInstance, usize)>> =
                        Vec::with_capacity(1 + per_phrase.len());
                    let g0: Vec<(PhraseInstance, usize)> = first_insts
                        .iter()
                        .filter(|inst| inst.col_idx == c)
                        .cloned()
                        .map(|inst| {
                            let plen = phrases[inst.phrase_idx].tokens.len().max(1);
                            (inst, plen)
                        })
                        .collect();
                    if g0.is_empty() {
                        continue;
                    }
                    groups.push(g0);
                    let mut missing_col = false;
                    for pm in &per_phrase {
                        let gk: Vec<(PhraseInstance, usize)> = pm[&rowid]
                            .iter()
                            .filter(|inst| inst.col_idx == c)
                            .cloned()
                            .map(|inst| {
                                let plen = phrases[inst.phrase_idx].tokens.len().max(1);
                                (inst, plen)
                            })
                            .collect();
                        if gk.is_empty() {
                            missing_col = true;
                            break;
                        }
                        groups.push(gk);
                    }
                    if missing_col {
                        continue;
                    }
                    collect_near_matches(&groups, *distance, &mut combined_valid);
                }
                if !combined_valid.is_empty() {
                    dedup_instances(&mut combined_valid);
                    out.insert(rowid, combined_valid);
                }
            }
            out
        }
        FtsQueryExpr::ColFilter { filter, inner } => {
            let mut next_allowed: BTreeSet<usize> = match col_mask {
                Some(prev) => prev.clone(),
                None => (0..cfg.columns.len()).collect(),
            };
            match filter {
                ColFilterSpec::Include(inc) => {
                    next_allowed.retain(|c| inc.contains(c));
                }
                ColFilterSpec::Exclude(exc) => {
                    next_allowed.retain(|c| !exc.contains(c));
                }
            }
            eval_query_expr(inner, phrases, cfg, merged_index, Some(&next_allowed))
        }
        FtsQueryExpr::And(lhs, rhs) => {
            let l = eval_query_expr(lhs, phrases, cfg, merged_index, col_mask);
            if l.is_empty() {
                return BTreeMap::new();
            }
            let r = eval_query_expr(rhs, phrases, cfg, merged_index, col_mask);
            let mut out = EvalDocSet::new();
            for (rowid, mut l_insts) in l {
                if let Some(r_insts) = r.get(&rowid) {
                    l_insts.extend(r_insts.iter().cloned());
                    dedup_instances(&mut l_insts);
                    out.insert(rowid, l_insts);
                }
            }
            out
        }
        FtsQueryExpr::Or(lhs, rhs) => {
            let mut l = eval_query_expr(lhs, phrases, cfg, merged_index, col_mask);
            let r = eval_query_expr(rhs, phrases, cfg, merged_index, col_mask);
            for (rowid, r_insts) in r {
                let entry = l.entry(rowid).or_default();
                entry.extend(r_insts);
                dedup_instances(entry);
            }
            l
        }
        FtsQueryExpr::Not(lhs, rhs) => {
            let mut l = eval_query_expr(lhs, phrases, cfg, merged_index, col_mask);
            if l.is_empty() {
                return l;
            }
            let r = eval_query_expr(rhs, phrases, cfg, merged_index, col_mask);
            for rowid in r.keys() {
                l.remove(rowid);
            }
            l
        }
    }
}

fn collect_near_matches(
    groups: &[Vec<(PhraseInstance, usize)>],
    distance: usize,
    out: &mut Vec<PhraseInstance>,
) {
    fn dfs(
        idx: usize,
        groups: &[Vec<(PhraseInstance, usize)>],
        chosen: &mut Vec<(PhraseInstance, usize)>,
        distance: usize,
        out: &mut Vec<PhraseInstance>,
    ) {
        if idx == groups.len() {
            // In FTS5 NEAR(p1 p2 ..., N), sort by start offset and verify total intervening tokens
            // `(max_start - min_end) <= distance` (and non-overlapping or within span).
            let mut intervals: Vec<(usize, usize)> = chosen
                .iter()
                .map(|(inst, plen)| (inst.token_offset, inst.token_offset + *plen))
                .collect();
            intervals.sort_by_key(|&(s, e)| (s, e));
            let min_end = intervals[0].1;
            let max_start = intervals[intervals.len() - 1].0;
            let gap = max_start.saturating_sub(min_end);
            if gap <= distance {
                for (inst, _) in chosen.iter() {
                    out.push(inst.clone());
                }
            }
            return;
        }
        for cand in &groups[idx] {
            chosen.push(cand.clone());
            dfs(idx + 1, groups, chosen, distance, out);
            chosen.pop();
        }
    }
    let mut chosen = Vec::with_capacity(groups.len());
    dfs(0, groups, &mut chosen, distance, out);
}

fn dedup_instances(insts: &mut Vec<PhraseInstance>) {
    insts.sort_by_key(|x| (x.col_idx, x.token_offset, x.phrase_idx));
    insts.dedup_by(|a, b| {
        a.phrase_idx == b.phrase_idx && a.col_idx == b.col_idx && a.token_offset == b.token_offset
    });
}

// ============================================================================
// `Fts5Module` & `Fts5Table`
// ============================================================================

/// Built-in `fts5` full-text search virtual table module.
#[derive(Debug, Clone)]
pub struct Fts5Module {
    aux_registry: Arc<Fts5AuxRegistry>,
}

impl Default for Fts5Module {
    fn default() -> Self {
        Self::new(Arc::new(Fts5AuxRegistry::new()))
    }
}

impl Fts5Module {
    pub fn new(aux_registry: Arc<Fts5AuxRegistry>) -> Self {
        Self { aux_registry }
    }
}

impl VirtualTableModule for Fts5Module {
    fn name(&self) -> &str {
        "fts5"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn shadow_table_suffixes(&self) -> &[&str] {
        SHADOW_SUFFIXES
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        let tbl_name = ctx.table_name().to_owned();
        let db_name = ctx.db_name().to_owned();
        let cfg = Fts5Config::parse(&tbl_name, args)?;

        let db_q = quote_ident(&db_name);
        let data_sq = quote_single(&format!("{tbl_name}_data"));
        let idx_sq = quote_single(&format!("{tbl_name}_idx"));
        let config_sq = quote_single(&format!("{tbl_name}_config"));

        ctx.execute_sql(
            &format!("CREATE TABLE {db_q}.{data_sq}(id INTEGER PRIMARY KEY, block BLOB)"),
            &[],
        )?;
        ctx.execute_sql(
            &format!(
                "CREATE TABLE {db_q}.{idx_sq}(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID"
            ),
            &[],
        )?;

        if cfg.content_mode == ContentMode::Normal {
            let content_sq = quote_single(&format!("{tbl_name}_content"));
            let mut cdefs = vec!["id INTEGER PRIMARY KEY".to_owned()];
            for i in 0..cfg.columns.len() {
                cdefs.push(format!("c{i}"));
            }
            ctx.execute_sql(
                &format!("CREATE TABLE {db_q}.{content_sq}({})", cdefs.join(", ")),
                &[],
            )?;
        }

        if cfg.columnsize {
            let docsize_sq = quote_single(&format!("{tbl_name}_docsize"));
            let docsize_cols = if cfg.contentless_delete {
                "id INTEGER PRIMARY KEY, sz BLOB, origin INTEGER"
            } else {
                "id INTEGER PRIMARY KEY, sz BLOB"
            };
            ctx.execute_sql(
                &format!("CREATE TABLE {db_q}.{docsize_sq}({docsize_cols})"),
                &[],
            )?;
        }

        ctx.execute_sql(
            &format!("CREATE TABLE {db_q}.{config_sq}(k PRIMARY KEY, v) WITHOUT ROWID"),
            &[],
        )?;
        let config_iq = quote_ident(&format!("{tbl_name}_config"));
        ctx.execute_sql(
            &format!("INSERT INTO {db_q}.{config_iq}(k, v) VALUES('version', 4)"),
            &[],
        )?;

        // Initialize `_data` averages (id=1) and structure record (id=10)
        let data_iq = quote_ident(&format!("{tbl_name}_data"));
        ctx.execute_sql(
            &format!("INSERT INTO {db_q}.{data_iq}(id, block) VALUES(?1, ?2)"),
            &[Value::Integer(FTS5_AVERAGES_ROWID), Value::Blob(Vec::new())],
        )?;
        ctx.execute_sql(
            &format!("INSERT INTO {db_q}.{data_iq}(id, block) VALUES(?1, ?2)"),
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(encode_empty_structure_record(cfg.contentless_delete)),
            ],
        )?;

        ctx.declare_vtab(&cfg.declare_sql(&tbl_name))?;
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);

        Ok(Box::new(Fts5Table {
            table_name: tbl_name,
            cfg,
            aux_registry: Arc::clone(&self.aux_registry),
        }))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        let tbl_name = ctx.table_name().to_owned();
        let cfg = Fts5Config::parse(&tbl_name, args)?;
        ctx.declare_vtab(&cfg.declare_sql(&tbl_name))?;
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
        Ok(Box::new(Fts5Table {
            table_name: tbl_name,
            cfg,
            aux_registry: Arc::clone(&self.aux_registry),
        }))
    }
}

#[derive(Debug)]
struct Fts5Table {
    table_name: String,
    cfg: Fts5Config,
    aux_registry: Arc<Fts5AuxRegistry>,
}

const IDX_FULLSCAN: i32 = 0;
const IDX_ROWID_EQ: i32 = 1;
const IDX_MATCH: i32 = 2;
const IDX_LIKE: i32 = 3;
const IDX_GLOB: i32 = 4;

const IDX_FLAG_HAS_RANK_CFG: i32 = 0x100;
const IDX_FLAG_ORDER_RANK_ASC: i32 = 0x200;
const IDX_FLAG_ORDER_RANK_DESC: i32 = 0x400;
const IDX_FLAG_ORDER_ROWID_ASC: i32 = 0x800;
const IDX_FLAG_ORDER_ROWID_DESC: i32 = 0x1000;

impl Fts5Table {
    fn hidden_tbl_col(&self) -> i32 {
        self.cfg.columns.len() as i32
    }

    fn hidden_rank_col(&self) -> i32 {
        (self.cfg.columns.len() + 1) as i32
    }

    fn append_segment(
        &self,
        ctx: &mut dyn VTabHostContext,
        seg_terms: &BTreeMap<Vec<u8>, Vec<DocPosting>>,
    ) -> Result<()> {
        if seg_terms.is_empty() {
            return Ok(());
        }
        let db_q = quote_ident(ctx.db_name());
        let mut merged = load_merged_index(ctx, &db_q, &self.table_name, self.cfg.detail)?;
        for (term, dps) in seg_terms {
            let entry = merged.entry(term.clone()).or_default();
            for dp in dps {
                if dp.deleted {
                    entry.remove(&dp.rowid);
                } else {
                    entry.insert(dp.rowid, dp.col_hits.clone());
                }
            }
        }
        merged.retain(|_, docs| !docs.is_empty());
        write_c_fts5_index(ctx, &db_q, &self.table_name, &self.cfg, &merged)
    }

    fn read_averages(&self, ctx: &mut dyn VTabHostContext) -> Result<Vec<u64>> {
        let db_q = quote_ident(ctx.db_name());
        let data_q = quote_ident(&format!("{}_data", self.table_name));
        let rows = ctx.query_sql(
            &format!("SELECT block FROM {db_q}.{data_q} WHERE id = {FTS5_AVERAGES_ROWID}"),
            &[],
        )?;
        let n_expected = 1 + self.cfg.columns.len();
        if let Some(Value::Blob(b)) = rows.first().and_then(|r| r.first())
            && let Some(mut list) = decode_varint_list(b)
        {
            list.resize(n_expected, 0);
            return Ok(list);
        }
        Ok(vec![0u64; n_expected])
    }

    fn write_averages(&self, ctx: &mut dyn VTabHostContext, avgs: &[u64]) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let data_q = quote_ident(&format!("{}_data", self.table_name));
        ctx.execute_sql(
            &format!("INSERT OR REPLACE INTO {db_q}.{data_q}(id, block) VALUES(?1, ?2)"),
            &[
                Value::Integer(FTS5_AVERAGES_ROWID),
                Value::Blob(encode_varint_list(avgs)),
            ],
        )?;
        Ok(())
    }

    fn read_content_row(
        &self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<Vec<Value>>> {
        let db_q = quote_ident(ctx.db_name());
        match &self.cfg.content_mode {
            ContentMode::Contentless => Ok(Some(vec![Value::Null; self.cfg.columns.len()])),
            ContentMode::Normal => {
                let content_q = quote_ident(&format!("{}_content", self.table_name));
                let cols: Vec<String> = (0..self.cfg.columns.len())
                    .map(|i| format!("c{i}"))
                    .collect();
                let sql = format!(
                    "SELECT {} FROM {db_q}.{content_q} WHERE id = ?1",
                    cols.join(", ")
                );
                let mut rows = ctx.query_sql(&sql, &[Value::Integer(rowid)])?;
                Ok(rows.pop())
            }
            ContentMode::External { table, rowid_col } => {
                let ext_q = quote_ident(table);
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let cols: Vec<String> = self.cfg.columns.iter().map(|c| quote_ident(c)).collect();
                let sql = format!(
                    "SELECT {} FROM {db_q}.{ext_q} WHERE {rid_q} = ?1",
                    cols.join(", ")
                );
                let mut rows = ctx.query_sql(&sql, &[Value::Integer(rowid)])?;
                Ok(rows.pop())
            }
        }
    }

    fn insert_document(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        explicit_rowid: Option<i64>,
        col_vals: &[Value],
    ) -> Result<i64> {
        let db_q = quote_ident(ctx.db_name());
        let rowid = match &self.cfg.content_mode {
            ContentMode::Normal => {
                let content_q = quote_ident(&format!("{}_content", self.table_name));
                if let Some(rid) = explicit_rowid {
                    let existing = ctx.query_sql(
                        &format!("SELECT 1 FROM {db_q}.{content_q} WHERE id = ?1"),
                        &[Value::Integer(rid)],
                    )?;
                    if !existing.is_empty() {
                        return Err(SqliteError::new(ErrorCode::Constraint, "constraint failed"));
                    }
                    let mut cols = vec!["id".to_owned()];
                    let mut phs = vec!["?1".to_owned()];
                    let mut params = vec![Value::Integer(rid)];
                    for (i, v) in col_vals.iter().enumerate().take(self.cfg.columns.len()) {
                        cols.push(format!("c{i}"));
                        phs.push(format!("?{}", i + 2));
                        params.push(v.clone());
                    }
                    ctx.execute_sql(
                        &format!(
                            "INSERT INTO {db_q}.{content_q}({}) VALUES({})",
                            cols.join(", "),
                            phs.join(", ")
                        ),
                        &params,
                    )?;
                    rid
                } else {
                    let max_r = ctx.query_sql(
                        &format!("SELECT coalesce(max(id), 0) + 1 FROM {db_q}.{content_q}"),
                        &[],
                    )?;
                    let rid = max_r
                        .first()
                        .and_then(|r| r.first())
                        .map_or(1, Value::to_integer_lossy);
                    let mut cols = vec!["id".to_owned()];
                    let mut phs = vec!["?1".to_owned()];
                    let mut params = vec![Value::Integer(rid)];
                    for (i, v) in col_vals.iter().enumerate().take(self.cfg.columns.len()) {
                        cols.push(format!("c{i}"));
                        phs.push(format!("?{}", i + 2));
                        params.push(v.clone());
                    }
                    ctx.execute_sql(
                        &format!(
                            "INSERT INTO {db_q}.{content_q}({}) VALUES({})",
                            cols.join(", "),
                            phs.join(", ")
                        ),
                        &params,
                    )?;
                    rid
                }
            }
            ContentMode::Contentless | ContentMode::External { .. } => {
                if let Some(rid) = explicit_rowid {
                    rid
                } else if self.cfg.columnsize {
                    let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
                    let max_r = ctx.query_sql(
                        &format!("SELECT coalesce(max(id), 0) + 1 FROM {db_q}.{docsize_q}"),
                        &[],
                    )?;
                    max_r
                        .first()
                        .and_then(|r| r.first())
                        .map_or(1, Value::to_integer_lossy)
                } else {
                    let avgs = self.read_averages(ctx)?;
                    (avgs[0] as i64) + 1
                }
            }
        };

        let (seg_terms, col_sizes) = build_doc_term_postings(&self.cfg, rowid, col_vals, false);
        self.append_segment(ctx, &seg_terms)?;

        if self.cfg.columnsize {
            let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
            if self.cfg.contentless_delete {
                ctx.execute_sql(
                    &format!(
                        "INSERT OR REPLACE INTO {db_q}.{docsize_q}(id, sz, origin) VALUES(?1, ?2, 1)"
                    ),
                    &[
                        Value::Integer(rowid),
                        Value::Blob(encode_varint_list(&col_sizes)),
                    ],
                )?;
            } else {
                ctx.execute_sql(
                    &format!("INSERT OR REPLACE INTO {db_q}.{docsize_q}(id, sz) VALUES(?1, ?2)"),
                    &[
                        Value::Integer(rowid),
                        Value::Blob(encode_varint_list(&col_sizes)),
                    ],
                )?;
            }
        }

        let mut avgs = self.read_averages(ctx)?;
        avgs[0] = avgs[0].saturating_add(1);
        for (i, &sz) in col_sizes.iter().enumerate() {
            avgs[1 + i] = avgs[1 + i].saturating_add(sz);
        }
        self.write_averages(ctx, &avgs)?;
        Ok(rowid)
    }

    fn delete_document_with_values(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        old_vals: &[Value],
        remove_content_row: bool,
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let (seg_terms, col_sizes) = build_doc_term_postings(&self.cfg, rowid, old_vals, true);
        self.append_segment(ctx, &seg_terms)?;

        if remove_content_row && self.cfg.content_mode == ContentMode::Normal {
            let content_q = quote_ident(&format!("{}_content", self.table_name));
            ctx.execute_sql(
                &format!("DELETE FROM {db_q}.{content_q} WHERE id = ?1"),
                &[Value::Integer(rowid)],
            )?;
        }

        if self.cfg.columnsize {
            let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
            ctx.execute_sql(
                &format!("DELETE FROM {db_q}.{docsize_q} WHERE id = ?1"),
                &[Value::Integer(rowid)],
            )?;
        }

        let mut avgs = self.read_averages(ctx)?;
        avgs[0] = avgs[0].saturating_sub(1);
        for (i, &sz) in col_sizes.iter().enumerate() {
            avgs[1 + i] = avgs[1 + i].saturating_sub(sz);
        }
        self.write_averages(ctx, &avgs)?;
        Ok(())
    }

    fn delete_contentless_rowid(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let mut merged = load_merged_index(ctx, &db_q, &self.table_name, self.cfg.detail)?;
        let mut removed_from_index = false;
        for docs in merged.values_mut() {
            if docs.remove(&rowid).is_some() {
                removed_from_index = true;
            }
        }
        merged.retain(|_, docs| !docs.is_empty());
        let mut existed = removed_from_index;
        if removed_from_index {
            write_c_fts5_index(ctx, &db_q, &self.table_name, &self.cfg, &merged)?;
        }

        let mut old_col_sizes = vec![0u64; self.cfg.columns.len()];
        if self.cfg.columnsize {
            let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
            let rows = ctx.query_sql(
                &format!("SELECT sz FROM {db_q}.{docsize_q} WHERE id = ?1"),
                &[Value::Integer(rowid)],
            )?;
            if let Some(Value::Blob(b)) = rows.first().and_then(|r| r.first()) {
                existed = true;
                if let Some(list) = decode_varint_list(b) {
                    for (i, sz) in list.into_iter().enumerate().take(old_col_sizes.len()) {
                        old_col_sizes[i] = sz;
                    }
                }
            }
            ctx.execute_sql(
                &format!("DELETE FROM {db_q}.{docsize_q} WHERE id = ?1"),
                &[Value::Integer(rowid)],
            )?;
        }

        if existed {
            let mut avgs = self.read_averages(ctx)?;
            avgs[0] = avgs[0].saturating_sub(1);
            for (i, &sz) in old_col_sizes.iter().enumerate() {
                avgs[1 + i] = avgs[1 + i].saturating_sub(sz);
            }
            self.write_averages(ctx, &avgs)?;
        }
        Ok(())
    }

    fn execute_special_command(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        cmd: &str,
        new_rowid: Option<Value>,
        values: &[Value],
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        match cmd {
            "delete" => {
                if self.cfg.content_mode == ContentMode::Normal {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "'delete' may not be used with a normal content table",
                    ));
                }
                let Some(rid_val) = new_rowid else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "fts5: 'delete' requires integer rowid",
                    ));
                };
                let Value::Integer(rid) = rid_val else {
                    return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                };
                let old_cols = &values[..self.cfg.columns.len()];
                self.delete_document_with_values(ctx, rid, old_cols, false)?;
                Ok(())
            }
            "delete-all" => {
                if self.cfg.content_mode == ContentMode::Normal {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "'delete-all' may not be used with a normal content table",
                    ));
                }
                self.reset_index_storage(ctx)?;
                Ok(())
            }
            "rebuild" => {
                if self.cfg.content_mode == ContentMode::Contentless {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "'rebuild' may not be used with a contentless fts5 table",
                    ));
                }
                self.rebuild_index(ctx)
            }
            "optimize" | "merge" => self.compact_segments(ctx),
            "integrity-check" => {
                let rank_arg = values.get(self.cfg.columns.len() + 1);
                let check_external = match rank_arg {
                    None | Some(Value::Null) => true,
                    Some(Value::Integer(0)) => false,
                    Some(Value::Integer(1)) => true,
                    _ => {
                        return Err(SqliteError::new(ErrorCode::Error, "fts5: syntax error"));
                    }
                };
                self.verify_integrity(ctx, check_external)
            }
            "rank" => {
                let rank_val = values
                    .get(self.cfg.columns.len() + 1)
                    .cloned()
                    .unwrap_or(Value::Null);
                let rank_str = rank_val.to_text_lossy();
                let _ = parse_rank_expression(&rank_str)?;
                let config_q = quote_ident(&format!("{}_config", self.table_name));
                ctx.execute_sql(
                    &format!("INSERT OR REPLACE INTO {db_q}.{config_q}(k, v) VALUES('rank', ?1)"),
                    &[Value::Text(rank_str)],
                )?;
                Ok(())
            }
            "pgsz" | "automerge" | "crisismerge" | "usermerge" | "secure-delete" => {
                let rank_val = values
                    .get(self.cfg.columns.len() + 1)
                    .cloned()
                    .unwrap_or(Value::Integer(0));
                let config_q = quote_ident(&format!("{}_config", self.table_name));
                ctx.execute_sql(
                    &format!("INSERT OR REPLACE INTO {db_q}.{config_q}(k, v) VALUES(?1, ?2)"),
                    &[Value::Text(cmd.to_owned()), rank_val],
                )?;
                Ok(())
            }
            other => Err(SqliteError::new(
                ErrorCode::Error,
                format!("fts5: syntax error near \"{other}\""),
            )),
        }
    }

    fn reset_index_storage(&self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let data_q = quote_ident(&format!("{}_data", self.table_name));
        let idx_q = quote_ident(&format!("{}_idx", self.table_name));
        ctx.execute_sql(&format!("DELETE FROM {db_q}.{data_q}"), &[])?;
        ctx.execute_sql(&format!("DELETE FROM {db_q}.{idx_q}"), &[])?;
        if self.cfg.columnsize {
            let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
            ctx.execute_sql(&format!("DELETE FROM {db_q}.{docsize_q}"), &[])?;
        }
        ctx.execute_sql(
            &format!("INSERT INTO {db_q}.{data_q}(id, block) VALUES(?1, ?2)"),
            &[Value::Integer(FTS5_AVERAGES_ROWID), Value::Blob(Vec::new())],
        )?;
        ctx.execute_sql(
            &format!("INSERT INTO {db_q}.{data_q}(id, block) VALUES(?1, ?2)"),
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(encode_empty_structure_record(self.cfg.contentless_delete)),
            ],
        )?;
        Ok(())
    }

    fn rebuild_index(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let src_rows = match &self.cfg.content_mode {
            ContentMode::Normal => {
                let content_q = quote_ident(&format!("{}_content", self.table_name));
                let mut cols = vec!["id".to_owned()];
                for i in 0..self.cfg.columns.len() {
                    cols.push(format!("c{i}"));
                }
                ctx.query_sql(
                    &format!(
                        "SELECT {} FROM {db_q}.{content_q} ORDER BY id ASC",
                        cols.join(", ")
                    ),
                    &[],
                )?
            }
            ContentMode::External { table, rowid_col } => {
                let ext_q = quote_ident(table);
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let mut cols = vec![rid_q.clone()];
                for c in &self.cfg.columns {
                    cols.push(quote_ident(c));
                }
                ctx.query_sql(
                    &format!(
                        "SELECT {} FROM {db_q}.{ext_q} ORDER BY {rid_q} ASC",
                        cols.join(", ")
                    ),
                    &[],
                )?
            }
            ContentMode::Contentless => Vec::new(),
        };

        self.reset_index_storage(ctx)?;
        let mut combined_terms: BTreeMap<Vec<u8>, Vec<DocPosting>> = BTreeMap::new();
        let mut avgs = vec![0u64; 1 + self.cfg.columns.len()];

        for row in src_rows {
            let rid = row.first().map_or(0, Value::to_integer_lossy);
            let cvals = &row[1..];
            let (doc_terms, col_sizes) = build_doc_term_postings(&self.cfg, rid, cvals, false);
            for (k, mut v) in doc_terms {
                combined_terms.entry(k).or_default().append(&mut v);
            }
            if self.cfg.columnsize {
                let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
                if self.cfg.contentless_delete {
                    ctx.execute_sql(
                        &format!(
                            "INSERT INTO {db_q}.{docsize_q}(id, sz, origin) VALUES(?1, ?2, 1)"
                        ),
                        &[
                            Value::Integer(rid),
                            Value::Blob(encode_varint_list(&col_sizes)),
                        ],
                    )?;
                } else {
                    ctx.execute_sql(
                        &format!("INSERT INTO {db_q}.{docsize_q}(id, sz) VALUES(?1, ?2)"),
                        &[
                            Value::Integer(rid),
                            Value::Blob(encode_varint_list(&col_sizes)),
                        ],
                    )?;
                }
            }
            avgs[0] += 1;
            for (i, &sz) in col_sizes.iter().enumerate() {
                avgs[1 + i] += sz;
            }
        }

        self.append_segment(ctx, &combined_terms)?;
        self.write_averages(ctx, &avgs)?;
        Ok(())
    }

    fn compact_segments(&self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let merged = load_merged_index(ctx, &db_q, &self.table_name, self.cfg.detail)?;
        write_c_fts5_index(ctx, &db_q, &self.table_name, &self.cfg, &merged)
    }

    fn verify_integrity(&self, ctx: &mut dyn VTabHostContext, check_external: bool) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let actual = load_merged_index(ctx, &db_q, &self.table_name, self.cfg.detail)?;

        let should_verify_content = match self.cfg.content_mode {
            ContentMode::Normal => true,
            ContentMode::External { .. } => check_external,
            ContentMode::Contentless => false,
        };
        if !should_verify_content {
            return Ok(());
        }

        let src_rows = match &self.cfg.content_mode {
            ContentMode::Normal => {
                let content_q = quote_ident(&format!("{}_content", self.table_name));
                let mut cols = vec!["id".to_owned()];
                for i in 0..self.cfg.columns.len() {
                    cols.push(format!("c{i}"));
                }
                ctx.query_sql(
                    &format!(
                        "SELECT {} FROM {db_q}.{content_q} ORDER BY id ASC",
                        cols.join(", ")
                    ),
                    &[],
                )?
            }
            ContentMode::External { table, rowid_col } => {
                let ext_q = quote_ident(table);
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let mut cols = vec![rid_q.clone()];
                for c in &self.cfg.columns {
                    cols.push(quote_ident(c));
                }
                ctx.query_sql(
                    &format!(
                        "SELECT {} FROM {db_q}.{ext_q} ORDER BY {rid_q} ASC",
                        cols.join(", ")
                    ),
                    &[],
                )?
            }
            ContentMode::Contentless => unreachable!(),
        };

        let mut expected: MergedIndexMap = BTreeMap::new();
        for row in src_rows {
            let rid = row.first().map_or(0, Value::to_integer_lossy);
            let cvals = &row[1..];
            let (doc_terms, _) = build_doc_term_postings(&self.cfg, rid, cvals, false);
            for (term, dps) in doc_terms {
                for dp in dps {
                    expected
                        .entry(term.clone())
                        .or_default()
                        .insert(dp.rowid, dp.col_hits);
                }
            }
        }
        if actual != expected {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!("fts5: checksum mismatch for table \"{}\"", self.table_name),
            ));
        }
        Ok(())
    }
}

fn parse_rank_expression(s: &str) -> Result<Vec<f64>> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let Some(open_p) = trimmed.find('(') else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("fts5: syntax error near \"{trimmed}\""),
        ));
    };
    if !trimmed.ends_with(')') {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("fts5: syntax error near \"{trimmed}\""),
        ));
    }
    let fn_name = trimmed[..open_p].trim();
    if !fn_name.eq_ignore_ascii_case("bm25") {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such function: {fn_name}"),
        ));
    }
    let inner = trimmed[open_p + 1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let mut weights = Vec::new();
    for part in inner.split(',') {
        let w = part.trim().parse::<f64>().map_err(|_| {
            SqliteError::new(
                ErrorCode::Error,
                format!("fts5: error parsing rank function: {trimmed}"),
            )
        })?;
        weights.push(w);
    }
    Ok(weights)
}

impl VirtualTable for Fts5Table {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let tbl_col = self.hidden_tbl_col();
        let rank_col = self.hidden_rank_col();

        let mut match_cidx: Option<(usize, i32, VTabConstraintOp)> = None;
        let mut rank_cidx: Option<usize> = None;
        let mut rowid_eq_cidx: Option<usize> = None;

        for (i, c) in info.constraints.iter().enumerate() {
            if !c.usable {
                if c.op == VTabConstraintOp::Match
                    || (c.i_column == tbl_col && c.op == VTabConstraintOp::Eq)
                {
                    return Err(SqliteError::new(
                        ErrorCode::Constraint,
                        "unusable fts5 match constraint",
                    ));
                }
                continue;
            }
            if c.op == VTabConstraintOp::Match
                || (c.i_column == tbl_col && c.op == VTabConstraintOp::Eq)
            {
                if match_cidx.is_none() {
                    match_cidx = Some((i, c.i_column, VTabConstraintOp::Match));
                }
            } else if c.i_column == rank_col
                && matches!(c.op, VTabConstraintOp::Eq | VTabConstraintOp::Match)
            {
                rank_cidx = Some(i);
            } else if c.i_column == -1 && c.op == VTabConstraintOp::Eq {
                rowid_eq_cidx = Some(i);
            } else if matches!(c.op, VTabConstraintOp::Like | VTabConstraintOp::Glob)
                && c.i_column >= 0
                && c.i_column < tbl_col
                && let Some((cs, _)) = self.cfg.tokenizer.trigram_params()
            {
                let ok = (c.op == VTabConstraintOp::Like && !cs)
                    || (c.op == VTabConstraintOp::Glob && cs);
                if ok && match_cidx.is_none() {
                    match_cidx = Some((i, c.i_column, c.op));
                }
            }
        }

        let mut argv_pos = 0usize;
        let mut idx_num = IDX_FULLSCAN;
        let mut idx_str = None;

        if let Some((mi, mcol, mop)) = match_cidx {
            argv_pos += 1;
            info.constraint_usage[mi].argv_index = argv_pos;
            // Omit `MATCH` / `ft = 'query'`, keep outer post-filter on `LIKE`/`GLOB`
            info.constraint_usage[mi].omit = mop == VTabConstraintOp::Match;
            idx_num = match mop {
                VTabConstraintOp::Match => IDX_MATCH,
                VTabConstraintOp::Like => IDX_LIKE,
                VTabConstraintOp::Glob => IDX_GLOB,
                _ => IDX_MATCH,
            };
            if mcol >= 0 && mcol < tbl_col {
                idx_str = Some(self.cfg.columns[mcol as usize].clone());
            }
            if let Some(ri) = rank_cidx {
                argv_pos += 1;
                info.constraint_usage[ri].argv_index = argv_pos;
                info.constraint_usage[ri].omit = true;
                idx_num |= IDX_FLAG_HAS_RANK_CFG;
            }
            info.estimated_cost = 10.0;
            info.estimated_rows = 10;
        } else if let Some(rqi) = rowid_eq_cidx {
            argv_pos += 1;
            info.constraint_usage[rqi].argv_index = argv_pos;
            info.constraint_usage[rqi].omit = true;
            idx_num = IDX_ROWID_EQ;
            info.estimated_cost = 1.0;
            info.estimated_rows = 1;
        } else {
            info.estimated_cost = 10_000.0;
            info.estimated_rows = 1_000;
        }

        if info.order_by.len() == 1 {
            let ob = &info.order_by[0];
            if ob.i_column == rank_col && (idx_num & 0xff) == IDX_MATCH {
                idx_num |= if ob.desc {
                    IDX_FLAG_ORDER_RANK_DESC
                } else {
                    IDX_FLAG_ORDER_RANK_ASC
                };
                info.order_by_consumed = true;
            } else if ob.i_column == -1 {
                idx_num |= if ob.desc {
                    IDX_FLAG_ORDER_ROWID_DESC
                } else {
                    IDX_FLAG_ORDER_ROWID_ASC
                };
                info.order_by_consumed = true;
            }
        }

        info.idx_num = idx_num;
        info.idx_str = idx_str;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(Fts5Cursor {
            table_name: self.table_name.clone(),
            cfg: self.cfg.clone(),
            aux_registry: Arc::clone(&self.aux_registry),
            rows: Vec::new(),
            pos: 0,
        }))
    }

    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let n_cols = self.cfg.columns.len();
        match op {
            VTabUpdateOperation::Insert { new_rowid, values } => {
                // Check special command: `INSERT INTO ft(ft, ...) VALUES('cmd', ...)`
                if let Some(Value::Text(cmd)) = values.get(n_cols) {
                    self.execute_special_command(ctx, cmd, new_rowid, &values)?;
                    return Ok(None);
                }
                let explicit_rid = match new_rowid {
                    None | Some(Value::Null) => None,
                    Some(Value::Integer(i)) => Some(i),
                    Some(_) => {
                        return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                    }
                };
                let rid = self.insert_document(ctx, explicit_rid, &values[..n_cols])?;
                Ok(Some(rid))
            }
            VTabUpdateOperation::Delete { old_rowid } => {
                let Value::Integer(rid) = old_rowid else {
                    return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                };
                if self.cfg.content_mode == ContentMode::Contentless {
                    if !self.cfg.contentless_delete {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "cannot DELETE from contentless fts5 table",
                        ));
                    }
                    self.delete_contentless_rowid(ctx, rid)?;
                } else if let Some(old_vals) = self.read_content_row(ctx, rid)? {
                    self.delete_document_with_values(ctx, rid, &old_vals, true)?;
                }
                Ok(None)
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                if self.cfg.content_mode == ContentMode::Contentless && !self.cfg.contentless_delete
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "cannot UPDATE contentless fts5 table",
                    ));
                }
                let Value::Integer(old_rid) = old_rowid else {
                    return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                };
                let new_rid = match new_rowid {
                    Value::Null => old_rid,
                    Value::Integer(i) => i,
                    _ => return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch")),
                };
                if self.cfg.content_mode == ContentMode::Contentless {
                    self.delete_contentless_rowid(ctx, old_rid)?;
                } else if let Some(old_vals) = self.read_content_row(ctx, old_rid)? {
                    self.delete_document_with_values(ctx, old_rid, &old_vals, true)?;
                }
                let inserted_rid = self.insert_document(ctx, Some(new_rid), &values[..n_cols])?;
                Ok(Some(inserted_rid))
            }
        }
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let old_name = self.table_name.clone();
        let mut suffixes = vec!["data", "idx", "config"];
        if self.cfg.content_mode == ContentMode::Normal {
            suffixes.push("content");
        }
        if self.cfg.columnsize {
            suffixes.push("docsize");
        }
        for suf in suffixes {
            let old_s = quote_ident(&format!("{old_name}_{suf}"));
            let new_s = quote_ident(&format!("{new_name}_{suf}"));
            ctx.execute_sql(
                &format!("ALTER TABLE {db_q}.{old_s} RENAME TO {new_s}"),
                &[],
            )?;
        }
        self.table_name = new_name.to_owned();
        Ok(())
    }

    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        for suf in SHADOW_SUFFIXES {
            let sq = quote_ident(&format!("{}_{suf}", self.table_name));
            ctx.execute_sql(&format!("DROP TABLE IF EXISTS {db_q}.{sq}"), &[])?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Fts5CursorRow {
    rowid: i64,
    cols: Vec<Value>,
    aux_handle: Value,
    rank: Value,
}

#[derive(Debug)]
struct Fts5Cursor {
    table_name: String,
    cfg: Fts5Config,
    aux_registry: Arc<Fts5AuxRegistry>,
    rows: Vec<Fts5CursorRow>,
    pos: usize,
}

impl Fts5Cursor {
    fn load_col_sizes(
        &self,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        col_vals: &[Value],
    ) -> Result<Vec<usize>> {
        if self.cfg.columnsize {
            let db_q = quote_ident(ctx.db_name());
            let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
            let rows = ctx.query_sql(
                &format!("SELECT sz FROM {db_q}.{docsize_q} WHERE id = ?1"),
                &[Value::Integer(rowid)],
            )?;
            if let Some(Value::Blob(b)) = rows.first().and_then(|r| r.first())
                && let Some(list) = decode_varint_list(b)
            {
                let mut out: Vec<usize> = list.into_iter().map(|x| x as usize).collect();
                out.resize(self.cfg.columns.len(), 0);
                return Ok(out);
            }
        }
        let mut out = vec![0usize; self.cfg.columns.len()];
        for (i, v) in col_vals.iter().enumerate().take(self.cfg.columns.len()) {
            if !self.cfg.unindexed[i] && !v.is_null() {
                out[i] = self.cfg.tokenizer.tokenize(&v.to_text_lossy(), false).len();
            }
        }
        Ok(out)
    }

    fn load_content_vals(&self, ctx: &mut dyn VTabHostContext, rowid: i64) -> Result<Vec<Value>> {
        let db_q = quote_ident(ctx.db_name());
        match &self.cfg.content_mode {
            ContentMode::Contentless => Ok(vec![Value::Null; self.cfg.columns.len()]),
            ContentMode::Normal => {
                let content_q = quote_ident(&format!("{}_content", self.table_name));
                let cols: Vec<String> = (0..self.cfg.columns.len())
                    .map(|i| format!("c{i}"))
                    .collect();
                let mut r = ctx.query_sql(
                    &format!(
                        "SELECT {} FROM {db_q}.{content_q} WHERE id = ?1",
                        cols.join(", ")
                    ),
                    &[Value::Integer(rowid)],
                )?;
                Ok(r.pop()
                    .unwrap_or_else(|| vec![Value::Null; self.cfg.columns.len()]))
            }
            ContentMode::External { table, rowid_col } => {
                let ext_q = quote_ident(table);
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let cols: Vec<String> = self.cfg.columns.iter().map(|c| quote_ident(c)).collect();
                let mut r = ctx.query_sql(
                    &format!(
                        "SELECT {} FROM {db_q}.{ext_q} WHERE {rid_q} = ?1",
                        cols.join(", ")
                    ),
                    &[Value::Integer(rowid)],
                )?;
                Ok(r.pop()
                    .unwrap_or_else(|| vec![Value::Null; self.cfg.columns.len()]))
            }
        }
    }

    fn load_configured_rank_weights(&self, ctx: &mut dyn VTabHostContext) -> Result<Vec<f64>> {
        let db_q = quote_ident(ctx.db_name());
        let config_q = quote_ident(&format!("{}_config", self.table_name));
        let rows = ctx.query_sql(
            &format!("SELECT v FROM {db_q}.{config_q} WHERE k = 'rank'"),
            &[],
        )?;
        if let Some(Value::Text(s)) = rows.first().and_then(|r| r.first()) {
            return parse_rank_expression(s);
        }
        Ok(Vec::new())
    }
}

fn find_trigram_substring_spans(
    text: &str,
    needle_raw: &str,
    case_sensitive: bool,
    remove_diacritics: u8,
) -> Vec<(usize, usize)> {
    if needle_raw.is_empty() {
        return Vec::new();
    }
    let (h_chars, h_spans) = fold_trigram_text_with_map(text, case_sensitive, remove_diacritics);
    let (n_chars, _) = fold_trigram_text_with_map(needle_raw, case_sensitive, remove_diacritics);
    if n_chars.is_empty() || h_chars.len() < n_chars.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=(h_chars.len() - n_chars.len()) {
        if h_chars[i..i + n_chars.len()] == n_chars[..] {
            out.push((h_spans[i].0, h_spans[i + n_chars.len() - 1].1));
        }
    }
    out
}

fn extract_like_glob_runs(pat: &str, is_glob: bool) -> Vec<String> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut chars = pat.chars().peekable();
    while let Some(ch) = chars.next() {
        let is_wild = if is_glob {
            matches!(ch, '*' | '?' | '[')
        } else {
            matches!(ch, '%' | '_')
        };
        if is_wild {
            if is_glob && ch == '[' {
                for next_c in chars.by_ref() {
                    if next_c == ']' {
                        break;
                    }
                }
            }
            if cur.chars().count() >= 3 {
                runs.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        } else {
            cur.push(ch);
        }
    }
    if cur.chars().count() >= 3 {
        runs.push(cur);
    }
    runs
}

impl Fts5Cursor {
    fn read_averages(&self, ctx: &mut dyn VTabHostContext) -> Result<Vec<u64>> {
        let db_q = quote_ident(ctx.db_name());
        let data_q = quote_ident(&format!("{}_data", self.table_name));
        let rows = ctx.query_sql(
            &format!("SELECT block FROM {db_q}.{data_q} WHERE id = {FTS5_AVERAGES_ROWID}"),
            &[],
        )?;
        let n_expected = 1 + self.cfg.columns.len();
        if let Some(Value::Blob(b)) = rows.first().and_then(|r| r.first())
            && let Some(mut list) = decode_varint_list(b)
        {
            list.resize(n_expected, 0);
            return Ok(list);
        }
        Ok(vec![0u64; n_expected])
    }
}

impl VirtualCursor for Fts5Cursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        let db_q = quote_ident(ctx.db_name());
        let mode = idx_num & 0xff;
        let desc_rowid = (idx_num & IDX_FLAG_ORDER_ROWID_DESC) != 0;

        match mode {
            IDX_FULLSCAN => {
                let order_kw = if desc_rowid { "DESC" } else { "ASC" };
                match &self.cfg.content_mode {
                    ContentMode::Normal => {
                        let content_q = quote_ident(&format!("{}_content", self.table_name));
                        let mut cols = vec!["id".to_owned()];
                        for i in 0..self.cfg.columns.len() {
                            cols.push(format!("c{i}"));
                        }
                        let qrows = ctx.query_sql(
                            &format!(
                                "SELECT {} FROM {db_q}.{content_q} ORDER BY id {order_kw}",
                                cols.join(", ")
                            ),
                            &[],
                        )?;
                        for r in qrows {
                            let rid = r[0].to_integer_lossy();
                            self.rows.push(Fts5CursorRow {
                                rowid: rid,
                                cols: r[1..].to_vec(),
                                aux_handle: Value::Null,
                                rank: Value::Null,
                            });
                        }
                    }
                    ContentMode::External { table, rowid_col } => {
                        let ext_q = quote_ident(table);
                        let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                        let mut cols = vec![rid_q.clone()];
                        for c in &self.cfg.columns {
                            cols.push(quote_ident(c));
                        }
                        let qrows = ctx.query_sql(
                            &format!(
                                "SELECT {} FROM {db_q}.{ext_q} ORDER BY {rid_q} {order_kw}",
                                cols.join(", ")
                            ),
                            &[],
                        )?;
                        for r in qrows {
                            let rid = r[0].to_integer_lossy();
                            self.rows.push(Fts5CursorRow {
                                rowid: rid,
                                cols: r[1..].to_vec(),
                                aux_handle: Value::Null,
                                rank: Value::Null,
                            });
                        }
                    }
                    ContentMode::Contentless => {
                        if self.cfg.columnsize {
                            let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
                            let qrows = ctx.query_sql(
                                &format!(
                                    "SELECT id FROM {db_q}.{docsize_q} ORDER BY id {order_kw}"
                                ),
                                &[],
                            )?;
                            for r in qrows {
                                let rid = r[0].to_integer_lossy();
                                self.rows.push(Fts5CursorRow {
                                    rowid: rid,
                                    cols: vec![Value::Null; self.cfg.columns.len()],
                                    aux_handle: Value::Null,
                                    rank: Value::Null,
                                });
                            }
                        }
                    }
                }
            }
            IDX_ROWID_EQ => {
                let Some(Value::Integer(rid)) = args.first() else {
                    return Ok(());
                };
                match &self.cfg.content_mode {
                    ContentMode::Normal => {
                        let content_q = quote_ident(&format!("{}_content", self.table_name));
                        let cols: Vec<String> = (0..self.cfg.columns.len())
                            .map(|i| format!("c{i}"))
                            .collect();
                        let qrows = ctx.query_sql(
                            &format!(
                                "SELECT {} FROM {db_q}.{content_q} WHERE id = ?1",
                                cols.join(", ")
                            ),
                            &[Value::Integer(*rid)],
                        )?;
                        if let Some(r) = qrows.into_iter().next() {
                            self.rows.push(Fts5CursorRow {
                                rowid: *rid,
                                cols: r,
                                aux_handle: Value::Null,
                                rank: Value::Null,
                            });
                        }
                    }
                    ContentMode::External { .. } => {
                        let vals = self.load_content_vals(ctx, *rid)?;
                        self.rows.push(Fts5CursorRow {
                            rowid: *rid,
                            cols: vals,
                            aux_handle: Value::Null,
                            rank: Value::Null,
                        });
                    }
                    ContentMode::Contentless => {
                        if self.cfg.columnsize {
                            let docsize_q = quote_ident(&format!("{}_docsize", self.table_name));
                            let qrows = ctx.query_sql(
                                &format!("SELECT 1 FROM {db_q}.{docsize_q} WHERE id = ?1"),
                                &[Value::Integer(*rid)],
                            )?;
                            if !qrows.is_empty() {
                                self.rows.push(Fts5CursorRow {
                                    rowid: *rid,
                                    cols: vec![Value::Null; self.cfg.columns.len()],
                                    aux_handle: Value::Null,
                                    rank: Value::Null,
                                });
                            }
                        }
                    }
                }
            }
            IDX_LIKE | IDX_GLOB => {
                let Some(pat_val) = args.first() else {
                    return Ok(());
                };
                if pat_val.is_null() {
                    return Ok(());
                }
                let pat = pat_val.to_text_lossy();
                let runs = extract_like_glob_runs(&pat, mode == IDX_GLOB);
                if runs.is_empty() {
                    // Fall back to full scan (outer engine still applies LIKE/GLOB filter)
                    return self.filter(ctx, IDX_FULLSCAN, None, &[]);
                }
                let merged_index =
                    load_merged_index(ctx, &db_q, &self.table_name, self.cfg.detail)?;
                let mut candidate_rowids: Option<BTreeSet<i64>> = None;
                for run in &runs {
                    for tg in self.cfg.tokenizer.tokenize(run, true) {
                        let tkey = make_term_key(0, &tg.term);
                        let set: BTreeSet<i64> = merged_index
                            .get(&tkey)
                            .map(|m| m.keys().copied().collect())
                            .unwrap_or_default();
                        candidate_rowids = Some(match candidate_rowids {
                            None => set,
                            Some(prev) => prev.intersection(&set).copied().collect(),
                        });
                    }
                }
                let mut rids: Vec<i64> = candidate_rowids.unwrap_or_default().into_iter().collect();
                if desc_rowid {
                    rids.reverse();
                }
                for rid in rids {
                    let cols = self.load_content_vals(ctx, rid)?;
                    self.rows.push(Fts5CursorRow {
                        rowid: rid,
                        cols,
                        aux_handle: Value::Null,
                        rank: Value::Null,
                    });
                }
            }
            IDX_MATCH => {
                let Some(qval) = args.first() else {
                    return Ok(());
                };
                if qval.is_null() {
                    return Ok(());
                }
                let qstr = qval.to_text_lossy();
                let rank_weights = if (idx_num & IDX_FLAG_HAS_RANK_CFG) != 0 {
                    let rv = args.get(1).cloned().unwrap_or(Value::Null);
                    parse_rank_expression(&rv.to_text_lossy())?
                } else {
                    self.load_configured_rank_weights(ctx)?
                };

                let lexed = lex_fts5_query(&qstr)?;
                let mut parser = QueryParser {
                    toks: &lexed,
                    pos: 0,
                    cfg: &self.cfg,
                    phrases: Vec::new(),
                };
                let mut qexpr = parser.parse_root()?;
                if let Some(col_name) = idx_str
                    && let Some(cidx) = self
                        .cfg
                        .columns
                        .iter()
                        .position(|c| c.eq_ignore_ascii_case(col_name))
                {
                    let mut set = BTreeSet::new();
                    set.insert(cidx);
                    qexpr = FtsQueryExpr::ColFilter {
                        filter: ColFilterSpec::Include(set),
                        inner: Box::new(qexpr),
                    };
                }
                let phrases = parser.phrases;
                let merged_index =
                    load_merged_index(ctx, &db_q, &self.table_name, self.cfg.detail)?;

                // Special handling for trigram terms < 3 chars (full content scan substring verify)
                // and >= 3 chars (index intersect + exact substring verification).
                let is_trigram = self.cfg.tokenizer.is_trigram();
                let has_short_trigram = is_trigram
                    && phrases
                        .iter()
                        .any(|p| p.tokens.is_empty() && !p.raw_text.is_empty());

                let mut matched_docs = if has_short_trigram {
                    // Load all candidate rowids from content table
                    let mut dummy_cur = Fts5Cursor {
                        table_name: self.table_name.clone(),
                        cfg: self.cfg.clone(),
                        aux_registry: Arc::clone(&self.aux_registry),
                        rows: Vec::new(),
                        pos: 0,
                    };
                    dummy_cur.filter(ctx, IDX_FULLSCAN, None, &[])?;
                    let mut all = EvalDocSet::new();
                    for r in dummy_cur.rows {
                        all.insert(r.rowid, Vec::new());
                    }
                    all
                } else {
                    eval_query_expr(&qexpr, &phrases, &self.cfg, &merged_index, None)
                };

                let mut phrase_doc_hits = vec![0usize; phrases.len()];
                for (pi, ph) in phrases.iter().enumerate() {
                    let pdocs = eval_phrase_against_index(pi, ph, &self.cfg, &merged_index);
                    phrase_doc_hits[pi] = pdocs.len();
                }

                let avgs = self.read_averages(ctx)?;
                let total_rows = avgs[0] as usize;
                let total_tokens: usize = avgs[1..].iter().map(|&x| x as usize).sum();

                let mut raw_entries: Vec<(i64, Vec<Value>, AuxRowEntry)> = Vec::new();
                for (rid, insts) in std::mem::take(&mut matched_docs) {
                    let cols = self.load_content_vals(ctx, rid)?;
                    let col_sizes = self.load_col_sizes(ctx, rid, &cols)?;
                    let col_texts: Vec<Option<String>> = cols
                        .iter()
                        .map(|v| (!v.is_null()).then(|| v.to_text_lossy()))
                        .collect();

                    let mut trigram_byte_spans = vec![Vec::new(); self.cfg.columns.len()];
                    if let Some((cs, rd)) = self.cfg.tokenizer.trigram_params()
                        && self.cfg.content_mode != ContentMode::Contentless
                    {
                        let mut any_span = false;
                        for ph in &phrases {
                            for (ci, ct_opt) in col_texts.iter().enumerate() {
                                if self.cfg.unindexed[ci] {
                                    continue;
                                }
                                if let Some(ct) = ct_opt {
                                    let spans =
                                        find_trigram_substring_spans(ct, &ph.raw_text, cs, rd);
                                    if !spans.is_empty() {
                                        any_span = true;
                                        trigram_byte_spans[ci].extend(spans);
                                    }
                                }
                            }
                        }
                        if !any_span && !phrases.is_empty() {
                            continue;
                        }
                    }

                    raw_entries.push((
                        rid,
                        cols,
                        AuxRowEntry {
                            col_texts,
                            col_sizes,
                            instances: insts,
                            trigram_byte_spans,
                        },
                    ));
                }

                let aux_rows: Vec<AuxRowEntry> =
                    raw_entries.iter().map(|(_, _, ar)| ar.clone()).collect();
                let qctx = Arc::new(AuxQueryContext {
                    num_cols: self.cfg.columns.len(),
                    tokenizer: self.cfg.tokenizer.clone(),
                    is_trigram_substring: is_trigram,
                    phrase_lengths: phrases.iter().map(|p| p.tokens.len().max(1)).collect(),
                    phrase_doc_hits,
                    total_rows: total_rows.max(raw_entries.len()),
                    total_tokens,
                    rows: aux_rows,
                });
                let handles = self.aux_registry.register_query_rows(Arc::clone(&qctx));

                for (idx, (rid, cols, _)) in raw_entries.into_iter().enumerate() {
                    let rank_score = compute_bm25_for_row(&qctx, idx, &rank_weights);
                    self.rows.push(Fts5CursorRow {
                        rowid: rid,
                        cols,
                        aux_handle: Value::Integer(handles[idx]),
                        rank: Value::real(rank_score),
                    });
                }

                if (idx_num & IDX_FLAG_ORDER_RANK_ASC) != 0 {
                    self.rows.sort_by(|a, b| {
                        a.rank
                            .to_real_lossy()
                            .total_cmp(&b.rank.to_real_lossy())
                            .then_with(|| a.rowid.cmp(&b.rowid))
                    });
                } else if (idx_num & IDX_FLAG_ORDER_RANK_DESC) != 0 {
                    self.rows.sort_by(|a, b| {
                        b.rank
                            .to_real_lossy()
                            .total_cmp(&a.rank.to_real_lossy())
                            .then_with(|| a.rowid.cmp(&b.rowid))
                    });
                } else if desc_rowid {
                    self.rows.reverse();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if self.pos < self.rows.len() {
            self.pos += 1;
        }
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let Some(row) = self.rows.get(self.pos) else {
            return Ok(Value::Null);
        };
        let n_cols = self.cfg.columns.len();
        if col_idx < n_cols {
            Ok(row.cols.get(col_idx).cloned().unwrap_or(Value::Null))
        } else if col_idx == n_cols {
            Ok(row.aux_handle.clone())
        } else if col_idx == n_cols + 1 {
            Ok(row.rank.clone())
        } else {
            Ok(Value::Null)
        }
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.rows.get(self.pos).map_or(0, |r| r.rowid))
    }
}

// ============================================================================
// `Fts5VocabModule` (`fts5vocab` Virtual Table: `'row'`, `'col'`, `'instance'`)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VocabType {
    Row,
    Col,
    Instance,
}

/// Built-in `fts5vocab` virtual table module for inspecting an `fts5` table's inverted index.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fts5VocabModule;

impl VirtualTableModule for Fts5VocabModule {
    fn name(&self) -> &str {
        "fts5vocab"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.connect(ctx, args)
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        let (target_db, target_tbl, vtype) = match args.len() {
            2 => {
                let tbl = unquote_sql_token(args[0].trim())?;
                let ty = parse_vocab_type(&unquote_sql_token(args[1].trim())?)?;
                (None, tbl, ty)
            }
            3 => {
                let db = unquote_sql_token(args[0].trim())?;
                let tbl = unquote_sql_token(args[1].trim())?;
                let ty = parse_vocab_type(&unquote_sql_token(args[2].trim())?)?;
                (Some(db), tbl, ty)
            }
            _ => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "fts5vocab: wrong number of arguments",
                ));
            }
        };

        let decl = match vtype {
            VocabType::Row => "CREATE TABLE x(term, doc, cnt)",
            VocabType::Col => "CREATE TABLE x(term, col, doc, cnt)",
            VocabType::Instance => "CREATE TABLE x(term, doc, col, offset)",
        };
        ctx.declare_vtab(decl)?;
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
        Ok(Box::new(Fts5VocabTable {
            target_db,
            target_tbl,
            vtype,
        }))
    }
}

fn parse_vocab_type(s: &str) -> Result<VocabType> {
    match s {
        "row" => Ok(VocabType::Row),
        "col" => Ok(VocabType::Col),
        "instance" => Ok(VocabType::Instance),
        _ => Err(SqliteError::new(
            ErrorCode::Error,
            "fts5vocab: unknown table type",
        )),
    }
}

#[derive(Debug)]
struct Fts5VocabTable {
    target_db: Option<String>,
    target_tbl: String,
    vtype: VocabType,
}

impl VirtualTable for Fts5VocabTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        info.estimated_cost = 100.0;
        info.estimated_rows = 100;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(Fts5VocabCursor {
            target_db: self.target_db.clone(),
            target_tbl: self.target_tbl.clone(),
            vtype: self.vtype,
            rows: Vec::new(),
            pos: 0,
        }))
    }
}

#[derive(Debug)]
struct Fts5VocabCursor {
    target_db: Option<String>,
    target_tbl: String,
    vtype: VocabType,
    rows: Vec<Vec<Value>>,
    pos: usize,
}

fn split_ddl_args(inner: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut in_quote: Option<char> = None;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = in_quote {
            cur.push(c);
            if c == q {
                if chars.peek().copied() == Some(q) {
                    if let Some(escaped) = chars.next() {
                        cur.push(escaped);
                    }
                } else {
                    in_quote = None;
                }
            }
        } else {
            match c {
                '\'' | '"' | '`' => {
                    in_quote = Some(c);
                    cur.push(c);
                }
                '[' => {
                    in_quote = Some(']');
                    cur.push(c);
                }
                ',' => {
                    args.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            }
        }
    }
    if !cur.trim().is_empty() {
        args.push(cur);
    }
    args
}

fn detect_fts5_table_detail(
    ctx: &mut dyn VTabHostContext,
    db_q: &str,
    target_tbl: &str,
) -> DetailMode {
    let Ok(rows) = ctx.query_sql(
        &format!(
            "SELECT sql FROM {db_q}.sqlite_schema WHERE type = 'table' AND name = ?1 COLLATE NOCASE"
        ),
        &[Value::Text(target_tbl.to_owned())],
    ) else {
        return DetailMode::Full;
    };
    let Some(Value::Text(sql)) = rows.first().and_then(|r| r.first()) else {
        return DetailMode::Full;
    };
    let Some(open) = sql.find('(') else {
        return DetailMode::Full;
    };
    let Some(close) = sql.rfind(')') else {
        return DetailMode::Full;
    };
    if close <= open {
        return DetailMode::Full;
    }
    let raw_args = split_ddl_args(&sql[open + 1..close]);
    Fts5Config::parse(target_tbl, &raw_args)
        .map(|cfg| cfg.detail)
        .unwrap_or(DetailMode::Full)
}

impl VirtualCursor for Fts5VocabCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        _idx_num: i32,
        _idx_str: Option<&str>,
        _args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        let db_name = self.target_db.as_deref().unwrap_or_else(|| ctx.db_name());
        let db_q = quote_ident(db_name);

        // Fetch target FTS5 column names via `pragma_table_info`
        let info_rows = ctx.query_sql(
            "SELECT name FROM pragma_table_info(?1, ?2) ORDER BY cid ASC",
            &[
                Value::Text(self.target_tbl.clone()),
                Value::Text(db_name.to_owned()),
            ],
        )?;
        let col_names: Vec<String> = info_rows
            .into_iter()
            .filter_map(|mut r| match r.pop() {
                Some(Value::Text(s)) => Some(s),
                _ => None,
            })
            .collect();

        let target_detail = detect_fts5_table_detail(ctx, &db_q, &self.target_tbl);
        let merged = load_merged_index(ctx, &db_q, &self.target_tbl, target_detail)?;

        for (tkey, docs) in merged {
            // Only main-index terms (`b'0'` prefix byte), skip prefix-index entries (`b'1'..`)
            if tkey.first().copied() != Some(b'0') {
                continue;
            }
            let term_str = String::from_utf8_lossy(&tkey[1..]).into_owned();
            match self.vtype {
                VocabType::Row => {
                    let doc_cnt = docs.len() as i64;
                    let mut tot_cnt = 0i64;
                    for cols in docs.values() {
                        for offs in cols.values() {
                            tot_cnt += offs.len().max(1) as i64;
                        }
                    }
                    self.rows.push(vec![
                        Value::Text(term_str),
                        Value::Integer(doc_cnt),
                        Value::Integer(tot_cnt),
                    ]);
                }
                VocabType::Col => {
                    let mut by_col: BTreeMap<usize, (i64, i64)> = BTreeMap::new();
                    for cols in docs.values() {
                        for (&cidx, offs) in cols {
                            let e = by_col.entry(cidx).or_insert((0, 0));
                            e.0 += 1;
                            e.1 += offs.len().max(1) as i64;
                        }
                    }
                    // Sort by column name ascending (matching C SQLite `fts5vocab` `'col'` output!)
                    let mut col_rows: Vec<(String, i64, i64)> = by_col
                        .into_iter()
                        .map(|(cidx, (dc, tc))| {
                            let cname = col_names
                                .get(cidx)
                                .cloned()
                                .unwrap_or_else(|| format!("c{cidx}"));
                            (cname, dc, tc)
                        })
                        .collect();
                    col_rows.sort_by(|a, b| a.0.cmp(&b.0));
                    for (cname, dc, tc) in col_rows {
                        self.rows.push(vec![
                            Value::Text(term_str.clone()),
                            Value::Text(cname),
                            Value::Integer(dc),
                            Value::Integer(tc),
                        ]);
                    }
                }
                VocabType::Instance => {
                    for (rowid, cols) in docs {
                        for (cidx, offs) in cols {
                            let cname = col_names
                                .get(cidx)
                                .cloned()
                                .unwrap_or_else(|| format!("c{cidx}"));
                            if offs.is_empty() {
                                self.rows.push(vec![
                                    Value::Text(term_str.clone()),
                                    Value::Integer(rowid),
                                    Value::Text(cname),
                                    Value::Null,
                                ]);
                            } else {
                                for off in offs {
                                    self.rows.push(vec![
                                        Value::Text(term_str.clone()),
                                        Value::Integer(rowid),
                                        Value::Text(cname.clone()),
                                        Value::Integer(off as i64),
                                    ]);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if self.pos < self.rows.len() {
            self.pos += 1;
        }
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        Ok(self
            .rows
            .get(self.pos)
            .and_then(|r| r.get(col_idx))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        Ok((self.pos + 1) as i64)
    }
}
