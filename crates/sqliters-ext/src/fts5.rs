//! Pure safe Rust implementation of SQLite's `fts5` and `fts5vocab` full-text search
//! virtual table modules (`sqlite/ext/fts5/`).
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
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
const FTS5_AUX_RING_SLOTS: usize = 64;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PhraseInstance {
    phrase_idx: u32,
    col_idx: u32,
    token_offset: u32,
}

#[derive(Debug, Clone)]
enum PhraseHits {
    Inline { len: u8, items: [PhraseInstance; 4] },
    Heap(Vec<PhraseInstance>),
}

impl Default for PhraseHits {
    fn default() -> Self {
        Self::new()
    }
}

impl PhraseHits {
    #[inline]
    const fn new() -> Self {
        Self::Inline {
            len: 0,
            items: [PhraseInstance {
                phrase_idx: 0,
                col_idx: 0,
                token_offset: 0,
            }; 4],
        }
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Inline { len, .. } => *len as usize,
            Self::Heap(v) => v.len(),
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    fn as_slice(&self) -> &[PhraseInstance] {
        match self {
            Self::Inline { len, items } => &items[..(*len as usize)],
            Self::Heap(v) => v.as_slice(),
        }
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [PhraseInstance] {
        match self {
            Self::Inline { len, items } => &mut items[..(*len as usize)],
            Self::Heap(v) => v.as_mut_slice(),
        }
    }

    #[inline]
    fn push(&mut self, item: PhraseInstance) {
        match self {
            Self::Inline { len, items } => {
                let n = *len as usize;
                if n < 4 {
                    items[n] = item;
                    *len = (n + 1) as u8;
                } else {
                    let mut v = Vec::with_capacity(8);
                    v.extend_from_slice(items);
                    v.push(item);
                    *self = Self::Heap(v);
                }
            }
            Self::Heap(v) => v.push(item),
        }
    }

    #[inline]
    fn extend_from(&mut self, other: &Self) {
        let add = other.as_slice();
        if add.is_empty() {
            return;
        }
        match self {
            Self::Inline { len, items } => {
                let n = *len as usize;
                if n + add.len() <= 4 {
                    items[n..n + add.len()].copy_from_slice(add);
                    *len = (n + add.len()) as u8;
                } else {
                    let mut v = Vec::with_capacity(n + add.len());
                    v.extend_from_slice(&items[..n]);
                    v.extend_from_slice(add);
                    *self = Self::Heap(v);
                }
            }
            Self::Heap(v) => v.extend_from_slice(add),
        }
    }

    #[inline]
    fn retain(&mut self, mut f: impl FnMut(&PhraseInstance) -> bool) {
        match self {
            Self::Inline { len, items } => {
                let n = *len as usize;
                let mut w = 0usize;
                for r in 0..n {
                    if f(&items[r]) {
                        items[w] = items[r];
                        w += 1;
                    }
                }
                *len = w as u8;
            }
            Self::Heap(v) => v.retain(f),
        }
    }

    #[inline]
    fn dedup_sorted(&mut self) {
        if self.len() <= 1 {
            return;
        }
        self.as_mut_slice()
            .sort_unstable_by_key(|x| (x.col_idx, x.token_offset, x.phrase_idx));
        match self {
            Self::Inline { len, items } => {
                let n = *len as usize;
                let mut w = 1usize;
                for r in 1..n {
                    if items[r] != items[w - 1] {
                        items[w] = items[r];
                        w += 1;
                    }
                }
                *len = w as u8;
            }
            Self::Heap(v) => v.dedup(),
        }
    }
}

#[derive(Debug, Clone)]
enum DocColSizes {
    Inline { len: u8, vals: [u32; 6] },
    Heap(Arc<[usize]>),
}

impl DocColSizes {
    #[inline]
    fn from_u64_slice(slice: &[u64]) -> Self {
        if slice.len() <= 6 && slice.iter().all(|&x| x <= u64::from(u32::MAX)) {
            let mut vals = [0u32; 6];
            for (i, &x) in slice.iter().enumerate() {
                vals[i] = x as u32;
            }
            Self::Inline {
                len: slice.len() as u8,
                vals,
            }
        } else {
            Self::Heap(slice.iter().map(|&x| x as usize).collect())
        }
    }

    #[inline]
    fn from_usize_vec(vec: Vec<usize>) -> Self {
        if vec.len() <= 6 && vec.iter().all(|&x| x <= u32::MAX as usize) {
            let mut vals = [0u32; 6];
            for (i, &x) in vec.iter().enumerate() {
                vals[i] = x as u32;
            }
            Self::Inline {
                len: vec.len() as u8,
                vals,
            }
        } else {
            Self::Heap(vec.into())
        }
    }

    #[inline]
    fn zeros(n_cols: usize) -> Self {
        if n_cols <= 6 {
            Self::Inline {
                len: n_cols as u8,
                vals: [0u32; 6],
            }
        } else {
            Self::Heap(vec![0usize; n_cols].into())
        }
    }

    #[inline]
    fn total_len(&self) -> usize {
        match self {
            Self::Inline { len, vals } => {
                let mut s = 0usize;
                for &v in &vals[..(*len as usize)] {
                    s += v as usize;
                }
                s
            }
            Self::Heap(a) => a.iter().sum(),
        }
    }

    #[inline]
    fn get(&self, idx: usize) -> usize {
        match self {
            Self::Inline { len, vals } => {
                if idx < (*len as usize) {
                    vals[idx] as usize
                } else {
                    0
                }
            }
            Self::Heap(a) => a.get(idx).copied().unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone)]
struct AuxRowTexts {
    col_texts: Vec<Option<String>>,
    /// Used by trigram substring highlight/snippet when byte ranges are matched directly.
    trigram_byte_spans: Vec<Vec<(usize, usize)>>,
}

#[derive(Debug, Clone)]
struct AuxRowEntry {
    col_sizes: DocColSizes,
    instances: PhraseHits,
    texts: Option<Box<AuxRowTexts>>,
}

#[derive(Debug)]
struct AuxQueryContext {
    num_cols: usize,
    tokenizer: TokenizerSpec,
    is_trigram_substring: bool,
    phrase_lengths: Vec<usize>,
    phrase_idfs: Vec<f64>,
    avgdl: f64,
    total_rows: usize,
    rows: Vec<AuxRowEntry>,
}

impl AuxQueryContext {
    #[allow(clippy::too_many_arguments)]
    fn build(
        num_cols: usize,
        tokenizer: TokenizerSpec,
        is_trigram_substring: bool,
        phrase_lengths: Vec<usize>,
        phrase_doc_hits: &[usize],
        total_rows: usize,
        total_tokens: usize,
        rows: Vec<AuxRowEntry>,
    ) -> Self {
        let avgdl = if total_tokens == 0 || total_rows == 0 {
            1.0
        } else {
            (total_tokens as f64) / (total_rows as f64)
        };
        let n_row_f = total_rows as f64;
        let mut phrase_idfs = Vec::with_capacity(phrase_lengths.len());
        if total_rows > 0 {
            for &hit in phrase_doc_hits {
                let n_hit = hit as f64;
                let mut idf = ((n_row_f - n_hit + 0.5) / (n_hit + 0.5)).ln();
                if idf <= 0.0 {
                    idf = 1.0e-6;
                }
                phrase_idfs.push(idf);
            }
        }
        Self {
            num_cols,
            tokenizer,
            is_trigram_substring,
            phrase_lengths,
            phrase_idfs,
            avgdl,
            total_rows,
            rows,
        }
    }
}

#[derive(Debug)]
struct AuxRegistryInner {
    next_qid: u32,
    slots: [Option<(u32, Arc<AuxQueryContext>)>; FTS5_AUX_RING_SLOTS],
}

impl Default for AuxRegistryInner {
    fn default() -> Self {
        Self {
            next_qid: 0,
            slots: std::array::from_fn(|_| None),
        }
    }
}

/// Connection-scoped registry mapping FTS5 hidden table-column row handles to active match state
/// for `bm25()`, `highlight()`, and `snippet()`.
#[derive(Debug, Default)]
pub struct Fts5AuxRegistry {
    inner: Mutex<AuxRegistryInner>,
    aux_text_depth: AtomicUsize,
}

impl Fts5AuxRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn enter_text_aux_scope(&self) {
        self.aux_text_depth.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn exit_text_aux_scope(&self) {
        self.aux_text_depth.fetch_sub(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn needs_aux_text(&self) -> bool {
        self.aux_text_depth.load(Ordering::Relaxed) > 0
    }

    #[inline]
    fn encode_handle(qid: u32, row_idx: usize) -> i64 {
        ((qid as i64) << 32) | ((row_idx as u32) as i64)
    }

    fn register_query(&self, qctx: Arc<AuxQueryContext>) -> u32 {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let qid = if guard.next_qid >= 0x7fff_fffe {
            1
        } else {
            guard.next_qid + 1
        };
        guard.next_qid = qid;
        guard.slots[(qid as usize) & (FTS5_AUX_RING_SLOTS - 1)] = Some((qid, qctx));
        qid
    }

    #[inline]
    fn lookup(&self, val: &Value) -> Result<(Arc<AuxQueryContext>, usize)> {
        let Value::Integer(handle) = val else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            ));
        };
        if *handle <= 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            ));
        }
        let qid = ((*handle as u64) >> 32) as u32;
        let row_idx = (*handle as u32) as usize;
        if qid == 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            ));
        }
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((slot_qid, ref qctx)) = guard.slots[(qid as usize) & (FTS5_AUX_RING_SLOTS - 1)]
            && slot_qid == qid
            && row_idx < qctx.rows.len()
        {
            return Ok((Arc::clone(qctx), row_idx));
        }
        Err(SqliteError::new(
            ErrorCode::Error,
            "unable to use function in the requested context",
        ))
    }

    #[inline]
    pub fn eval_bm25_fast(&self, handle_val: &Value, weights: &[f64]) -> Result<Value> {
        let Value::Integer(handle) = handle_val else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            ));
        };
        if *handle <= 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            ));
        }
        let qid = ((*handle as u64) >> 32) as u32;
        let row_idx = (*handle as u32) as usize;
        if qid == 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "unable to use function in the requested context",
            ));
        }
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((slot_qid, ref qctx)) = guard.slots[(qid as usize) & (FTS5_AUX_RING_SLOTS - 1)]
            && slot_qid == qid
            && row_idx < qctx.rows.len()
        {
            let score = compute_bm25_for_row(qctx, row_idx, weights);
            return Ok(Value::real(score));
        }
        Err(SqliteError::new(
            ErrorCode::Error,
            "unable to use function in the requested context",
        ))
    }

    pub fn eval_bm25(&self, args: &[Value]) -> Result<Value> {
        let Some(first) = args.first() else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "wrong number of arguments to function bm25()",
            ));
        };
        if args.len() == 1 {
            return self.eval_bm25_fast(first, &[]);
        }
        let weights: Vec<f64> = args[1..].iter().map(Value::to_real_lossy).collect();
        self.eval_bm25_fast(first, &weights)
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
        let Some(ref texts) = row.texts else {
            return Ok(Value::Null);
        };
        let Some(ref text) = texts.col_texts[col_idx] else {
            return Ok(Value::Null);
        };
        if qctx.is_trigram_substring {
            return Ok(Value::Text(render_byte_spans_highlight(
                text,
                &texts.trigram_byte_spans[col_idx],
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
    let n_phrase = qctx.phrase_idfs.len();
    if n_phrase == 0 || qctx.total_rows == 0 {
        return 0.0;
    }
    let avgdl = qctx.avgdl;
    let row = &qctx.rows[row_idx];
    let doc_len: f64 = row.col_sizes.total_len() as f64;
    let mut freq_stack = [0.0f64; 8];
    let mut freq_heap: Option<Vec<f64>> = (n_phrase > 8).then(|| vec![0.0f64; n_phrase]);
    let freq: &mut [f64] = match freq_heap.as_mut() {
        Some(v) => v.as_mut_slice(),
        None => &mut freq_stack[..n_phrase],
    };
    for inst in row.instances.as_slice() {
        let pidx = inst.phrase_idx as usize;
        if pidx < n_phrase {
            let w = weights.get(inst.col_idx as usize).copied().unwrap_or(1.0);
            freq[pidx] += w;
        }
    }
    let norm = K1 * (1.0 - B + B * doc_len / avgdl);
    let mut score = 0.0f64;
    for (i, &f) in freq.iter().enumerate() {
        if f == 0.0 {
            continue;
        }
        let denom = f + norm;
        if denom > 0.0 {
            score += qctx.phrase_idfs[i] * ((f * (K1 + 1.0)) / denom);
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
    for inst in row.instances.as_slice() {
        if (inst.col_idx as usize) != col_idx {
            continue;
        }
        let len = qctx
            .phrase_lengths
            .get(inst.phrase_idx as usize)
            .copied()
            .unwrap_or(1);
        if len == 0 {
            continue;
        }
        let s = inst.token_offset as usize;
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

    let Some(ref row_texts) = row.texts else {
        return Value::Null;
    };

    if qctx.is_trigram_substring {
        for c in 0..qctx.num_cols {
            if req_col >= 0 && (req_col as usize) != c {
                continue;
            }
            if !row_texts.trigram_byte_spans[c].is_empty() {
                best_col = c;
                break;
            }
        }
        let Some(ref text) = row_texts.col_texts[best_col] else {
            return Value::Null;
        };
        return Value::Text(render_byte_spans_highlight(
            text,
            &row_texts.trigram_byte_spans[best_col],
            open_tag,
            close_tag,
        ));
    }

    let n_phrase = qctx.phrase_lengths.len();
    for c in 0..qctx.num_cols {
        if req_col >= 0 && (req_col as usize) != c {
            continue;
        }
        let Some(ref col_text) = row_texts.col_texts[c] else {
            continue;
        };
        let docsize = row.col_sizes.get(c);

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
            for cand in row.instances.as_slice() {
                let c_off = cand.token_offset as usize;
                let c_pidx = cand.phrase_idx as usize;
                if (cand.col_idx as usize) == c && c_off >= start_pos && c_off < end {
                    sc += if seen[c_pidx] { 1 } else { 1000 };
                    seen[c_pidx] = true;
                    if fo.is_none() {
                        fo = Some(c_off);
                    }
                    lo = c_off + qctx.phrase_lengths[c_pidx];
                }
            }
            (sc, fo, lo)
        };

        for inst in row.instances.as_slice() {
            if (inst.col_idx as usize) != c {
                continue;
            }
            let pos = inst.token_offset as usize;
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

    let Some(ref text) = row_texts.col_texts[best_col] else {
        return Value::Null;
    };
    if max_tokens == 0 {
        return Value::Text(String::new());
    }
    let col_size = row.col_sizes.get(best_col);
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

    fn for_each_doc_token(&self, text: &str, mut cb: impl FnMut(&str)) {
        let mut apply_porter = false;
        let mut cur = self;
        while let Self::Porter(inner) = cur {
            apply_porter = true;
            cur = inner;
        }
        let mut emit = |term: &str| {
            if apply_porter
                && term.len() >= 3
                && term.len() < 64
                && term.bytes().all(|b| b.is_ascii_lowercase())
            {
                let stemmed = porter_stem(term);
                cb(&stemmed);
            } else {
                cb(term);
            }
        };
        match cur {
            Self::Unicode61 {
                remove_diacritics,
                tokenchars,
                separators,
            } => {
                let mut rest_offset = 0usize;
                if tokenchars.is_empty() && separators.is_empty() {
                    let bytes = text.as_bytes();
                    let mut i = 0usize;
                    let mut scratch = String::new();
                    let mut hit_non_ascii = false;
                    while i + 8 <= bytes.len() {
                        let s = &bytes[i..i + 8];
                        let w =
                            u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                        if (w & 0x8080_8080_8080_8080) != 0 {
                            rest_offset = i;
                            hit_non_ascii = true;
                            break;
                        }
                        let (alnum, up) = swar_ascii_alnum_and_upper_msbs(w);
                        if alnum == 0 {
                            i += 8;
                            continue;
                        }
                        let lead_sep = (alnum.trailing_zeros() >> 3) as usize;
                        if lead_sep > 0 {
                            i += lead_sep;
                            continue;
                        }
                        let non_alnum = !alnum & 0x8080_8080_8080_8080;
                        if non_alnum != 0 {
                            let tok_len = (non_alnum.trailing_zeros() >> 3) as usize;
                            let tok_mask = u64::MAX >> ((8 - tok_len) * 8);
                            if (up & tok_mask) == 0 {
                                emit(&text[i..i + tok_len]);
                            } else {
                                let folded = (w | (up >> 2)).to_le_bytes();
                                if let Ok(fs) = std::str::from_utf8(&folded[..tok_len]) {
                                    emit(fs);
                                }
                            }
                            i += tok_len + 1;
                            continue;
                        }
                        // Token starts at `i` and spans all 8 ASCII alnum bytes `i..i+8`.
                        let start = i;
                        let mut has_upper = up != 0;
                        i += 8;
                        while i + 8 <= bytes.len() {
                            let s2 = &bytes[i..i + 8];
                            let w2 = u64::from_le_bytes([
                                s2[0], s2[1], s2[2], s2[3], s2[4], s2[5], s2[6], s2[7],
                            ]);
                            if (w2 & 0x8080_8080_8080_8080) != 0 {
                                break;
                            }
                            let (alnum2, up2) = swar_ascii_alnum_and_upper_msbs(w2);
                            if alnum2 == 0x8080_8080_8080_8080 {
                                has_upper |= up2 != 0;
                                i += 8;
                            } else {
                                let rem = ((!alnum2 & 0x8080_8080_8080_8080).trailing_zeros() >> 3)
                                    as usize;
                                if rem > 0 {
                                    let m2 = u64::MAX >> ((8 - rem) * 8);
                                    has_upper |= (up2 & m2) != 0;
                                    i += rem;
                                }
                                break;
                            }
                        }
                        while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                            has_upper |= bytes[i].is_ascii_uppercase();
                            i += 1;
                        }
                        if i < bytes.len() && bytes[i] >= 0x80 {
                            rest_offset = start;
                            hit_non_ascii = true;
                            break;
                        }
                        if !has_upper {
                            emit(&text[start..i]);
                        } else {
                            scratch.clear();
                            scratch.reserve(i - start);
                            for &b in &bytes[start..i] {
                                scratch.push(b.to_ascii_lowercase() as char);
                            }
                            emit(&scratch);
                        }
                        i += 1;
                    }
                    if !hit_non_ascii {
                        while i < bytes.len() {
                            if bytes[i] >= 0x80 {
                                rest_offset = i;
                                hit_non_ascii = true;
                                break;
                            }
                            if !bytes[i].is_ascii_alphanumeric() {
                                i += 1;
                                continue;
                            }
                            let start = i;
                            let mut has_upper = bytes[i].is_ascii_uppercase();
                            i += 1;
                            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                                has_upper |= bytes[i].is_ascii_uppercase();
                                i += 1;
                            }
                            if i < bytes.len() && bytes[i] >= 0x80 {
                                rest_offset = start;
                                hit_non_ascii = true;
                                break;
                            }
                            if !has_upper {
                                emit(&text[start..i]);
                            } else {
                                scratch.clear();
                                scratch.reserve(i - start);
                                for &b in &bytes[start..i] {
                                    scratch.push(b.to_ascii_lowercase() as char);
                                }
                                emit(&scratch);
                            }
                            i += 1;
                        }
                    }
                    if !hit_non_ascii {
                        return;
                    }
                }
                let mut cur_term = String::new();
                let mut in_tok = false;
                for (_, ch) in text[rest_offset..].char_indices() {
                    if is_unicode61_token_char(ch, tokenchars, separators) {
                        in_tok = true;
                        fold_unicode61_char(ch, *remove_diacritics, &mut cur_term);
                    } else if in_tok {
                        in_tok = false;
                        if !cur_term.is_empty() {
                            emit(&cur_term);
                            cur_term.clear();
                        }
                    }
                }
                if in_tok && !cur_term.is_empty() {
                    emit(&cur_term);
                }
            }
            Self::Ascii {
                tokenchars,
                separators,
            } => {
                let mut cur_term = String::new();
                let mut in_tok = false;
                for (_, ch) in text.char_indices() {
                    let is_tok = if separators.contains(&ch) {
                        false
                    } else if tokenchars.contains(&ch) {
                        true
                    } else {
                        ch.is_ascii_alphanumeric() || (ch as u32) >= 0x80
                    };
                    if is_tok {
                        in_tok = true;
                        if ch.is_ascii_uppercase() {
                            cur_term.push(ch.to_ascii_lowercase());
                        } else {
                            cur_term.push(ch);
                        }
                    } else if in_tok {
                        in_tok = false;
                        if !cur_term.is_empty() {
                            emit(&cur_term);
                            cur_term.clear();
                        }
                    }
                }
                if in_tok && !cur_term.is_empty() {
                    emit(&cur_term);
                }
            }
            Self::Trigram {
                case_sensitive,
                remove_diacritics,
            } => {
                for tok in tokenize_trigram(text, *case_sensitive, *remove_diacritics) {
                    emit(&tok.term);
                }
            }
            Self::Porter(_) => unreachable!(),
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

/// Given a 64-bit word `w` already verified pure ASCII (`(w & 0x8080_8080_8080_8080) == 0`),
/// computes `(alnum_msbs, upper_msbs)` across all 8 byte lanes in parallel using branchless SWAR.
#[inline(always)]
const fn swar_ascii_alnum_and_upper_msbs(w: u64) -> (u64, u64) {
    const HI: u64 = 0x8080_8080_8080_8080;
    let d = w.wrapping_add(0x5050_5050_5050_5050) & !w.wrapping_add(0x4646_4646_4646_4646);
    let fl = w | 0x2020_2020_2020_2020;
    let a = fl.wrapping_add(0x1f1f_1f1f_1f1f_1f1f) & !fl.wrapping_add(0x0505_0505_0505_0505);
    let alnum = (d | a) & HI;
    let up = w.wrapping_add(0x3f3f_3f3f_3f3f_3f3f) & !w.wrapping_add(0x2525_2525_2525_2525) & HI;
    (alnum, up)
}

fn tokenize_unicode61(
    text: &str,
    remove_diacritics: u8,
    tokenchars: &BTreeSet<char>,
    separators: &BTreeSet<char>,
) -> Vec<EmittedToken> {
    let mut out = Vec::new();
    let mut rest_offset = 0usize;
    if tokenchars.is_empty() && separators.is_empty() {
        let bytes = text.as_bytes();
        let mut i = 0usize;
        let mut hit_non_ascii = false;
        while i + 8 <= bytes.len() {
            let s = &bytes[i..i + 8];
            let w = u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
            if (w & 0x8080_8080_8080_8080) != 0 {
                rest_offset = i;
                hit_non_ascii = true;
                break;
            }
            let (alnum, up) = swar_ascii_alnum_and_upper_msbs(w);
            if alnum == 0 {
                i += 8;
                continue;
            }
            let lead_sep = (alnum.trailing_zeros() >> 3) as usize;
            if lead_sep > 0 {
                i += lead_sep;
                continue;
            }
            let non_alnum = !alnum & 0x8080_8080_8080_8080;
            if non_alnum != 0 {
                let tok_len = (non_alnum.trailing_zeros() >> 3) as usize;
                let tok_mask = u64::MAX >> ((8 - tok_len) * 8);
                let term = if (up & tok_mask) == 0 {
                    text[i..i + tok_len].to_owned()
                } else {
                    let folded = (w | (up >> 2)).to_le_bytes();
                    String::from_utf8_lossy(&folded[..tok_len]).into_owned()
                };
                out.push(EmittedToken {
                    term,
                    start_byte: i,
                    end_byte: i + tok_len,
                });
                i += tok_len + 1;
                continue;
            }
            let start = i;
            let mut has_upper = up != 0;
            i += 8;
            while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                has_upper |= bytes[i].is_ascii_uppercase();
                i += 1;
            }
            if i < bytes.len() && bytes[i] >= 0x80 {
                rest_offset = start;
                hit_non_ascii = true;
                break;
            }
            let term = if !has_upper {
                text[start..i].to_owned()
            } else {
                text[start..i].to_ascii_lowercase()
            };
            out.push(EmittedToken {
                term,
                start_byte: start,
                end_byte: i,
            });
            i += 1;
        }
        if !hit_non_ascii {
            while i < bytes.len() {
                if bytes[i] >= 0x80 {
                    rest_offset = i;
                    hit_non_ascii = true;
                    break;
                }
                if !bytes[i].is_ascii_alphanumeric() {
                    i += 1;
                    continue;
                }
                let start = i;
                let mut has_upper = bytes[i].is_ascii_uppercase();
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
                    has_upper |= bytes[i].is_ascii_uppercase();
                    i += 1;
                }
                if i < bytes.len() && bytes[i] >= 0x80 {
                    rest_offset = start;
                    hit_non_ascii = true;
                    break;
                }
                let term = if !has_upper {
                    text[start..i].to_owned()
                } else {
                    text[start..i].to_ascii_lowercase()
                };
                out.push(EmittedToken {
                    term,
                    start_byte: start,
                    end_byte: i,
                });
                i += 1;
            }
        }
        if !hit_non_ascii {
            return out;
        }
    }
    let mut cur_term = String::new();
    let mut cur_start: Option<usize> = None;
    let mut last_end = rest_offset;

    for (rel_idx, ch) in text[rest_offset..].char_indices() {
        let idx = rest_offset + rel_idx;
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
                        let v_lc = v.to_ascii_lowercase();
                        detail = if !v_lc.is_empty() && "full".starts_with(v_lc.as_str()) {
                            DetailMode::Full
                        } else if !v_lc.is_empty() && "columns".starts_with(v_lc.as_str()) {
                            DetailMode::Col
                        } else if !v_lc.is_empty() && "none".starts_with(v_lc.as_str()) {
                            DetailMode::None
                        } else {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                "malformed detail=... directive",
                            ));
                        };
                    }
                    "locale" => {
                        if !matches!(v.as_str(), "0" | "1") {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                "malformed locale=... directive",
                            ));
                        }
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

type MergedIndexMap = BTreeMap<Vec<u8>, BTreeMap<i64, BTreeMap<usize, Vec<usize>>>>;
type DocTermPostings = Vec<(Vec<u8>, BTreeMap<usize, Vec<usize>>)>;

#[derive(Clone)]
enum SmallBytes22 {
    Inline { len: u8, buf: [u8; 22] },
    Heap(Box<[u8]>),
}

impl SmallBytes22 {
    #[inline]
    const fn empty() -> Self {
        Self::Inline {
            len: 0,
            buf: [0u8; 22],
        }
    }

    #[inline]
    fn from_slice(s: &[u8]) -> Self {
        if s.len() <= 22 {
            let mut buf = [0u8; 22];
            buf[..s.len()].copy_from_slice(s);
            Self::Inline {
                len: s.len() as u8,
                buf,
            }
        } else {
            Self::Heap(s.into())
        }
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Inline { len, buf } => &buf[..(*len as usize)],
            Self::Heap(b) => b,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.as_slice().len()
    }
}

impl PartialEq for SmallBytes22 {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for SmallBytes22 {}

impl PartialOrd for SmallBytes22 {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SmallBytes22 {
    #[inline]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl std::hash::Hash for SmallBytes22 {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl std::borrow::Borrow<[u8]> for SmallBytes22 {
    #[inline]
    fn borrow(&self) -> &[u8] {
        self.as_slice()
    }
}

impl std::fmt::Debug for SmallBytes22 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_slice(), f)
    }
}

type EncodedDocTerms = Vec<(SmallBytes22, SmallBytes22)>;

fn encode_empty_structure_record(v2: bool) -> Vec<u8> {
    encode_empty_structure_record_with_cookie(v2, 0)
}

fn encode_empty_structure_record_with_cookie(v2: bool, cookie: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(11);
    out.extend_from_slice(&cookie.to_be_bytes());
    if v2 {
        out.extend_from_slice(b"\xff\x00\x00\x01");
    }
    out.extend_from_slice(&[0, 0, 0]);
    out
}

fn encode_single_seg_structure_record_with_cookie(
    v2: bool,
    cookie: u32,
    segid: u64,
    pgno_last: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&cookie.to_be_bytes());
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

fn read_structure_cookie(ctx: &mut dyn VTabHostContext, data_shadow: &str) -> u32 {
    if let Ok(Some(row)) = ctx.shadow_get_rowid(data_shadow, FTS5_STRUCTURE_ROWID)
        && let Some(Value::Blob(b)) = row.get(1)
        && b.len() >= 4
    {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    } else {
        0
    }
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

fn encode_posting_payload(
    detail: DetailMode,
    deleted: bool,
    col_hits: &BTreeMap<usize, Vec<usize>>,
) -> Vec<u8> {
    if deleted {
        match detail {
            DetailMode::None => vec![0x00],
            DetailMode::Col | DetailMode::Full => vec![0x01],
        }
    } else {
        encode_poslist(detail, col_hits)
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

type EncodedSegmentPages = (u64, Vec<(u64, Vec<u8>)>, Vec<(Vec<u8>, i64)>);

fn decode_full_poslist_payload(
    detail: DetailMode,
    payload: &[u8],
) -> Result<BTreeMap<usize, Vec<usize>>> {
    if detail == DetailMode::None {
        return decode_poslist_body(DetailMode::None, &[]);
    }
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let mut pos = 0usize;
    let _hdr = get_uvarint(payload, &mut pos).ok_or_else(err_corrupt)?;
    decode_poslist_body(detail, &payload[pos..])
}

fn encode_segment_pages_bytes<'a, I, D>(terms_iter: I) -> EncodedSegmentPages
where
    I: IntoIterator<Item = (&'a [u8], D)>,
    D: IntoIterator<Item = (i64, &'a [u8])>,
{
    let mut pgno = 1u64;
    let mut page_buf = Vec::with_capacity(FTS5_LEAF_TARGET_PGSZ + 256);
    page_buf.extend_from_slice(&[0u8; 4]);
    let mut pgidx = Vec::with_capacity(256);
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
        let mut next_buf = Vec::with_capacity(FTS5_LEAF_TARGET_PGSZ + 256);
        next_buf.extend_from_slice(&[0u8; 4]);
        let finished = std::mem::replace(page_buf, next_buf);
        leaves.push((*pgno, finished));
        pgidx.clear();
        *prev_pgidx_off = 0;
        prev_term_on_page.clear();
        *pgno += 1;
        *first_term_on_page = true;
        *first_rowid_on_page = true;
    };

    for (term, docs) in terms_iter {
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

        for (rowid, poslist) in docs {
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
            page_buf.extend_from_slice(poslist);
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
    (pgno_last, leaves, idx_rows)
}

fn encode_segment_pages_from_iter<'a, I, D>(
    detail: DetailMode,
    terms_iter: I,
) -> EncodedSegmentPages
where
    I: IntoIterator<Item = (&'a [u8], D)>,
    D: IntoIterator<Item = (i64, bool, &'a BTreeMap<usize, Vec<usize>>)>,
{
    let preencoded: Vec<_> = terms_iter
        .into_iter()
        .map(|(t, docs)| {
            let enc_docs: Vec<(i64, Vec<u8>)> = docs
                .into_iter()
                .map(|(rid, del, ch)| (rid, encode_posting_payload(detail, del, ch)))
                .collect();
            (t, enc_docs)
        })
        .collect();
    encode_segment_pages_bytes(
        preencoded
            .iter()
            .map(|(t, docs)| (*t, docs.iter().map(|(rid, pl)| (*rid, pl.as_slice())))),
    )
}

/// Writes `merged` into `<tbl>_data` and `<tbl>_idx` as a canonical C SQLite 3.54.1 FTS5 segment (`segid = 1`).
fn write_c_fts5_index(
    ctx: &mut dyn VTabHostContext,
    db_q: &str,
    tbl_name: &str,
    cfg: &Fts5Config,
    merged: &MergedIndexMap,
) -> Result<Vec<u8>> {
    let data_shadow = format!("{tbl_name}_data");
    let data_q = quote_ident(&data_shadow);
    let idx_q = quote_ident(&format!("{tbl_name}_idx"));
    let next_cookie = read_structure_cookie(ctx, &data_shadow).wrapping_add(1);

    ctx.execute_sql(
        &format!("DELETE FROM {db_q}.{data_q} WHERE id > {FTS5_STRUCTURE_ROWID}"),
        &[],
    )?;
    ctx.execute_sql(&format!("DELETE FROM {db_q}.{idx_q}"), &[])?;

    if merged.is_empty() {
        let sblob = encode_empty_structure_record_with_cookie(cfg.contentless_delete, next_cookie);
        ctx.shadow_put_rowid(
            &data_shadow,
            FTS5_STRUCTURE_ROWID,
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(sblob.clone()),
            ],
            true,
        )?;
        return Ok(sblob);
    }

    let segid = 1u64;
    let (pgno_last, leaves, idx_rows) = encode_segment_pages_from_iter(
        cfg.detail,
        merged
            .iter()
            .map(|(t, docs)| (t.as_slice(), docs.iter().map(|(&rid, ch)| (rid, false, ch)))),
    );

    for (leaf_pgno, leaf_bytes) in leaves {
        let lrid = fts5_segment_rowid(segid, leaf_pgno);
        ctx.shadow_put_rowid(
            &data_shadow,
            lrid,
            &[Value::Integer(lrid), Value::Blob(leaf_bytes)],
            true,
        )?;
    }
    let idx_ins_sql = format!("INSERT INTO {db_q}.{idx_q}(segid, term, pgno) VALUES(?1, ?2, ?3)");
    for (bt_key, bt_pg_val) in idx_rows {
        ctx.execute_sql(
            &idx_ins_sql,
            &[
                Value::Integer(segid as i64),
                Value::Blob(bt_key),
                Value::Integer(bt_pg_val),
            ],
        )?;
    }
    let sblob = encode_single_seg_structure_record_with_cookie(
        cfg.contentless_delete,
        next_cookie,
        segid,
        pgno_last,
    );
    ctx.shadow_put_rowid(
        &data_shadow,
        FTS5_STRUCTURE_ROWID,
        &[
            Value::Integer(FTS5_STRUCTURE_ROWID),
            Value::Blob(sblob.clone()),
        ],
        true,
    )?;
    Ok(sblob)
}

#[derive(Debug, Clone)]
struct Fts5SegRecord {
    segid: u64,
    pgno_first: u64,
    pgno_last: u64,
    n_tombstone_pages: u64,
    origin1: u64,
    origin2: u64,
}

#[derive(Debug, Clone)]
struct FullStructureRecord {
    cookie: u32,
    is_v2: bool,
    write_counter: u64,
    levels: Vec<(u64, Vec<Fts5SegRecord>)>,
}

impl FullStructureRecord {
    fn total_segments(&self) -> usize {
        self.levels.iter().map(|(_, s)| s.len()).sum()
    }

    fn max_segid(&self) -> u64 {
        self.levels
            .iter()
            .flat_map(|(_, s)| s.iter().map(|r| r.segid))
            .max()
            .unwrap_or(0)
    }

    fn chronological_segments(&self) -> Vec<Fts5SegRecord> {
        let mut chronological = Vec::with_capacity(self.total_segments());
        for (_, lsegs) in self.levels.iter().rev() {
            chronological.extend(lsegs.iter().cloned());
        }
        chronological
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.total_segments() * 12);
        out.extend_from_slice(&self.cookie.to_be_bytes());
        if self.is_v2 {
            out.extend_from_slice(b"\xff\x00\x00\x01");
        }
        if self.levels.is_empty() || self.total_segments() == 0 {
            out.extend_from_slice(&[0, 0, 0]);
            return out;
        }
        put_uvarint(&mut out, self.levels.len() as u64);
        put_uvarint(&mut out, self.total_segments() as u64);
        put_uvarint(&mut out, self.write_counter);
        for (n_merge, lsegs) in &self.levels {
            put_uvarint(&mut out, *n_merge);
            put_uvarint(&mut out, lsegs.len() as u64);
            for seg in lsegs {
                put_uvarint(&mut out, seg.segid);
                put_uvarint(&mut out, seg.pgno_first);
                put_uvarint(&mut out, seg.pgno_last);
                if self.is_v2 {
                    put_uvarint(&mut out, seg.origin1);
                    put_uvarint(&mut out, seg.origin2);
                    put_uvarint(&mut out, seg.n_tombstone_pages);
                }
            }
        }
        out
    }
}

fn decode_full_structure(blob: &[u8]) -> Result<FullStructureRecord> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    if blob.len() < 7 {
        return Err(err_corrupt());
    }
    let cookie = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]);
    let is_v2 = blob.len() >= 11 && &blob[4..8] == b"\xff\x00\x00\x01";
    let mut pos = if is_v2 { 8usize } else { 4usize };
    let n_level = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)? as usize;
    let _n_total_seg = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
    let write_counter = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
    if n_level > 128 {
        return Err(err_corrupt());
    }
    let mut levels: Vec<(u64, Vec<Fts5SegRecord>)> = Vec::with_capacity(n_level);
    for _ in 0..n_level {
        let n_merge = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
        let n_seg = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)? as usize;
        if n_seg > 65_536 {
            return Err(err_corrupt());
        }
        let mut lsegs = Vec::with_capacity(n_seg);
        for _ in 0..n_seg {
            let segid = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
            let pgno_first = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
            let pgno_last = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
            let (origin1, origin2, n_tombstone_pages) = if is_v2 {
                let o1 = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                let o2 = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                let ntp = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                (o1, o2, ntp)
            } else {
                (0, 0, 0)
            };
            lsegs.push(Fts5SegRecord {
                segid,
                pgno_first,
                pgno_last,
                n_tombstone_pages,
                origin1,
                origin2,
            });
        }
        levels.push((n_merge, lsegs));
    }
    if pos != blob.len() {
        return Err(err_corrupt());
    }
    Ok(FullStructureRecord {
        cookie,
        is_v2,
        write_counter,
        levels,
    })
}

fn decode_structure_segments(blob: &[u8]) -> Result<Vec<Fts5SegRecord>> {
    Ok(decode_full_structure(blob)?.chronological_segments())
}

fn load_segment_tombstones(
    ctx: &mut dyn VTabHostContext,
    data_shadow: &str,
    segid: u64,
    n_pages: u64,
) -> Result<BTreeSet<i64>> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let mut out = BTreeSet::new();
    for ipg in 0..n_pages {
        let tid = fts5_tombstone_rowid(segid, ipg);
        let row = ctx.shadow_get_rowid(data_shadow, tid)?;
        let Some(Value::Blob(b)) = row.as_ref().and_then(|r| r.get(1)) else {
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

#[derive(Debug, Clone)]
struct ParsedLeafPage {
    raw: Vec<u8>,
    first_rowid_off: usize,
    leading_end: usize,
    terms: Vec<(Vec<u8>, usize, usize)>,
}

impl ParsedLeafPage {
    fn parse(raw: Vec<u8>, is_first_seg_page: bool) -> Result<Self> {
        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        if raw.len() < 4 {
            return Err(err_corrupt());
        }
        let first_rowid_off = u16::from_be_bytes([raw[0], raw[1]]) as usize;
        let footer_off = u16::from_be_bytes([raw[2], raw[3]]) as usize;
        if footer_off < 4 || footer_off > raw.len() {
            return Err(err_corrupt());
        }
        let mut term_offsets = Vec::new();
        let mut fpos = footer_off;
        let mut acc_off = 0usize;
        while fpos < raw.len() {
            let d = get_uvarint(&raw, &mut fpos).ok_or_else(err_corrupt)? as usize;
            if d == 0 && !term_offsets.is_empty() {
                return Err(err_corrupt());
            }
            acc_off = acc_off.checked_add(d).ok_or_else(err_corrupt)?;
            if acc_off < 4 || acc_off >= footer_off {
                return Err(err_corrupt());
            }
            term_offsets.push(acc_off);
        }
        let leading_end = term_offsets.first().copied().unwrap_or(footer_off);
        if is_first_seg_page && leading_end > 4 {
            return Err(err_corrupt());
        }

        let mut terms = Vec::with_capacity(term_offsets.len());
        let mut prev_page_term: Vec<u8> = Vec::new();
        for (k, &toff) in term_offsets.iter().enumerate() {
            let tend = term_offsets.get(k + 1).copied().unwrap_or(footer_off);
            if toff > tend {
                return Err(err_corrupt());
            }
            let mut pos = toff;
            let term_bytes = if k == 0 {
                let tlen = get_uvarint(&raw, &mut pos).ok_or_else(err_corrupt)? as usize;
                if tlen > tend.saturating_sub(pos) {
                    return Err(err_corrupt());
                }
                let tb = raw[pos..pos + tlen].to_vec();
                pos += tlen;
                tb
            } else {
                let npref = get_uvarint(&raw, &mut pos).ok_or_else(err_corrupt)? as usize;
                let nsuf = get_uvarint(&raw, &mut pos).ok_or_else(err_corrupt)? as usize;
                if npref > prev_page_term.len() || nsuf > tend.saturating_sub(pos) {
                    return Err(err_corrupt());
                }
                let total_len = npref.checked_add(nsuf).ok_or_else(err_corrupt)?;
                let mut tb = Vec::with_capacity(total_len.min(65_536));
                tb.extend_from_slice(&prev_page_term[..npref]);
                tb.extend_from_slice(&raw[pos..pos + nsuf]);
                pos += nsuf;
                tb
            };
            prev_page_term.clone_from(&term_bytes);
            terms.push((term_bytes, pos, tend));
        }

        Ok(Self {
            raw,
            first_rowid_off,
            leading_end,
            terms,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_doclist_slice_on_block<F>(
    block: &[u8],
    first_rowid_off: usize,
    slice_start: usize,
    slice_end: usize,
    continues_to_next_page: bool,
    detail: DetailMode,
    cur_rowid: &mut i64,
    first_in_doclist: &mut bool,
    pending_poslist: &mut Option<(bool, usize, Vec<u8>)>,
    apply_posting: &mut F,
) -> Result<()>
where
    F: FnMut(i64, bool, &[u8]) -> Result<()>,
{
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let mut pos = slice_start;
    if let Some((del, rem, mut pbuf)) = pending_poslist.take() {
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
        apply_posting(*cur_rowid, del, &pbuf)?;
    }

    while pos < slice_end {
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
                apply_posting(*cur_rowid, del, &[])?;
            }
            DetailMode::Col | DetailMode::Full => {
                let sz_flags = get_uvarint(block, &mut pos).ok_or_else(err_corrupt)? as usize;
                let del = (sz_flags & 1) != 0;
                let plen = sz_flags >> 1;
                let avail = slice_end.saturating_sub(pos);
                if plen <= avail {
                    let pbody = &block[pos..pos + plen];
                    pos += plen;
                    apply_posting(*cur_rowid, del, pbody)?;
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
}

/// Merges all active C SQLite 3.54.1 FTS5 segments in chronological order into `MergedIndexMap`.
fn load_merged_index(
    ctx: &mut dyn VTabHostContext,
    _db_q: &str,
    tbl_name: &str,
    detail: DetailMode,
) -> Result<MergedIndexMap> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let data_shadow = format!("{tbl_name}_data");
    let srow = ctx.shadow_get_rowid(&data_shadow, FTS5_STRUCTURE_ROWID)?;
    let Some(Value::Blob(struct_blob)) = srow.as_ref().and_then(|r| r.get(1)) else {
        return Err(err_corrupt());
    };
    let segments = decode_structure_segments(struct_blob)?;
    let mut merged: MergedIndexMap = BTreeMap::new();

    for seg in segments {
        if seg.pgno_first == 0 || seg.pgno_last < seg.pgno_first {
            continue;
        }
        let tombstones = if seg.n_tombstone_pages > 0 {
            load_segment_tombstones(ctx, &data_shadow, seg.segid, seg.n_tombstone_pages)?
        } else {
            BTreeSet::new()
        };

        let mut cur_term: Option<Vec<u8>> = None;
        let mut cur_rowid: i64 = 0;
        let mut first_in_doclist = true;
        let mut pending_poslist: Option<(bool, usize, Vec<u8>)> = None;

        for pg in seg.pgno_first..=seg.pgno_last {
            let lrid = fts5_segment_rowid(seg.segid, pg);
            let lrow = ctx.shadow_get_rowid(&data_shadow, lrid)?;
            let Some(Value::Blob(block_vec)) = lrow.and_then(|mut r| {
                if r.len() >= 2 {
                    Some(r.swap_remove(1))
                } else {
                    None
                }
            }) else {
                return Err(err_corrupt());
            };
            let leaf = ParsedLeafPage::parse(block_vec, pg == seg.pgno_first)?;

            if leaf.leading_end > 4 {
                let Some(tbytes) = cur_term.as_deref() else {
                    return Err(err_corrupt());
                };
                let entry = merged.entry(tbytes.to_vec()).or_default();
                parse_doclist_slice_on_block(
                    &leaf.raw,
                    leaf.first_rowid_off,
                    4,
                    leaf.leading_end,
                    leaf.terms.is_empty() && pg < seg.pgno_last,
                    detail,
                    &mut cur_rowid,
                    &mut first_in_doclist,
                    &mut pending_poslist,
                    &mut |rid, del, body| {
                        if del || tombstones.contains(&rid) {
                            entry.remove(&rid);
                        } else {
                            let hits = decode_poslist_body(detail, body)?;
                            entry.insert(rid, hits);
                        }
                        Ok(())
                    },
                )?;
            }

            for (k, (term_bytes, ds, de)) in leaf.terms.iter().enumerate() {
                if pending_poslist.is_some() {
                    return Err(err_corrupt());
                }
                cur_term = Some(term_bytes.clone());
                first_in_doclist = true;
                let entry = merged.entry(term_bytes.clone()).or_default();
                parse_doclist_slice_on_block(
                    &leaf.raw,
                    leaf.first_rowid_off,
                    *ds,
                    *de,
                    k + 1 == leaf.terms.len() && pg < seg.pgno_last,
                    detail,
                    &mut cur_rowid,
                    &mut first_in_doclist,
                    &mut pending_poslist,
                    &mut |rid, del, body| {
                        if del || tombstones.contains(&rid) {
                            entry.remove(&rid);
                        } else {
                            let hits = decode_poslist_body(detail, body)?;
                            entry.insert(rid, hits);
                        }
                        Ok(())
                    },
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
    col_vals: &[Value],
    deleted: bool,
) -> (DocTermPostings, Vec<u64>) {
    let mut term_cols: HashMap<Vec<u8>, BTreeMap<usize, Vec<usize>>> = HashMap::new();
    let mut col_sizes = vec![0u64; cfg.columns.len()];

    for (col_idx, val) in col_vals.iter().enumerate().take(cfg.columns.len()) {
        if cfg.unindexed[col_idx] || val.is_null() {
            continue;
        }
        let text: std::borrow::Cow<'_, str> = match val {
            Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
            _ => std::borrow::Cow::Owned(val.to_text_lossy()),
        };
        let tokens = cfg.tokenizer.tokenize(&text, false);
        col_sizes[col_idx] = tokens.len() as u64;
        for (tok_pos, tok) in tokens.iter().enumerate() {
            let main_key = make_term_key(0, &tok.term);
            record_posting_hit(
                &mut term_cols,
                main_key,
                col_idx,
                tok_pos,
                cfg.detail,
                deleted,
            );

            for (p_idx, &plen) in cfg.prefixes.iter().enumerate() {
                if tok.term.chars().count() >= plen {
                    let pref: String = tok.term.chars().take(plen).collect();
                    let p_key = make_term_key((p_idx + 1) as u8, &pref);
                    record_posting_hit(
                        &mut term_cols,
                        p_key,
                        col_idx,
                        tok_pos,
                        cfg.detail,
                        deleted,
                    );
                }
            }
        }
    }

    (term_cols.into_iter().collect(), col_sizes)
}

fn record_posting_hit(
    term_cols: &mut HashMap<Vec<u8>, BTreeMap<usize, Vec<usize>>>,
    key: Vec<u8>,
    col_idx: usize,
    tok_pos: usize,
    detail: DetailMode,
    deleted: bool,
) {
    let by_col = term_cols.entry(key).or_default();
    if deleted {
        return;
    }
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

struct TermPosAcc {
    last_col: u32,
    last_off: u32,
    has_any: bool,
    inline_len: u8,
    inline_buf: [u8; 22],
    heap_body: Vec<u8>,
}

impl TermPosAcc {
    #[inline]
    fn new_active() -> Self {
        Self {
            last_col: 0,
            last_off: 0,
            has_any: false,
            inline_len: 1,
            inline_buf: [0u8; 22],
            heap_body: Vec::new(),
        }
    }

    #[inline]
    fn push_bytes(&mut self, data: &[u8]) {
        if self.inline_len > 0 {
            let cur = self.inline_len as usize;
            if cur + data.len() <= 22 {
                self.inline_buf[cur..cur + data.len()].copy_from_slice(data);
                self.inline_len = (cur + data.len()) as u8;
                return;
            }
            self.heap_body.reserve((cur - 1 + data.len()).max(32));
            self.heap_body.extend_from_slice(&self.inline_buf[1..cur]);
            self.heap_body.extend_from_slice(data);
            self.inline_len = 0;
        } else {
            self.heap_body.extend_from_slice(data);
        }
    }

    #[inline]
    fn push_byte(&mut self, b: u8) {
        if self.inline_len > 0 && (self.inline_len as usize) < 22 {
            self.inline_buf[self.inline_len as usize] = b;
            self.inline_len += 1;
        } else {
            self.push_bytes(&[b]);
        }
    }

    #[inline]
    fn push_uvarint(&mut self, v: u64) {
        if v < 0x80 && self.inline_len > 0 && (self.inline_len as usize) < 22 {
            self.inline_buf[self.inline_len as usize] = v as u8;
            self.inline_len += 1;
            return;
        }
        let mut tmp = [0u8; 9];
        let n = put_varint(&mut tmp, v);
        self.push_bytes(&tmp[..n]);
    }

    #[inline]
    fn record(&mut self, col_idx: u32, tok_pos: u32, detail: DetailMode, deleted: bool) {
        if deleted || detail == DetailMode::None {
            return;
        }
        match detail {
            DetailMode::Col => {
                if !self.has_any {
                    self.push_uvarint(u64::from(col_idx) + 2);
                    self.last_col = col_idx;
                    self.has_any = true;
                } else if col_idx > self.last_col {
                    self.push_uvarint(u64::from(col_idx - self.last_col) + 2);
                    self.last_col = col_idx;
                }
            }
            DetailMode::Full => {
                if !self.has_any {
                    if col_idx > 0 {
                        self.push_byte(0x01);
                        self.push_uvarint(u64::from(col_idx));
                    }
                    self.push_uvarint(u64::from(tok_pos) + 2);
                    self.last_col = col_idx;
                    self.last_off = tok_pos;
                    self.has_any = true;
                } else if col_idx == self.last_col {
                    self.push_uvarint(u64::from(tok_pos.saturating_sub(self.last_off)) + 2);
                    self.last_off = tok_pos;
                } else {
                    self.push_byte(0x01);
                    self.push_uvarint(u64::from(col_idx));
                    self.push_uvarint(u64::from(tok_pos) + 2);
                    self.last_col = col_idx;
                    self.last_off = tok_pos;
                }
            }
            DetailMode::None => {}
        }
    }

    #[inline]
    fn finish(mut self, detail: DetailMode, deleted: bool) -> SmallBytes22 {
        if deleted {
            return match detail {
                DetailMode::None => SmallBytes22::from_slice(&[0x00]),
                DetailMode::Col | DetailMode::Full => SmallBytes22::from_slice(&[0x01]),
            };
        }
        if detail == DetailMode::None {
            return SmallBytes22::empty();
        }
        if self.inline_len > 0 {
            let body_len = (self.inline_len - 1) as usize;
            self.inline_buf[0] = (body_len as u8) << 1;
            SmallBytes22::Inline {
                len: self.inline_len,
                buf: self.inline_buf,
            }
        } else {
            let mut out = Vec::with_capacity(self.heap_body.len() + 3);
            put_uvarint(&mut out, (self.heap_body.len() as u64) << 1);
            out.extend_from_slice(&self.heap_body);
            SmallBytes22::Heap(out.into_boxed_slice())
        }
    }
}

fn build_doc_term_encoded_postings(
    cfg: &Fts5Config,
    col_vals: &[Value],
    deleted: bool,
) -> (EncodedDocTerms, Vec<u64>) {
    let mut term_acc: HashMap<SmallBytes22, TermPosAcc> = HashMap::with_capacity(16);
    let mut col_sizes = vec![0u64; cfg.columns.len()];
    let mut key_buf: Vec<u8> = Vec::with_capacity(32);

    let hit_key = |term_acc: &mut HashMap<SmallBytes22, TermPosAcc>,
                   key_slice: &[u8],
                   col_u32: u32,
                   tok_u32: u32| {
        if let Some(acc) = term_acc.get_mut(key_slice) {
            acc.record(col_u32, tok_u32, cfg.detail, deleted);
        } else {
            let mut acc = TermPosAcc::new_active();
            acc.record(col_u32, tok_u32, cfg.detail, deleted);
            term_acc.insert(SmallBytes22::from_slice(key_slice), acc);
        }
    };

    for (col_idx, val) in col_vals.iter().enumerate().take(cfg.columns.len()) {
        if cfg.unindexed[col_idx] || val.is_null() {
            continue;
        }
        let text: std::borrow::Cow<'_, str> = match val {
            Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
            _ => std::borrow::Cow::Owned(val.to_text_lossy()),
        };
        let col_u32 = col_idx as u32;
        let mut tok_pos = 0u32;
        cfg.tokenizer.for_each_doc_token(&text, |term| {
            key_buf.clear();
            key_buf.push(b'0');
            key_buf.extend_from_slice(term.as_bytes());
            hit_key(&mut term_acc, &key_buf, col_u32, tok_pos);

            if !cfg.prefixes.is_empty() {
                for (p_idx, &plen) in cfg.prefixes.iter().enumerate() {
                    if term.is_ascii() {
                        if term.len() >= plen {
                            key_buf.clear();
                            key_buf.push(b'0'.wrapping_add((p_idx + 1) as u8));
                            key_buf.extend_from_slice(&term.as_bytes()[..plen]);
                            hit_key(&mut term_acc, &key_buf, col_u32, tok_pos);
                        }
                    } else if let Some((byte_end, _)) = term.char_indices().nth(plen - 1) {
                        let ch_len = term[byte_end..].chars().next().map_or(0, char::len_utf8);
                        key_buf.clear();
                        key_buf.push(b'0'.wrapping_add((p_idx + 1) as u8));
                        key_buf.extend_from_slice(&term.as_bytes()[..byte_end + ch_len]);
                        hit_key(&mut term_acc, &key_buf, col_u32, tok_pos);
                    }
                }
            }
            tok_pos += 1;
        });
        col_sizes[col_idx] = u64::from(tok_pos);
    }

    let encoded: EncodedDocTerms = term_acc
        .into_iter()
        .map(|(k, acc)| (k, acc.finish(cfg.detail, deleted)))
        .collect();
    (encoded, col_sizes)
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
        if Self::expr_height(&expr) > FTS5_MAX_EXPR_DEPTH {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("fts5 expression tree is too large (maximum depth {FTS5_MAX_EXPR_DEPTH})"),
            ));
        }
        Ok(expr)
    }

    fn expr_height(expr: &FtsQueryExpr) -> usize {
        match expr {
            FtsQueryExpr::Phrase(_) | FtsQueryExpr::Near { .. } => 0,
            FtsQueryExpr::ColFilter { inner, .. } => Self::expr_height(inner),
            FtsQueryExpr::Not(lhs, rhs) => 1 + Self::expr_height(lhs).max(Self::expr_height(rhs)),
            FtsQueryExpr::And(lhs, rhs) => {
                fn max_and_child(e: &FtsQueryExpr) -> usize {
                    match e {
                        FtsQueryExpr::And(l, r) => max_and_child(l).max(max_and_child(r)),
                        other => QueryParser::expr_height(other),
                    }
                }
                1 + max_and_child(lhs).max(max_and_child(rhs))
            }
            FtsQueryExpr::Or(lhs, rhs) => {
                fn max_or_child(e: &FtsQueryExpr) -> usize {
                    match e {
                        FtsQueryExpr::Or(l, r) => max_or_child(l).max(max_or_child(r)),
                        other => QueryParser::expr_height(other),
                    }
                }
                1 + max_or_child(lhs).max(max_or_child(rhs))
            }
        }
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
        if depth > FTS5_MAX_EXPR_DEPTH * 2 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("fts5 expression tree is too large (maximum depth {FTS5_MAX_EXPR_DEPTH})"),
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

/// Per-document match list sorted strictly ascending by `rowid`: `(rowid, PhraseHits)`.
type EvalDocSet = Vec<(i64, PhraseHits)>;
type TermDocHitsMap = BTreeMap<i64, BTreeMap<usize, Vec<usize>>>;

#[derive(Debug, Clone)]
enum SmallOffsets {
    Inline { len: u8, vals: [u32; 4] },
    Heap(Vec<u32>),
}

impl Default for SmallOffsets {
    fn default() -> Self {
        Self::new()
    }
}

impl SmallOffsets {
    #[inline]
    const fn new() -> Self {
        Self::Inline {
            len: 0,
            vals: [0u32; 4],
        }
    }

    #[inline]
    fn from_usize_vec(v: Vec<usize>) -> Self {
        if v.len() <= 4 {
            let mut vals = [0u32; 4];
            for (i, &x) in v.iter().enumerate() {
                vals[i] = x as u32;
            }
            Self::Inline {
                len: v.len() as u8,
                vals,
            }
        } else {
            Self::Heap(v.into_iter().map(|x| x as u32).collect())
        }
    }

    #[inline]
    fn as_slice(&self) -> &[u32] {
        match self {
            Self::Inline { len, vals } => &vals[..(*len as usize)],
            Self::Heap(v) => v.as_slice(),
        }
    }

    #[inline]
    fn push(&mut self, val: u32) {
        match self {
            Self::Inline { len, vals } => {
                let n = *len as usize;
                if n < 4 {
                    vals[n] = val;
                    *len = (n + 1) as u8;
                } else {
                    let mut v = Vec::with_capacity(8);
                    v.extend_from_slice(vals);
                    v.push(val);
                    *self = Self::Heap(v);
                }
            }
            Self::Heap(v) => v.push(val),
        }
    }
}

#[derive(Debug, Clone)]
enum DocColHits {
    Inline {
        len: u8,
        cols: [(u32, SmallOffsets); 2],
    },
    Heap(Vec<(u32, SmallOffsets)>),
}

impl DocColHits {
    #[inline]
    const fn new() -> Self {
        Self::Inline {
            len: 0,
            cols: [(0, SmallOffsets::new()), (0, SmallOffsets::new())],
        }
    }

    #[inline]
    fn as_slice(&self) -> &[(u32, SmallOffsets)] {
        match self {
            Self::Inline { len, cols } => &cols[..(*len as usize)],
            Self::Heap(v) => v.as_slice(),
        }
    }

    #[inline]
    fn ensure_col(&mut self, col_idx: u32) -> &mut SmallOffsets {
        if let Self::Inline { len, cols } = self
            && *len == 2
            && cols[0].0 != col_idx
            && cols[1].0 != col_idx
        {
            let mut v = Vec::with_capacity(4);
            v.push(std::mem::take(&mut cols[0]));
            v.push(std::mem::take(&mut cols[1]));
            *self = Self::Heap(v);
        }
        match self {
            Self::Inline { len, cols } => {
                let n = *len as usize;
                if n > 0 && cols[n - 1].0 == col_idx {
                    return &mut cols[n - 1].1;
                }
                if n > 1 && cols[0].0 == col_idx {
                    return &mut cols[0].1;
                }
                cols[n] = (col_idx, SmallOffsets::new());
                *len = (n + 1) as u8;
                &mut cols[n].1
            }
            Self::Heap(v) => {
                let existing = if v.last().is_some_and(|x| x.0 == col_idx) {
                    Some(v.len() - 1)
                } else {
                    v.iter().position(|x| x.0 == col_idx)
                };
                if let Some(idx) = existing {
                    return &mut v[idx].1;
                }
                v.push((col_idx, SmallOffsets::new()));
                &mut v.last_mut().unwrap().1
            }
        }
    }

    fn sort_cols_if_needed(&mut self) {
        match self {
            Self::Inline { len, cols } => {
                if *len == 2 && cols[0].0 > cols[1].0 {
                    cols.swap(0, 1);
                }
            }
            Self::Heap(v) => {
                if v.windows(2).any(|w| w[0].0 > w[1].0) {
                    v.sort_unstable_by_key(|x| x.0);
                }
            }
        }
    }
}

/// Flat sorted postings for a single term: `(rowid, DocColHits)`.
type TermPostings = Vec<(i64, DocColHits)>;

fn decode_poslist_body_compact(detail: DetailMode, body: &[u8]) -> Result<DocColHits> {
    let err_corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
    let mut out = DocColHits::new();
    match detail {
        DetailMode::None => {
            let _ = out.ensure_col(0);
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
                let _ = out.ensure_col(cur_col as u32);
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
                out.ensure_col(cur_col as u32).push(cur_off as u32);
            }
        }
    }
    out.sort_cols_if_needed();
    Ok(out)
}

fn flatten_term_doc_hits(map: TermDocHitsMap) -> TermPostings {
    map.into_iter()
        .map(|(rid, cols)| {
            let mut dch = DocColHits::new();
            for (c, offs) in cols {
                *dch.ensure_col(c as u32) = SmallOffsets::from_usize_vec(offs);
            }
            (rid, dch)
        })
        .collect()
}

fn eval_phrase_from_token_hits(
    phrase_idx: usize,
    phrase: &QueryPhrase,
    cfg: &Fts5Config,
    token_hits: &[Arc<TermPostings>],
) -> EvalDocSet {
    if phrase.tokens.is_empty() || token_hits.is_empty() {
        return Vec::new();
    }

    let first_docs = token_hits[0].as_slice();
    let mut result = Vec::with_capacity(first_docs.len());
    let pidx_u32 = phrase_idx as u32;

    if phrase.tokens.len() == 1 && !phrase.anchored_start {
        for &(rowid, ref first_cols) in first_docs {
            let mut instances = PhraseHits::new();
            for &(col_idx, ref first_offs) in first_cols.as_slice() {
                if cfg.detail == DetailMode::Full {
                    for &start_off in first_offs.as_slice() {
                        instances.push(PhraseInstance {
                            phrase_idx: pidx_u32,
                            col_idx,
                            token_offset: start_off,
                        });
                    }
                } else {
                    instances.push(PhraseInstance {
                        phrase_idx: pidx_u32,
                        col_idx,
                        token_offset: 0,
                    });
                }
            }
            if !instances.is_empty() {
                result.push((rowid, instances));
            }
        }
        return result;
    }

    let mut cursors = vec![0usize; token_hits.len()];

    'doc_loop: for &(rowid, ref first_cols) in first_docs {
        for (ti, other) in token_hits.iter().enumerate().skip(1) {
            let slice = other.as_slice();
            let cur = &mut cursors[ti];
            while *cur < slice.len() && slice[*cur].0 < rowid {
                *cur += 1;
            }
            if *cur >= slice.len() {
                break 'doc_loop;
            }
            if slice[*cur].0 != rowid {
                continue 'doc_loop;
            }
        }

        let mut instances = PhraseHits::new();
        for &(col_idx, ref first_offs) in first_cols.as_slice() {
            if cfg.detail == DetailMode::Full {
                for &start_off in first_offs.as_slice() {
                    if phrase.anchored_start && start_off != 0 {
                        continue;
                    }
                    let mut all_present = true;
                    for (delta, other) in token_hits.iter().enumerate().skip(1) {
                        let ocols = other[cursors[delta]].1.as_slice();
                        let target_off = start_off.saturating_add(delta as u32);
                        let has = ocols
                            .binary_search_by_key(&col_idx, |&(c, _)| c)
                            .ok()
                            .is_some_and(|ci| {
                                ocols[ci].1.as_slice().binary_search(&target_off).is_ok()
                            });
                        if !has {
                            all_present = false;
                            break;
                        }
                    }
                    if all_present {
                        instances.push(PhraseInstance {
                            phrase_idx: pidx_u32,
                            col_idx,
                            token_offset: start_off,
                        });
                    }
                }
            } else {
                let mut all_present = !phrase.anchored_start && phrase.tokens.len() == 1;
                if phrase.tokens.len() > 1 {
                    all_present = token_hits.iter().enumerate().skip(1).all(|(ti, other)| {
                        other[cursors[ti]]
                            .1
                            .as_slice()
                            .binary_search_by_key(&col_idx, |&(c, _)| c)
                            .is_ok()
                    });
                }
                if all_present {
                    instances.push(PhraseInstance {
                        phrase_idx: pidx_u32,
                        col_idx,
                        token_offset: 0,
                    });
                }
            }
        }

        if !instances.is_empty() {
            result.push((rowid, instances));
        }
    }

    result
}

fn eval_query_expr_precomputed(
    expr: &FtsQueryExpr,
    phrases: &[QueryPhrase],
    cfg: &Fts5Config,
    precomputed_phrases: &mut [EvalDocSet],
    col_mask: Option<&BTreeSet<usize>>,
) -> EvalDocSet {
    match expr {
        FtsQueryExpr::Phrase(pidx) => {
            let mut docs = precomputed_phrases
                .get_mut(*pidx)
                .map(std::mem::take)
                .unwrap_or_default();
            if let Some(allowed) = col_mask {
                docs.retain_mut(|(_, insts)| {
                    insts.retain(|inst| allowed.contains(&(inst.col_idx as usize)));
                    !insts.is_empty()
                });
            }
            docs
        }
        FtsQueryExpr::Near {
            phrase_indices,
            distance,
        } => {
            let mut per_phrase: Vec<EvalDocSet> = Vec::with_capacity(phrase_indices.len());
            for &pi in phrase_indices {
                let Some(pdocs) = precomputed_phrases.get_mut(pi).map(std::mem::take) else {
                    return Vec::new();
                };
                if pdocs.is_empty() {
                    return Vec::new();
                }
                per_phrase.push(pdocs);
            }
            let Some((first, rest)) = per_phrase.split_first() else {
                return Vec::new();
            };
            let mut out = EvalDocSet::new();
            let mut cursors = vec![0usize; rest.len()];
            'near_doc: for &(rowid, ref first_insts) in first {
                for (ri, pm) in rest.iter().enumerate() {
                    let cur = &mut cursors[ri];
                    while *cur < pm.len() && pm[*cur].0 < rowid {
                        *cur += 1;
                    }
                    if *cur >= pm.len() {
                        break 'near_doc;
                    }
                    if pm[*cur].0 != rowid {
                        continue 'near_doc;
                    }
                }
                let mut combined_valid = PhraseHits::new();
                for c in 0..cfg.columns.len() {
                    if let Some(allowed) = col_mask
                        && !allowed.contains(&c)
                    {
                        continue;
                    }
                    let c_u32 = c as u32;
                    let mut groups: Vec<Vec<(PhraseInstance, usize)>> =
                        Vec::with_capacity(1 + rest.len());
                    let g0: Vec<(PhraseInstance, usize)> = first_insts
                        .as_slice()
                        .iter()
                        .filter(|inst| inst.col_idx == c_u32)
                        .map(|&inst| {
                            let plen = phrases[inst.phrase_idx as usize].tokens.len().max(1);
                            (inst, plen)
                        })
                        .collect();
                    if g0.is_empty() {
                        continue;
                    }
                    groups.push(g0);
                    let mut missing_col = false;
                    for (ri, pm) in rest.iter().enumerate() {
                        let gk: Vec<(PhraseInstance, usize)> = pm[cursors[ri]]
                            .1
                            .as_slice()
                            .iter()
                            .filter(|inst| inst.col_idx == c_u32)
                            .map(|&inst| {
                                let plen = phrases[inst.phrase_idx as usize].tokens.len().max(1);
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
                    combined_valid.dedup_sorted();
                    out.push((rowid, combined_valid));
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
            eval_query_expr_precomputed(
                inner,
                phrases,
                cfg,
                precomputed_phrases,
                Some(&next_allowed),
            )
        }
        FtsQueryExpr::And(lhs, rhs) => {
            let l = eval_query_expr_precomputed(lhs, phrases, cfg, precomputed_phrases, col_mask);
            if l.is_empty() {
                return Vec::new();
            }
            let r = eval_query_expr_precomputed(rhs, phrases, cfg, precomputed_phrases, col_mask);
            if r.is_empty() {
                return Vec::new();
            }
            let mut out = Vec::with_capacity(l.len().min(r.len()));
            let mut l_iter = l.into_iter().peekable();
            let mut r_iter = r.into_iter().peekable();
            while let (Some(&(l_rid, _)), Some(&(r_rid, _))) = (l_iter.peek(), r_iter.peek()) {
                match l_rid.cmp(&r_rid) {
                    std::cmp::Ordering::Less => {
                        l_iter.next();
                    }
                    std::cmp::Ordering::Greater => {
                        r_iter.next();
                    }
                    std::cmp::Ordering::Equal => {
                        let (rid, mut l_insts) = l_iter.next().unwrap();
                        let (_, r_insts) = r_iter.next().unwrap();
                        l_insts.extend_from(&r_insts);
                        l_insts.dedup_sorted();
                        out.push((rid, l_insts));
                    }
                }
            }
            out
        }
        FtsQueryExpr::Or(lhs, rhs) => {
            let l = eval_query_expr_precomputed(lhs, phrases, cfg, precomputed_phrases, col_mask);
            let r = eval_query_expr_precomputed(rhs, phrases, cfg, precomputed_phrases, col_mask);
            if r.is_empty() {
                return l;
            }
            if l.is_empty() {
                return r;
            }
            let mut out = Vec::with_capacity(l.len() + r.len());
            let mut l_iter = l.into_iter().peekable();
            let mut r_iter = r.into_iter().peekable();
            loop {
                match (l_iter.peek(), r_iter.peek()) {
                    (Some(&(l_rid, _)), Some(&(r_rid, _))) => match l_rid.cmp(&r_rid) {
                        std::cmp::Ordering::Less => out.push(l_iter.next().unwrap()),
                        std::cmp::Ordering::Greater => out.push(r_iter.next().unwrap()),
                        std::cmp::Ordering::Equal => {
                            let (rid, mut l_insts) = l_iter.next().unwrap();
                            let (_, r_insts) = r_iter.next().unwrap();
                            l_insts.extend_from(&r_insts);
                            l_insts.dedup_sorted();
                            out.push((rid, l_insts));
                        }
                    },
                    (Some(_), None) => {
                        out.extend(l_iter);
                        break;
                    }
                    (None, Some(_)) => {
                        out.extend(r_iter);
                        break;
                    }
                    (None, None) => break,
                }
            }
            out
        }
        FtsQueryExpr::Not(lhs, rhs) => {
            let mut l =
                eval_query_expr_precomputed(lhs, phrases, cfg, precomputed_phrases, col_mask);
            if l.is_empty() {
                return l;
            }
            let r = eval_query_expr_precomputed(rhs, phrases, cfg, precomputed_phrases, col_mask);
            if r.is_empty() {
                return l;
            }
            let mut ri = 0usize;
            l.retain(|&(l_rid, _)| {
                while ri < r.len() && r[ri].0 < l_rid {
                    ri += 1;
                }
                ri >= r.len() || r[ri].0 != l_rid
            });
            l
        }
    }
}

fn collect_near_matches(
    groups: &[Vec<(PhraseInstance, usize)>],
    distance: usize,
    out: &mut PhraseHits,
) {
    fn dfs(
        idx: usize,
        groups: &[Vec<(PhraseInstance, usize)>],
        chosen: &mut Vec<(PhraseInstance, usize)>,
        distance: usize,
        out: &mut PhraseHits,
    ) {
        if idx == groups.len() {
            // In FTS5 NEAR(p1 p2 ..., N), sort by start offset and verify total intervening tokens
            // `(max_start - min_end) <= distance` (and non-overlapping or within span).
            let mut intervals: Vec<(usize, usize)> = chosen
                .iter()
                .map(|(inst, plen)| {
                    let s = inst.token_offset as usize;
                    (s, s + *plen)
                })
                .collect();
            intervals.sort_by_key(|&(s, e)| (s, e));
            let min_end = intervals[0].1;
            let max_start = intervals[intervals.len() - 1].0;
            let gap = max_start.saturating_sub(min_end);
            if gap <= distance {
                for &(inst, _) in chosen.iter() {
                    out.push(inst);
                }
            }
            return;
        }
        for &cand in &groups[idx] {
            chosen.push(cand);
            dfs(idx + 1, groups, chosen, distance, out);
            chosen.pop();
        }
    }
    let mut chosen = Vec::with_capacity(groups.len());
    dfs(0, groups, &mut chosen, distance, out);
}

// ============================================================================
// `Fts5Module`, `Fts5SharedState`, & `Fts5Table`
// ============================================================================

const FTS5_PENDING_MAX_BYTES: usize = 4 * 1024 * 1024;
const FTS5_PENDING_MAX_OPS: usize = 16_384;
const FTS5_MAX_SEGMENTS_BEFORE_COMPACT: usize = 15;

#[derive(Debug, Clone)]
struct Fts5ShadowNames {
    table_name: String,
    data: String,
    idx: String,
    content: String,
    docsize: String,
    config: String,
}

impl Fts5ShadowNames {
    fn new(tbl_name: &str) -> Self {
        Self {
            table_name: tbl_name.to_owned(),
            data: format!("{tbl_name}_data"),
            idx: format!("{tbl_name}_idx"),
            content: format!("{tbl_name}_content"),
            docsize: format!("{tbl_name}_docsize"),
            config: format!("{tbl_name}_config"),
        }
    }
}

#[derive(Debug, Clone)]
enum PendingOp {
    DocPostings {
        rowid: i64,
        is_delete: bool,
        col_sizes: Vec<u64>,
        doc_terms: EncodedDocTerms,
    },
    ContentlessRowidDelete {
        rowid: i64,
        old_col_sizes: Vec<u64>,
        had_docsize: bool,
    },
}

impl PendingOp {
    fn approx_mem_bytes(&self) -> usize {
        match self {
            Self::DocPostings {
                col_sizes,
                doc_terms,
                ..
            } => {
                let terms_sz: usize = doc_terms.iter().map(|(k, p)| 48 + k.len() + p.len()).sum();
                64 + col_sizes.len() * 8 + terms_sz
            }
            Self::ContentlessRowidDelete { old_col_sizes, .. } => 64 + old_col_sizes.len() * 8,
        }
    }
}

#[derive(Debug, Clone)]
struct PendingSavepoint {
    idx: usize,
    ops_len: usize,
    approx_bytes: usize,
    flushed_since: bool,
    flushed_pre_ops: Option<Vec<PendingOp>>,
}

#[derive(Debug, Default)]
struct Fts5PendingBuffer {
    ops: Vec<PendingOp>,
    approx_bytes: usize,
    savepoints: Vec<PendingSavepoint>,
}

impl Fts5PendingBuffer {
    fn clear(&mut self) {
        self.ops.clear();
        self.approx_bytes = 0;
        self.savepoints.clear();
    }

    fn net_doc_delta(&self) -> i64 {
        let mut d = 0i64;
        for op in &self.ops {
            match op {
                PendingOp::DocPostings { is_delete, .. } => {
                    if *is_delete {
                        d -= 1;
                    } else {
                        d += 1;
                    }
                }
                PendingOp::ContentlessRowidDelete { .. } => {
                    d -= 1;
                }
            }
        }
        d
    }
}

type SegIdxEntries = Vec<(Vec<u8>, u64)>;

const FTS5_EXACT_TERM_CACHE_MAX: usize = 4096;
const FTS5_DENSE_DOCSIZE_MAX: usize = 262_144;

#[derive(Debug, Default)]
struct Fts5ReadCache {
    structure_blob: Option<Vec<u8>>,
    segments: Vec<Fts5SegRecord>,
    averages: Option<Vec<u64>>,
    configured_rank_weights: Option<Vec<f64>>,
    secure_delete: Option<bool>,
    seg_tombstones: HashMap<u64, Arc<BTreeSet<i64>>>,
    seg_idx: HashMap<u64, Arc<SegIdxEntries>>,
    leaf_pages: HashMap<(u64, u64), Arc<ParsedLeafPage>>,
    exact_terms: HashMap<SmallBytes22, Arc<TermPostings>>,
    dense_docsize: Vec<Option<DocColSizes>>,
    docsize_cache: HashMap<i64, DocColSizes>,
}

impl Fts5ReadCache {
    #[inline]
    fn get_docsize(&self, rowid: i64) -> Option<DocColSizes> {
        if rowid > 0 && (rowid as usize) <= FTS5_DENSE_DOCSIZE_MAX {
            let idx = rowid as usize;
            if idx < self.dense_docsize.len() {
                return self.dense_docsize[idx].clone();
            }
            return None;
        }
        self.docsize_cache.get(&rowid).cloned()
    }

    #[inline]
    fn insert_docsize(&mut self, rowid: i64, sz: DocColSizes) {
        if rowid > 0 && (rowid as usize) <= FTS5_DENSE_DOCSIZE_MAX {
            let idx = rowid as usize;
            if idx >= self.dense_docsize.len() {
                self.dense_docsize.resize(
                    (idx + 1)
                        .next_power_of_two()
                        .min(FTS5_DENSE_DOCSIZE_MAX + 1),
                    None,
                );
            }
            self.dense_docsize[idx] = Some(sz);
        } else {
            self.docsize_cache.insert(rowid, sz);
        }
    }

    #[inline]
    fn remove_docsize(&mut self, rowid: i64) {
        if rowid > 0 && (rowid as usize) <= FTS5_DENSE_DOCSIZE_MAX {
            let idx = rowid as usize;
            if idx < self.dense_docsize.len() {
                self.dense_docsize[idx] = None;
            }
        } else {
            self.docsize_cache.remove(&rowid);
        }
    }

    #[inline]
    fn clear_docsize(&mut self) {
        self.dense_docsize.clear();
        self.docsize_cache.clear();
    }

    fn clear_all(&mut self) {
        self.structure_blob = None;
        self.segments.clear();
        self.averages = None;
        self.configured_rank_weights = None;
        self.secure_delete = None;
        self.seg_tombstones.clear();
        self.seg_idx.clear();
        self.leaf_pages.clear();
        self.exact_terms.clear();
        self.clear_docsize();
    }

    fn clear_segment_maps(&mut self) {
        self.structure_blob = None;
        self.segments.clear();
        self.averages = None;
        self.seg_tombstones.clear();
        self.seg_idx.clear();
        self.leaf_pages.clear();
        self.exact_terms.clear();
    }
}

#[derive(Debug)]
struct Fts5SharedState {
    shadows: Fts5ShadowNames,
    in_tx: bool,
    pending: Fts5PendingBuffer,
    read_cache: Fts5ReadCache,
}

impl Fts5SharedState {
    fn new(tbl_name: &str) -> Self {
        Self {
            shadows: Fts5ShadowNames::new(tbl_name),
            in_tx: false,
            pending: Fts5PendingBuffer::default(),
            read_cache: Fts5ReadCache::default(),
        }
    }

    fn ensure_structure_coherent(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if let Some(ref cur_sblob) = self.read_cache.structure_blob
            && ctx.shadow_rowid_blob_eq(&self.shadows.data, FTS5_STRUCTURE_ROWID, 1, cur_sblob)?
                == Some(true)
        {
            return Ok(());
        }
        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let srow = ctx.shadow_get_rowid(&self.shadows.data, FTS5_STRUCTURE_ROWID)?;
        let Some(Value::Blob(sblob)) = srow.and_then(|mut r| {
            if r.len() >= 2 {
                Some(r.swap_remove(1))
            } else {
                None
            }
        }) else {
            return Err(err_corrupt());
        };
        if self.read_cache.structure_blob.as_deref() == Some(sblob.as_slice()) {
            return Ok(());
        }
        let segments = decode_structure_segments(&sblob)?;
        self.read_cache.clear_all();
        self.read_cache.structure_blob = Some(sblob);
        self.read_cache.segments = segments;
        Ok(())
    }

    fn read_averages(
        &mut self,
        cfg: &Fts5Config,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<Vec<u64>> {
        if let Some(ref avgs) = self.read_cache.averages {
            return Ok(avgs.clone());
        }
        let n_expected = 1 + cfg.columns.len();
        let row = ctx.shadow_get_rowid(&self.shadows.data, FTS5_AVERAGES_ROWID)?;
        let avgs = if let Some(Value::Blob(b)) = row.as_ref().and_then(|r| r.get(1))
            && let Some(mut list) = decode_varint_list(b)
        {
            list.resize(n_expected, 0);
            list
        } else {
            vec![0u64; n_expected]
        };
        self.read_cache.averages = Some(avgs.clone());
        Ok(avgs)
    }

    fn write_averages(&mut self, ctx: &mut dyn VTabHostContext, avgs: &[u64]) -> Result<()> {
        ctx.shadow_put_rowid(
            &self.shadows.data,
            FTS5_AVERAGES_ROWID,
            &[
                Value::Integer(FTS5_AVERAGES_ROWID),
                Value::Blob(encode_varint_list(avgs)),
            ],
            true,
        )?;
        self.read_cache.averages = Some(avgs.to_vec());
        Ok(())
    }

    fn is_secure_delete_enabled(&mut self, ctx: &mut dyn VTabHostContext) -> Result<bool> {
        if let Some(sd) = self.read_cache.secure_delete {
            return Ok(sd);
        }
        let db_q = quote_ident(ctx.db_name());
        let config_q = quote_ident(&self.shadows.config);
        let rows = ctx.query_sql(
            &format!("SELECT v FROM {db_q}.{config_q} WHERE k = 'secure-delete'"),
            &[],
        )?;
        let enabled = rows
            .first()
            .and_then(|r| r.first())
            .is_some_and(|v| v.to_integer_lossy() != 0);
        self.read_cache.secure_delete = Some(enabled);
        Ok(enabled)
    }

    fn get_configured_rank_weights(&mut self, ctx: &mut dyn VTabHostContext) -> Result<Vec<f64>> {
        if let Some(ref w) = self.read_cache.configured_rank_weights {
            return Ok(w.clone());
        }
        let db_q = quote_ident(ctx.db_name());
        let config_q = quote_ident(&self.shadows.config);
        let rows = ctx.query_sql(
            &format!("SELECT v FROM {db_q}.{config_q} WHERE k = 'rank'"),
            &[],
        )?;
        let weights = if let Some(Value::Text(s)) = rows.first().and_then(|r| r.first()) {
            parse_rank_expression(s)?
        } else {
            Vec::new()
        };
        self.read_cache.configured_rank_weights = Some(weights.clone());
        Ok(weights)
    }

    fn flush_pending_via_compaction(
        &mut self,
        cfg: &Fts5Config,
        ctx: &mut dyn VTabHostContext,
        ops: Vec<PendingOp>,
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let mut merged = load_merged_index(ctx, &db_q, &self.shadows.table_name, cfg.detail)?;
        let mut avgs = self.read_averages(cfg, ctx)?;

        for op in ops {
            match op {
                PendingOp::DocPostings {
                    rowid,
                    is_delete,
                    col_sizes,
                    doc_terms,
                } => {
                    for (term, poslist) in doc_terms {
                        let entry = merged.entry(term.as_slice().to_vec()).or_default();
                        if is_delete {
                            entry.remove(&rowid);
                        } else {
                            let col_hits =
                                decode_full_poslist_payload(cfg.detail, poslist.as_slice())?;
                            entry.insert(rowid, col_hits);
                        }
                    }
                    if is_delete {
                        avgs[0] = avgs[0].saturating_sub(1);
                        for (i, &sz) in col_sizes.iter().enumerate() {
                            avgs[1 + i] = avgs[1 + i].saturating_sub(sz);
                        }
                    } else {
                        avgs[0] = avgs[0].saturating_add(1);
                        for (i, &sz) in col_sizes.iter().enumerate() {
                            avgs[1 + i] = avgs[1 + i].saturating_add(sz);
                        }
                    }
                }
                PendingOp::ContentlessRowidDelete {
                    rowid,
                    old_col_sizes,
                    had_docsize,
                } => {
                    let mut removed = false;
                    for docs in merged.values_mut() {
                        if docs.remove(&rowid).is_some() {
                            removed = true;
                        }
                    }
                    if removed || had_docsize {
                        avgs[0] = avgs[0].saturating_sub(1);
                        for (i, &sz) in old_col_sizes.iter().enumerate() {
                            avgs[1 + i] = avgs[1 + i].saturating_sub(sz);
                        }
                    }
                }
            }
        }
        merged.retain(|_, docs| !docs.is_empty());
        let sblob = write_c_fts5_index(ctx, &db_q, &self.shadows.table_name, cfg, &merged)?;
        let segs = decode_structure_segments(&sblob)?;
        self.write_averages(ctx, &avgs)?;
        self.read_cache.clear_segment_maps();
        self.read_cache.structure_blob = Some(sblob);
        self.read_cache.segments = segs;
        self.read_cache.averages = Some(avgs);
        Ok(())
    }

    fn flush_pending(&mut self, cfg: &Fts5Config, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if self.pending.ops.is_empty() {
            return Ok(());
        }
        for sp in &mut self.pending.savepoints {
            if sp.flushed_pre_ops.is_none() && sp.ops_len > 0 {
                sp.flushed_pre_ops = Some(self.pending.ops[..sp.ops_len].to_vec());
            }
            sp.flushed_since = true;
        }
        let ops = std::mem::take(&mut self.pending.ops);
        self.pending.approx_bytes = 0;

        let mut has_contentless_rowid_del = false;
        let mut has_any_delete = false;
        for op in &ops {
            match op {
                PendingOp::ContentlessRowidDelete { .. } => {
                    has_contentless_rowid_del = true;
                    has_any_delete = true;
                }
                PendingOp::DocPostings { is_delete, .. } => {
                    if *is_delete {
                        has_any_delete = true;
                    }
                }
            }
        }

        if has_contentless_rowid_del || (has_any_delete && self.is_secure_delete_enabled(ctx)?) {
            return self.flush_pending_via_compaction(cfg, ctx, ops);
        }

        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let srow = ctx.shadow_get_rowid(&self.shadows.data, FTS5_STRUCTURE_ROWID)?;
        let Some(Value::Blob(prev_sblob)) = srow.and_then(|mut r| {
            if r.len() >= 2 {
                Some(r.swap_remove(1))
            } else {
                None
            }
        }) else {
            return Err(err_corrupt());
        };
        let mut full_struct = decode_full_structure(&prev_sblob)?;
        if full_struct.total_segments() >= FTS5_MAX_SEGMENTS_BEFORE_COMPACT {
            return self.flush_pending_via_compaction(cfg, ctx, ops);
        }

        let cache_was_coherent =
            self.read_cache.structure_blob.as_deref() == Some(prev_sblob.as_slice());
        if !cache_was_coherent {
            self.read_cache.clear_segment_maps();
        }

        let mut avgs = self.read_averages(cfg, ctx)?;
        #[allow(clippy::type_complexity)]
        let mut seg_map: HashMap<SmallBytes22, Vec<(i64, bool, SmallBytes22)>> =
            HashMap::with_capacity(ops.len().min(4096));
        for op in ops {
            if let PendingOp::DocPostings {
                rowid,
                is_delete,
                col_sizes,
                doc_terms,
            } = op
            {
                for (term, poslist) in doc_terms {
                    let entry = seg_map.entry(term).or_default();
                    if entry.last().is_none_or(|last| rowid > last.0) {
                        entry.push((rowid, is_delete, poslist));
                    } else {
                        match entry.binary_search_by_key(&rowid, |x| x.0) {
                            Ok(idx) => entry[idx] = (rowid, is_delete, poslist),
                            Err(idx) => entry.insert(idx, (rowid, is_delete, poslist)),
                        }
                    }
                }
                if is_delete {
                    avgs[0] = avgs[0].saturating_sub(1);
                    for (i, &sz) in col_sizes.iter().enumerate() {
                        avgs[1 + i] = avgs[1 + i].saturating_sub(sz);
                    }
                } else {
                    avgs[0] = avgs[0].saturating_add(1);
                    for (i, &sz) in col_sizes.iter().enumerate() {
                        avgs[1 + i] = avgs[1 + i].saturating_add(sz);
                    }
                }
            }
        }

        self.write_averages(ctx, &avgs)?;

        if seg_map.is_empty()
            || (full_struct.total_segments() == 0
                && seg_map
                    .values()
                    .all(|docs| docs.iter().all(|(_, del, _)| *del)))
        {
            if cache_was_coherent {
                self.read_cache.structure_blob = Some(prev_sblob);
            }
            return Ok(());
        }

        let mut sorted_terms: Vec<_> = seg_map.into_iter().collect();
        sorted_terms.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        let new_segid = full_struct.max_segid() + 1;
        let (pgno_last, leaves, idx_rows) =
            encode_segment_pages_bytes(sorted_terms.iter().map(|(t, docs)| {
                (
                    t.as_slice(),
                    docs.iter().map(|(rid, _del, pl)| (*rid, pl.as_slice())),
                )
            }));

        for (leaf_pgno, leaf_bytes) in leaves {
            let lrid = fts5_segment_rowid(new_segid, leaf_pgno);
            ctx.shadow_put_rowid(
                &self.shadows.data,
                lrid,
                &[Value::Integer(lrid), Value::Blob(leaf_bytes)],
                true,
            )?;
        }

        let db_q = quote_ident(ctx.db_name());
        let idx_q = quote_ident(&self.shadows.idx);
        let idx_ins_sql =
            format!("INSERT INTO {db_q}.{idx_q}(segid, term, pgno) VALUES(?1, ?2, ?3)");
        let mut cached_idx_vec = Vec::with_capacity(idx_rows.len());
        for (bt_key, bt_pg_val) in idx_rows {
            let pg_u64 = (bt_pg_val >> 1).max(1) as u64;
            cached_idx_vec.push((bt_key.clone(), pg_u64));
            ctx.execute_sql(
                &idx_ins_sql,
                &[
                    Value::Integer(new_segid as i64),
                    Value::Blob(bt_key),
                    Value::Integer(bt_pg_val),
                ],
            )?;
        }
        self.read_cache
            .seg_idx
            .insert(new_segid, Arc::new(cached_idx_vec));

        let new_rec = Fts5SegRecord {
            segid: new_segid,
            pgno_first: 1,
            pgno_last,
            n_tombstone_pages: 0,
            origin1: 1,
            origin2: 1,
        };
        full_struct.cookie = full_struct.cookie.wrapping_add(1);
        full_struct.write_counter = full_struct.write_counter.saturating_add(pgno_last);
        if full_struct.levels.is_empty() {
            full_struct.levels.push((0, vec![new_rec.clone()]));
        } else {
            full_struct.levels[0].1.push(new_rec.clone());
        }
        let new_sblob = full_struct.encode();
        ctx.shadow_put_rowid(
            &self.shadows.data,
            FTS5_STRUCTURE_ROWID,
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(new_sblob.clone()),
            ],
            true,
        )?;

        if cache_was_coherent {
            self.read_cache.structure_blob = Some(new_sblob);
            self.read_cache.segments.push(new_rec);
        } else {
            self.read_cache.structure_blob = Some(new_sblob);
            self.read_cache.segments = full_struct.chronological_segments();
        }
        self.read_cache.exact_terms.clear();
        self.read_cache.averages = Some(avgs);
        Ok(())
    }

    fn get_or_load_tombstones(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        segid: u64,
        n_pages: u64,
    ) -> Result<Option<Arc<BTreeSet<i64>>>> {
        if n_pages == 0 {
            return Ok(None);
        }
        if let Some(ts) = self.read_cache.seg_tombstones.get(&segid) {
            return Ok(Some(Arc::clone(ts)));
        }
        let ts = Arc::new(load_segment_tombstones(
            ctx,
            &self.shadows.data,
            segid,
            n_pages,
        )?);
        self.read_cache
            .seg_tombstones
            .insert(segid, Arc::clone(&ts));
        Ok(Some(ts))
    }

    fn get_or_load_seg_idx(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        segid: u64,
    ) -> Result<Arc<SegIdxEntries>> {
        if let Some(entries) = self.read_cache.seg_idx.get(&segid) {
            return Ok(Arc::clone(entries));
        }
        let db_q = quote_ident(ctx.db_name());
        let idx_q = quote_ident(&self.shadows.idx);
        let rows = ctx.query_sql(
            &format!("SELECT term, pgno FROM {db_q}.{idx_q} WHERE segid = ?1 ORDER BY term ASC"),
            &[Value::Integer(segid as i64)],
        )?;
        let mut parsed = Vec::with_capacity(rows.len());
        for mut r in rows {
            if r.len() < 2 {
                continue;
            }
            let pg_i64 = r[1].to_integer_lossy();
            let pg_u64 = (pg_i64 >> 1).max(1) as u64;
            let key_bytes = match r.swap_remove(0) {
                Value::Blob(b) => b,
                Value::Text(s) => s.into_bytes(),
                other => other.to_text_lossy().into_bytes(),
            };
            parsed.push((key_bytes, pg_u64));
        }
        let arc = Arc::new(parsed);
        self.read_cache.seg_idx.insert(segid, Arc::clone(&arc));
        Ok(arc)
    }

    fn get_or_load_leaf_page(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        segid: u64,
        pgno: u64,
        is_first_seg_page: bool,
    ) -> Result<Arc<ParsedLeafPage>> {
        if let Some(lp) = self.read_cache.leaf_pages.get(&(segid, pgno)) {
            return Ok(Arc::clone(lp));
        }
        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let lrid = fts5_segment_rowid(segid, pgno);
        let lrow = ctx.shadow_get_rowid(&self.shadows.data, lrid)?;
        let Some(Value::Blob(raw)) = lrow.and_then(|mut r| {
            if r.len() >= 2 {
                Some(r.swap_remove(1))
            } else {
                None
            }
        }) else {
            return Err(err_corrupt());
        };
        let parsed = Arc::new(ParsedLeafPage::parse(raw, is_first_seg_page)?);
        self.read_cache
            .leaf_pages
            .insert((segid, pgno), Arc::clone(&parsed));
        Ok(parsed)
    }

    /// Decodes the doclist of `leaf.terms[term_idx]` starting on `start_pg` (following any
    /// cross-page continuations onto `start_pg + 1 ..= seg.pgno_last`) into `target_docs`,
    /// returning the last pure-continuation page consumed (`>= start_pg`).
    #[allow(clippy::too_many_arguments)]
    fn decode_term_doclist_across_pages(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        detail: DetailMode,
        seg: &Fts5SegRecord,
        tombstones: Option<&BTreeSet<i64>>,
        start_pg: u64,
        leaf: &ParsedLeafPage,
        term_idx: usize,
        target_docs: &mut BTreeMap<i64, BTreeMap<usize, Vec<usize>>>,
    ) -> Result<u64> {
        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let (_, ds, de) = leaf.terms[term_idx];
        let mut cur_rowid = 0i64;
        let mut first_in_doclist = true;
        let mut pending_poslist: Option<(bool, usize, Vec<u8>)> = None;
        let is_last_on_page = term_idx + 1 == leaf.terms.len();

        parse_doclist_slice_on_block(
            &leaf.raw,
            leaf.first_rowid_off,
            ds,
            de,
            is_last_on_page && start_pg < seg.pgno_last,
            detail,
            &mut cur_rowid,
            &mut first_in_doclist,
            &mut pending_poslist,
            &mut |rid, del, body| {
                if del || tombstones.is_some_and(|ts| ts.contains(&rid)) {
                    target_docs.remove(&rid);
                } else {
                    let hits = decode_poslist_body(detail, body)?;
                    target_docs.insert(rid, hits);
                }
                Ok(())
            },
        )?;

        let mut last_pure_cont_pg = start_pg;
        if is_last_on_page && start_pg < seg.pgno_last {
            for cont_pg in (start_pg + 1)..=seg.pgno_last {
                let cont_leaf =
                    self.get_or_load_leaf_page(ctx, seg.segid, cont_pg, cont_pg == seg.pgno_first)?;
                if cont_leaf.leading_end > 4 {
                    parse_doclist_slice_on_block(
                        &cont_leaf.raw,
                        cont_leaf.first_rowid_off,
                        4,
                        cont_leaf.leading_end,
                        cont_leaf.terms.is_empty() && cont_pg < seg.pgno_last,
                        detail,
                        &mut cur_rowid,
                        &mut first_in_doclist,
                        &mut pending_poslist,
                        &mut |rid, del, body| {
                            if del || tombstones.is_some_and(|ts| ts.contains(&rid)) {
                                target_docs.remove(&rid);
                            } else {
                                let hits = decode_poslist_body(detail, body)?;
                                target_docs.insert(rid, hits);
                            }
                            Ok(())
                        },
                    )?;
                }
                if !cont_leaf.terms.is_empty() {
                    if pending_poslist.is_some() {
                        return Err(err_corrupt());
                    }
                    break;
                }
                last_pure_cont_pg = cont_pg;
            }
        }

        if pending_poslist.is_some() {
            return Err(err_corrupt());
        }
        Ok(last_pure_cont_pg)
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_term_doclist_into_vec(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        detail: DetailMode,
        seg: &Fts5SegRecord,
        tombstones: Option<&BTreeSet<i64>>,
        start_pg: u64,
        leaf: &ParsedLeafPage,
        term_idx: usize,
        target_vec: &mut TermPostings,
    ) -> Result<()> {
        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let (_, ds, de) = leaf.terms[term_idx];
        let mut cur_rowid = 0i64;
        let mut first_in_doclist = true;
        let mut pending_poslist: Option<(bool, usize, Vec<u8>)> = None;
        let is_last_on_page = term_idx + 1 == leaf.terms.len();

        let mut push_doc = |rid: i64, del: bool, body: &[u8]| -> Result<()> {
            if del || tombstones.is_some_and(|ts| ts.contains(&rid)) {
                if target_vec.last().is_some_and(|&(r, _)| r == rid) {
                    target_vec.pop();
                } else if let Ok(pos) = target_vec.binary_search_by_key(&rid, |&(r, _)| r) {
                    target_vec.remove(pos);
                }
            } else {
                let hits = decode_poslist_body_compact(detail, body)?;
                if target_vec.last().is_none_or(|&(r, _)| rid > r) {
                    target_vec.push((rid, hits));
                } else {
                    match target_vec.binary_search_by_key(&rid, |&(r, _)| r) {
                        Ok(pos) => target_vec[pos].1 = hits,
                        Err(pos) => target_vec.insert(pos, (rid, hits)),
                    }
                }
            }
            Ok(())
        };

        parse_doclist_slice_on_block(
            &leaf.raw,
            leaf.first_rowid_off,
            ds,
            de,
            is_last_on_page && start_pg < seg.pgno_last,
            detail,
            &mut cur_rowid,
            &mut first_in_doclist,
            &mut pending_poslist,
            &mut push_doc,
        )?;

        if is_last_on_page && start_pg < seg.pgno_last {
            for cont_pg in (start_pg + 1)..=seg.pgno_last {
                let cont_leaf =
                    self.get_or_load_leaf_page(ctx, seg.segid, cont_pg, cont_pg == seg.pgno_first)?;
                if cont_leaf.leading_end > 4 {
                    parse_doclist_slice_on_block(
                        &cont_leaf.raw,
                        cont_leaf.first_rowid_off,
                        4,
                        cont_leaf.leading_end,
                        cont_leaf.terms.is_empty() && cont_pg < seg.pgno_last,
                        detail,
                        &mut cur_rowid,
                        &mut first_in_doclist,
                        &mut pending_poslist,
                        &mut push_doc,
                    )?;
                }
                if !cont_leaf.terms.is_empty() {
                    if pending_poslist.is_some() {
                        return Err(err_corrupt());
                    }
                    break;
                }
            }
        }

        if pending_poslist.is_some() {
            return Err(err_corrupt());
        }
        Ok(())
    }

    fn lookup_exact_term(
        &mut self,
        cfg: &Fts5Config,
        ctx: &mut dyn VTabHostContext,
        target_key: &[u8],
    ) -> Result<Arc<TermPostings>> {
        if let Some(cached) = self.read_cache.exact_terms.get(target_key) {
            return Ok(Arc::clone(cached));
        }
        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let segments = self.read_cache.segments.clone();

        let postings = if segments.len() == 1 {
            let seg = &segments[0];
            let mut vec_out = TermPostings::new();
            if seg.pgno_first > 0 && seg.pgno_last >= seg.pgno_first {
                let tombstones =
                    self.get_or_load_tombstones(ctx, seg.segid, seg.n_tombstone_pages)?;
                let idx_entries = self.get_or_load_seg_idx(ctx, seg.segid)?;
                let pos = idx_entries.partition_point(|(k, _)| k.as_slice() <= target_key);
                let start_pg = if pos > 0 {
                    idx_entries[pos - 1].1.max(seg.pgno_first)
                } else {
                    seg.pgno_first
                };
                if start_pg > seg.pgno_last {
                    return Err(err_corrupt());
                }
                'single_seg: for pg in start_pg..=seg.pgno_last {
                    let leaf =
                        self.get_or_load_leaf_page(ctx, seg.segid, pg, pg == seg.pgno_first)?;
                    for (k, (t, _, _)) in leaf.terms.iter().enumerate() {
                        match t.as_slice().cmp(target_key) {
                            std::cmp::Ordering::Less => {}
                            std::cmp::Ordering::Greater => break 'single_seg,
                            std::cmp::Ordering::Equal => {
                                self.decode_term_doclist_into_vec(
                                    ctx,
                                    cfg.detail,
                                    seg,
                                    tombstones.as_deref(),
                                    pg,
                                    &leaf,
                                    k,
                                    &mut vec_out,
                                )?;
                                break 'single_seg;
                            }
                        }
                    }
                }
            }
            vec_out
        } else {
            let mut out: TermDocHitsMap = BTreeMap::new();
            for seg in &segments {
                if seg.pgno_first == 0 || seg.pgno_last < seg.pgno_first {
                    continue;
                }
                let tombstones =
                    self.get_or_load_tombstones(ctx, seg.segid, seg.n_tombstone_pages)?;
                let idx_entries = self.get_or_load_seg_idx(ctx, seg.segid)?;
                let pos = idx_entries.partition_point(|(k, _)| k.as_slice() <= target_key);
                let start_pg = if pos > 0 {
                    idx_entries[pos - 1].1.max(seg.pgno_first)
                } else {
                    seg.pgno_first
                };
                if start_pg > seg.pgno_last {
                    return Err(err_corrupt());
                }

                'seg_pages: for pg in start_pg..=seg.pgno_last {
                    let leaf =
                        self.get_or_load_leaf_page(ctx, seg.segid, pg, pg == seg.pgno_first)?;
                    for (k, (t, _, _)) in leaf.terms.iter().enumerate() {
                        match t.as_slice().cmp(target_key) {
                            std::cmp::Ordering::Less => {}
                            std::cmp::Ordering::Greater => break 'seg_pages,
                            std::cmp::Ordering::Equal => {
                                let _ = self.decode_term_doclist_across_pages(
                                    ctx,
                                    cfg.detail,
                                    seg,
                                    tombstones.as_deref(),
                                    pg,
                                    &leaf,
                                    k,
                                    &mut out,
                                )?;
                                break 'seg_pages;
                            }
                        }
                    }
                }
            }
            flatten_term_doc_hits(out)
        };

        let arc = Arc::new(postings);
        if self.read_cache.exact_terms.len() >= FTS5_EXACT_TERM_CACHE_MAX {
            self.read_cache.exact_terms.clear();
        }
        self.read_cache
            .exact_terms
            .insert(SmallBytes22::from_slice(target_key), Arc::clone(&arc));
        Ok(arc)
    }

    fn lookup_prefix_terms(
        &mut self,
        cfg: &Fts5Config,
        ctx: &mut dyn VTabHostContext,
        prefix_key: &[u8],
    ) -> Result<Arc<TermPostings>> {
        let err_corrupt =
            || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let segments = self.read_cache.segments.clone();
        let mut per_term: MergedIndexMap = BTreeMap::new();

        for seg in &segments {
            if seg.pgno_first == 0 || seg.pgno_last < seg.pgno_first {
                continue;
            }
            let tombstones = self.get_or_load_tombstones(ctx, seg.segid, seg.n_tombstone_pages)?;
            let idx_entries = self.get_or_load_seg_idx(ctx, seg.segid)?;
            let pos = idx_entries.partition_point(|(k, _)| k.as_slice() <= prefix_key);
            let mut pg = if pos > 0 {
                idx_entries[pos - 1].1.max(seg.pgno_first)
            } else {
                seg.pgno_first
            };
            if pg > seg.pgno_last {
                return Err(err_corrupt());
            }

            'seg_scan: while pg <= seg.pgno_last {
                let leaf = self.get_or_load_leaf_page(ctx, seg.segid, pg, pg == seg.pgno_first)?;
                let mut jump_to_pg = pg;
                for (k, (t, _, _)) in leaf.terms.iter().enumerate() {
                    if t.as_slice() < prefix_key {
                        continue;
                    }
                    if !t.starts_with(prefix_key) {
                        break 'seg_scan;
                    }
                    let entry = per_term.entry(t.clone()).or_default();
                    let last_cont = self.decode_term_doclist_across_pages(
                        ctx,
                        cfg.detail,
                        seg,
                        tombstones.as_deref(),
                        pg,
                        &leaf,
                        k,
                        entry,
                    )?;
                    if last_cont > jump_to_pg {
                        jump_to_pg = last_cont;
                    }
                }
                pg = jump_to_pg + 1;
            }
        }

        let mut combined: TermDocHitsMap = BTreeMap::new();
        for (_t, docs) in per_term {
            for (rid, cols) in docs {
                let dcols = combined.entry(rid).or_default();
                for (cidx, offs) in cols {
                    dcols.entry(cidx).or_default().extend(offs);
                }
            }
        }
        for cols in combined.values_mut() {
            for offs in cols.values_mut() {
                offs.sort_unstable();
                offs.dedup();
            }
        }
        Ok(Arc::new(flatten_term_doc_hits(combined)))
    }

    fn eval_phrase_selective(
        &mut self,
        cfg: &Fts5Config,
        ctx: &mut dyn VTabHostContext,
        phrase_idx: usize,
        phrase: &QueryPhrase,
    ) -> Result<EvalDocSet> {
        if phrase.tokens.is_empty() {
            return Ok(Vec::new());
        }
        let mut token_hits: Vec<Arc<TermPostings>> = Vec::with_capacity(phrase.tokens.len());
        for ptok in &phrase.tokens {
            let doc_map = if ptok.is_prefix {
                let char_len = ptok.term.chars().count();
                if let Some(p_pos) = cfg.prefixes.iter().position(|&n| n == char_len) {
                    let p_key = make_term_key((p_pos + 1) as u8, &ptok.term);
                    self.lookup_exact_term(cfg, ctx, &p_key)?
                } else {
                    let start_key = make_term_key(0, &ptok.term);
                    self.lookup_prefix_terms(cfg, ctx, &start_key)?
                }
            } else {
                let exact_key = make_term_key(0, &ptok.term);
                self.lookup_exact_term(cfg, ctx, &exact_key)?
            };
            if doc_map.is_empty() {
                return Ok(Vec::new());
            }
            token_hits.push(doc_map);
        }
        Ok(eval_phrase_from_token_hits(
            phrase_idx,
            phrase,
            cfg,
            &token_hits,
        ))
    }

    fn get_or_load_docsize(
        &mut self,
        cfg: &Fts5Config,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<DocColSizes>> {
        if !cfg.columnsize {
            return Ok(None);
        }
        if let Some(sz) = self.read_cache.get_docsize(rowid) {
            return Ok(Some(sz));
        }
        if let Some(row) = ctx.shadow_get_rowid(&self.shadows.docsize, rowid)?
            && let Some(Value::Blob(b)) = row.get(1)
            && let Some(list) = decode_varint_list(b)
        {
            let mut out: Vec<usize> = list.into_iter().map(|x| x as usize).collect();
            out.resize(cfg.columns.len(), 0);
            let doc_sz = DocColSizes::from_usize_vec(out);
            self.read_cache.insert_docsize(rowid, doc_sz.clone());
            return Ok(Some(doc_sz));
        }
        Ok(None)
    }
}

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
        let empty_sblob = encode_empty_structure_record(cfg.contentless_delete);
        let data_shadow = format!("{tbl_name}_data");
        ctx.shadow_put_rowid(
            &data_shadow,
            FTS5_AVERAGES_ROWID,
            &[Value::Integer(FTS5_AVERAGES_ROWID), Value::Blob(Vec::new())],
            true,
        )?;
        ctx.shadow_put_rowid(
            &data_shadow,
            FTS5_STRUCTURE_ROWID,
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(empty_sblob.clone()),
            ],
            true,
        )?;

        ctx.declare_vtab(&cfg.declare_sql(&tbl_name))?;
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);

        let mut state = Fts5SharedState::new(&tbl_name);
        state.read_cache.structure_blob = Some(empty_sblob);
        state.read_cache.averages = Some(vec![0u64; 1 + cfg.columns.len()]);
        state.read_cache.configured_rank_weights = Some(Vec::new());
        state.read_cache.secure_delete = Some(false);

        Ok(Box::new(Fts5Table {
            cfg,
            aux_registry: Arc::clone(&self.aux_registry),
            state: Arc::new(Mutex::new(state)),
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
            cfg,
            aux_registry: Arc::clone(&self.aux_registry),
            state: Arc::new(Mutex::new(Fts5SharedState::new(&tbl_name))),
        }))
    }
}

#[derive(Debug)]
struct Fts5Table {
    cfg: Fts5Config,
    aux_registry: Arc<Fts5AuxRegistry>,
    state: Arc<Mutex<Fts5SharedState>>,
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
const IDX_FLAG_NEEDS_CONTENT: i32 = 0x2000;
const IDX_FLAG_NEEDS_RANK_COL: i32 = 0x4000;

impl Fts5Table {
    fn hidden_tbl_col(&self) -> i32 {
        self.cfg.columns.len() as i32
    }

    fn hidden_rank_col(&self) -> i32 {
        (self.cfg.columns.len() + 1) as i32
    }

    fn read_content_row(
        &self,
        state: &Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<Vec<Value>>> {
        match &self.cfg.content_mode {
            ContentMode::Contentless => Ok(Some(vec![Value::Null; self.cfg.columns.len()])),
            ContentMode::Normal => {
                if let Some(mut r) = ctx.shadow_get_rowid(&state.shadows.content, rowid)? {
                    if !r.is_empty() {
                        r.drain(..1);
                    }
                    return Ok(Some(r));
                }
                Ok(None)
            }
            ContentMode::External { table, rowid_col } => {
                let db_q = quote_ident(ctx.db_name());
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

    fn push_pending_and_maybe_flush(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        op: PendingOp,
    ) -> Result<()> {
        state.pending.approx_bytes += op.approx_mem_bytes();
        state.pending.ops.push(op);
        if !state.in_tx
            || state.pending.approx_bytes >= FTS5_PENDING_MAX_BYTES
            || state.pending.ops.len() >= FTS5_PENDING_MAX_OPS
        {
            state.flush_pending(&self.cfg, ctx)?;
        }
        Ok(())
    }

    fn insert_document(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        explicit_rowid: Option<i64>,
        col_vals: &[Value],
    ) -> Result<i64> {
        let n_cols = self.cfg.columns.len();
        let rowid = match &self.cfg.content_mode {
            ContentMode::Normal => {
                let rid = match explicit_rowid {
                    Some(r) => r,
                    None => ctx
                        .shadow_max_rowid(&state.shadows.content)?
                        .map_or(1, |m| m.saturating_add(1)),
                };
                let mut row = Vec::with_capacity(1 + n_cols);
                row.push(Value::Integer(rid));
                for v in col_vals.iter().take(n_cols) {
                    row.push(v.clone());
                }
                while row.len() < 1 + n_cols {
                    row.push(Value::Null);
                }
                ctx.shadow_put_rowid(&state.shadows.content, rid, &row, false)?;
                rid
            }
            ContentMode::Contentless | ContentMode::External { .. } => {
                if let Some(rid) = explicit_rowid {
                    rid
                } else if self.cfg.columnsize {
                    ctx.shadow_max_rowid(&state.shadows.docsize)?
                        .map_or(1, |m| m.saturating_add(1))
                } else {
                    let base_avgs = state.read_averages(&self.cfg, ctx)?;
                    let net_delta = state.pending.net_doc_delta();
                    ((base_avgs[0] as i64) + net_delta) + 1
                }
            }
        };

        let (doc_terms, col_sizes) = build_doc_term_encoded_postings(&self.cfg, col_vals, false);

        if self.cfg.columnsize {
            let sz_blob = Value::Blob(encode_varint_list(&col_sizes));
            if self.cfg.contentless_delete {
                ctx.shadow_put_rowid(
                    &state.shadows.docsize,
                    rowid,
                    &[Value::Integer(rowid), sz_blob, Value::Integer(1)],
                    true,
                )?;
            } else {
                ctx.shadow_put_rowid(
                    &state.shadows.docsize,
                    rowid,
                    &[Value::Integer(rowid), sz_blob],
                    true,
                )?;
            }
            state
                .read_cache
                .insert_docsize(rowid, DocColSizes::from_u64_slice(&col_sizes));
        }

        self.push_pending_and_maybe_flush(
            state,
            ctx,
            PendingOp::DocPostings {
                rowid,
                is_delete: false,
                col_sizes,
                doc_terms,
            },
        )?;
        Ok(rowid)
    }

    fn delete_document_with_values(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        old_vals: &[Value],
        remove_content_row: bool,
    ) -> Result<()> {
        let (doc_terms, col_sizes) = build_doc_term_encoded_postings(&self.cfg, old_vals, true);

        if remove_content_row && self.cfg.content_mode == ContentMode::Normal {
            ctx.shadow_delete_rowid(&state.shadows.content, rowid)?;
        }

        if self.cfg.columnsize {
            ctx.shadow_delete_rowid(&state.shadows.docsize, rowid)?;
            state.read_cache.remove_docsize(rowid);
        }

        self.push_pending_and_maybe_flush(
            state,
            ctx,
            PendingOp::DocPostings {
                rowid,
                is_delete: true,
                col_sizes,
                doc_terms,
            },
        )
    }

    fn delete_contentless_rowid(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<()> {
        let mut old_col_sizes = vec![0u64; self.cfg.columns.len()];
        let mut had_docsize = false;
        if self.cfg.columnsize {
            if let Some(row) = ctx.shadow_get_rowid(&state.shadows.docsize, rowid)?
                && let Some(Value::Blob(b)) = row.get(1)
            {
                had_docsize = true;
                if let Some(list) = decode_varint_list(b) {
                    for (i, sz) in list.into_iter().enumerate().take(old_col_sizes.len()) {
                        old_col_sizes[i] = sz;
                    }
                }
            }
            ctx.shadow_delete_rowid(&state.shadows.docsize, rowid)?;
            state.read_cache.remove_docsize(rowid);
        }

        self.push_pending_and_maybe_flush(
            state,
            ctx,
            PendingOp::ContentlessRowidDelete {
                rowid,
                old_col_sizes,
                had_docsize,
            },
        )
    }

    fn execute_special_command(
        &self,
        state: &mut Fts5SharedState,
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
                self.delete_document_with_values(state, ctx, rid, old_cols, false)?;
                Ok(())
            }
            "delete-all" => {
                if self.cfg.content_mode == ContentMode::Normal {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "'delete-all' may not be used with a normal content table",
                    ));
                }
                for sp in &mut state.pending.savepoints {
                    if sp.flushed_pre_ops.is_none() && sp.ops_len > 0 {
                        sp.flushed_pre_ops = Some(state.pending.ops[..sp.ops_len].to_vec());
                    }
                    sp.flushed_since = true;
                }
                state.pending.ops.clear();
                state.pending.approx_bytes = 0;
                self.reset_index_storage(state, ctx)?;
                Ok(())
            }
            "rebuild" => {
                if self.cfg.content_mode == ContentMode::Contentless {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "'rebuild' may not be used with a contentless fts5 table",
                    ));
                }
                for sp in &mut state.pending.savepoints {
                    if sp.flushed_pre_ops.is_none() && sp.ops_len > 0 {
                        sp.flushed_pre_ops = Some(state.pending.ops[..sp.ops_len].to_vec());
                    }
                    sp.flushed_since = true;
                }
                state.pending.ops.clear();
                state.pending.approx_bytes = 0;
                self.rebuild_index(state, ctx)
            }
            "optimize" | "merge" => self.compact_segments(state, ctx),
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
                state.flush_pending(&self.cfg, ctx)?;
                self.verify_integrity(state, ctx, check_external)
            }
            "rank" => {
                let rank_val = values
                    .get(self.cfg.columns.len() + 1)
                    .cloned()
                    .unwrap_or(Value::Null);
                let rank_str = rank_val.to_text_lossy();
                let parsed_weights = parse_rank_expression(&rank_str)?;
                let config_q = quote_ident(&state.shadows.config);
                ctx.execute_sql(
                    &format!("INSERT OR REPLACE INTO {db_q}.{config_q}(k, v) VALUES('rank', ?1)"),
                    &[Value::Text(rank_str)],
                )?;
                state.read_cache.configured_rank_weights = Some(parsed_weights);
                Ok(())
            }
            "pgsz" | "automerge" | "crisismerge" | "usermerge" | "secure-delete" => {
                let rank_val = values
                    .get(self.cfg.columns.len() + 1)
                    .cloned()
                    .unwrap_or(Value::Integer(0));
                if cmd == "secure-delete" {
                    state.read_cache.secure_delete = Some(rank_val.to_integer_lossy() != 0);
                }
                let config_q = quote_ident(&state.shadows.config);
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

    fn reset_index_storage(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let next_cookie = read_structure_cookie(ctx, &state.shadows.data).wrapping_add(1);
        let data_q = quote_ident(&state.shadows.data);
        let idx_q = quote_ident(&state.shadows.idx);
        ctx.execute_sql(&format!("DELETE FROM {db_q}.{data_q}"), &[])?;
        ctx.execute_sql(&format!("DELETE FROM {db_q}.{idx_q}"), &[])?;
        if self.cfg.columnsize {
            let docsize_q = quote_ident(&state.shadows.docsize);
            ctx.execute_sql(&format!("DELETE FROM {db_q}.{docsize_q}"), &[])?;
        }
        let empty_sblob =
            encode_empty_structure_record_with_cookie(self.cfg.contentless_delete, next_cookie);
        ctx.shadow_put_rowid(
            &state.shadows.data,
            FTS5_AVERAGES_ROWID,
            &[Value::Integer(FTS5_AVERAGES_ROWID), Value::Blob(Vec::new())],
            true,
        )?;
        ctx.shadow_put_rowid(
            &state.shadows.data,
            FTS5_STRUCTURE_ROWID,
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(empty_sblob.clone()),
            ],
            true,
        )?;
        state.read_cache.clear_segment_maps();
        state.read_cache.clear_docsize();
        state.read_cache.structure_blob = Some(empty_sblob);
        state.read_cache.averages = Some(vec![0u64; 1 + self.cfg.columns.len()]);
        Ok(())
    }

    fn rebuild_index(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let src_rows = match &self.cfg.content_mode {
            ContentMode::Normal => {
                let content_q = quote_ident(&state.shadows.content);
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

        self.reset_index_storage(state, ctx)?;
        let mut merged: MergedIndexMap = BTreeMap::new();
        let mut avgs = vec![0u64; 1 + self.cfg.columns.len()];

        for row in src_rows {
            let rid = row.first().map_or(0, Value::to_integer_lossy);
            let cvals = &row[1..];
            let (doc_terms, col_sizes) = build_doc_term_postings(&self.cfg, cvals, false);
            for (k, chits) in doc_terms {
                merged.entry(k).or_default().insert(rid, chits);
            }
            if self.cfg.columnsize {
                let sz_blob = Value::Blob(encode_varint_list(&col_sizes));
                if self.cfg.contentless_delete {
                    ctx.shadow_put_rowid(
                        &state.shadows.docsize,
                        rid,
                        &[Value::Integer(rid), sz_blob, Value::Integer(1)],
                        true,
                    )?;
                } else {
                    ctx.shadow_put_rowid(
                        &state.shadows.docsize,
                        rid,
                        &[Value::Integer(rid), sz_blob],
                        true,
                    )?;
                }
                state
                    .read_cache
                    .insert_docsize(rid, DocColSizes::from_u64_slice(&col_sizes));
            }
            avgs[0] += 1;
            for (i, &sz) in col_sizes.iter().enumerate() {
                avgs[1 + i] += sz;
            }
        }

        let sblob = write_c_fts5_index(ctx, &db_q, &state.shadows.table_name, &self.cfg, &merged)?;
        let segs = decode_structure_segments(&sblob)?;
        state.write_averages(ctx, &avgs)?;
        state.read_cache.clear_segment_maps();
        state.read_cache.structure_blob = Some(sblob);
        state.read_cache.segments = segs;
        state.read_cache.averages = Some(avgs);
        Ok(())
    }

    fn compact_segments(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<()> {
        if !state.pending.ops.is_empty() {
            for sp in &mut state.pending.savepoints {
                if sp.flushed_pre_ops.is_none() && sp.ops_len > 0 {
                    sp.flushed_pre_ops = Some(state.pending.ops[..sp.ops_len].to_vec());
                }
                sp.flushed_since = true;
            }
            let ops = std::mem::take(&mut state.pending.ops);
            state.pending.approx_bytes = 0;
            return state.flush_pending_via_compaction(&self.cfg, ctx, ops);
        }
        state.ensure_structure_coherent(ctx)?;
        if state.read_cache.segments.len() <= 1
            && state
                .read_cache
                .segments
                .iter()
                .all(|s| s.n_tombstone_pages == 0)
        {
            return Ok(());
        }
        let db_q = quote_ident(ctx.db_name());
        let merged = load_merged_index(ctx, &db_q, &state.shadows.table_name, self.cfg.detail)?;
        let sblob = write_c_fts5_index(ctx, &db_q, &state.shadows.table_name, &self.cfg, &merged)?;
        let segs = decode_structure_segments(&sblob)?;
        state.read_cache.clear_segment_maps();
        state.read_cache.structure_blob = Some(sblob);
        state.read_cache.segments = segs;
        Ok(())
    }

    fn verify_integrity(
        &self,
        state: &Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        check_external: bool,
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let actual = load_merged_index(ctx, &db_q, &state.shadows.table_name, self.cfg.detail)?;

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
                let content_q = quote_ident(&state.shadows.content);
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
            let (doc_terms, _) = build_doc_term_postings(&self.cfg, cvals, false);
            for (term, chits) in doc_terms {
                expected.entry(term).or_default().insert(rid, chits);
            }
        }
        if actual != expected {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                format!(
                    "fts5: checksum mismatch for table \"{}\"",
                    state.shadows.table_name
                ),
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
    };
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

        let n_vis = self.cfg.columns.len();
        let vis_mask = if n_vis == 0 {
            0u64
        } else if n_vis >= 64 {
            u64::MAX
        } else {
            (1u64 << n_vis) - 1
        };
        if (info.col_used & vis_mask) != 0
            || !self.cfg.columnsize
            || self.cfg.tokenizer.is_trigram()
        {
            idx_num |= IDX_FLAG_NEEDS_CONTENT;
        }
        let rank_bit = 1u64 << (rank_col as usize).min(63);
        if (info.col_used & rank_bit) != 0
            || (idx_num & (IDX_FLAG_ORDER_RANK_ASC | IDX_FLAG_ORDER_RANK_DESC)) != 0
            || (idx_num & IDX_FLAG_HAS_RANK_CFG) != 0
        {
            idx_num |= IDX_FLAG_NEEDS_RANK_COL;
        }

        info.idx_num = idx_num;
        info.idx_str = idx_str;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(Fts5Cursor {
            cfg: self.cfg.clone(),
            aux_registry: Arc::clone(&self.aux_registry),
            state: Arc::clone(&self.state),
            rows: Vec::new(),
            pos: 0,
        }))
    }

    fn begin(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.in_tx = true;
        Ok(())
    }

    fn sync(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.flush_pending(&self.cfg, ctx)
    }

    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.in_tx = false;
        st.pending.clear();
        Ok(())
    }

    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.in_tx = false;
        st.pending.clear();
        st.read_cache.clear_all();
        Ok(())
    }

    fn savepoint(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.in_tx = true;
        let ops_len = st.pending.ops.len();
        let approx_bytes = st.pending.approx_bytes;
        st.pending.savepoints.push(PendingSavepoint {
            idx,
            ops_len,
            approx_bytes,
            flushed_since: false,
            flushed_pre_ops: None,
        });
        Ok(())
    }

    fn release(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = st.pending.savepoints.iter().position(|sp| sp.idx >= idx) {
            st.pending.savepoints.truncate(pos);
        }
        Ok(())
    }

    fn rollback_to(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = st.pending.savepoints.iter().position(|sp| sp.idx >= idx) {
            let any_flushed = st.pending.savepoints[pos..]
                .iter()
                .any(|sp| sp.flushed_since);
            let target_len = st.pending.savepoints[pos].ops_len;
            let target_bytes = st.pending.savepoints[pos].approx_bytes;
            let keep_same = st.pending.savepoints[pos].idx == idx;
            let pre_ops = st.pending.savepoints[pos].flushed_pre_ops.clone();

            if keep_same {
                st.pending.savepoints.truncate(pos + 1);
                st.pending.savepoints[pos].flushed_since = false;
                st.pending.savepoints[pos].flushed_pre_ops = None;
            } else {
                st.pending.savepoints.truncate(pos);
            }

            if any_flushed {
                st.pending.ops = pre_ops.unwrap_or_default();
                st.pending.approx_bytes =
                    st.pending.ops.iter().map(PendingOp::approx_mem_bytes).sum();
                st.read_cache.clear_all();
            } else {
                st.pending.ops.truncate(target_len);
                st.pending.approx_bytes = target_bytes;
                st.read_cache.clear_docsize();
                st.read_cache.configured_rank_weights = None;
                st.read_cache.secure_delete = None;
            }
        } else {
            st.read_cache.clear_all();
        }
        Ok(())
    }

    fn flush_before_shadow_access(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.flush_pending(&self.cfg, ctx)
    }

    fn invalidate_shadow_cache(&mut self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.read_cache.clear_all();
    }

    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let n_cols = self.cfg.columns.len();
        match op {
            VTabUpdateOperation::Insert { new_rowid, values } => {
                // Check special command: `INSERT INTO ft(ft, ...) VALUES('cmd', ...)`
                if let Some(Value::Text(cmd)) = values.get(n_cols) {
                    self.execute_special_command(&mut st, ctx, cmd, new_rowid, &values)?;
                    return Ok(None);
                }
                let explicit_rid = match new_rowid {
                    None | Some(Value::Null) => None,
                    Some(Value::Integer(i)) => Some(i),
                    Some(_) => {
                        return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                    }
                };
                let rid = self.insert_document(&mut st, ctx, explicit_rid, &values[..n_cols])?;
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
                    self.delete_contentless_rowid(&mut st, ctx, rid)?;
                } else if let Some(old_vals) = self.read_content_row(&st, ctx, rid)? {
                    self.delete_document_with_values(&mut st, ctx, rid, &old_vals, true)?;
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
                    self.delete_contentless_rowid(&mut st, ctx, old_rid)?;
                } else if let Some(old_vals) = self.read_content_row(&st, ctx, old_rid)? {
                    self.delete_document_with_values(&mut st, ctx, old_rid, &old_vals, true)?;
                }
                let inserted_rid =
                    self.insert_document(&mut st, ctx, Some(new_rid), &values[..n_cols])?;
                Ok(Some(inserted_rid))
            }
        }
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.flush_pending(&self.cfg, ctx)?;
        let db_q = quote_ident(ctx.db_name());
        let old_name = st.shadows.table_name.clone();
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
        st.shadows = Fts5ShadowNames::new(new_name);
        Ok(())
    }

    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.pending.clear();
        st.read_cache.clear_all();
        let db_q = quote_ident(ctx.db_name());
        for suf in SHADOW_SUFFIXES {
            let sq = quote_ident(&format!("{}_{suf}", st.shadows.table_name));
            ctx.execute_sql(&format!("DROP TABLE IF EXISTS {db_q}.{sq}"), &[])?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Fts5CursorRow {
    rowid: i64,
    cols: Vec<Value>,
    aux_handle: i64,
    rank: Option<f64>,
}

#[derive(Debug)]
struct Fts5Cursor {
    cfg: Fts5Config,
    aux_registry: Arc<Fts5AuxRegistry>,
    state: Arc<Mutex<Fts5SharedState>>,
    rows: Vec<Fts5CursorRow>,
    pos: usize,
}

impl Fts5Cursor {
    fn compute_fallback_col_sizes(&self, col_vals: &[Value]) -> DocColSizes {
        let mut out = vec![0usize; self.cfg.columns.len()];
        for (i, v) in col_vals.iter().enumerate().take(self.cfg.columns.len()) {
            if !self.cfg.unindexed[i] && !v.is_null() {
                out[i] = self.cfg.tokenizer.tokenize(&v.to_text_lossy(), false).len();
            }
        }
        DocColSizes::from_usize_vec(out)
    }

    fn load_content_vals(
        &self,
        shadows: &Fts5ShadowNames,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Vec<Value>> {
        match &self.cfg.content_mode {
            ContentMode::Contentless => Ok(vec![Value::Null; self.cfg.columns.len()]),
            ContentMode::Normal => {
                if let Some(mut r) = ctx.shadow_get_rowid(&shadows.content, rowid)? {
                    if !r.is_empty() {
                        r.drain(..1);
                    }
                    return Ok(r);
                }
                Ok(vec![Value::Null; self.cfg.columns.len()])
            }
            ContentMode::External { table, rowid_col } => {
                let db_q = quote_ident(ctx.db_name());
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

        let shadows = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !st.pending.ops.is_empty() {
                st.flush_pending(&self.cfg, ctx)?;
            }
            st.shadows.clone()
        };

        let db_q = quote_ident(ctx.db_name());
        let mode = idx_num & 0xff;
        let desc_rowid = (idx_num & IDX_FLAG_ORDER_ROWID_DESC) != 0;

        match mode {
            IDX_FULLSCAN => {
                let order_kw = if desc_rowid { "DESC" } else { "ASC" };
                match &self.cfg.content_mode {
                    ContentMode::Normal => {
                        let content_q = quote_ident(&shadows.content);
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
                        for mut r in qrows {
                            let rid = r[0].to_integer_lossy();
                            r.drain(..1);
                            self.rows.push(Fts5CursorRow {
                                rowid: rid,
                                cols: r,
                                aux_handle: 0,
                                rank: None,
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
                        for mut r in qrows {
                            let rid = r[0].to_integer_lossy();
                            r.drain(..1);
                            self.rows.push(Fts5CursorRow {
                                rowid: rid,
                                cols: r,
                                aux_handle: 0,
                                rank: None,
                            });
                        }
                    }
                    ContentMode::Contentless => {
                        if self.cfg.columnsize {
                            let docsize_q = quote_ident(&shadows.docsize);
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
                                    cols: Vec::new(),
                                    aux_handle: 0,
                                    rank: None,
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
                        if let Some(mut r) = ctx.shadow_get_rowid(&shadows.content, *rid)? {
                            if !r.is_empty() {
                                r.drain(..1);
                            }
                            self.rows.push(Fts5CursorRow {
                                rowid: *rid,
                                cols: r,
                                aux_handle: 0,
                                rank: None,
                            });
                        }
                    }
                    ContentMode::External { .. } => {
                        let vals = self.load_content_vals(&shadows, ctx, *rid)?;
                        self.rows.push(Fts5CursorRow {
                            rowid: *rid,
                            cols: vals,
                            aux_handle: 0,
                            rank: None,
                        });
                    }
                    ContentMode::Contentless => {
                        if self.cfg.columnsize
                            && ctx.shadow_get_rowid(&shadows.docsize, *rid)?.is_some()
                        {
                            self.rows.push(Fts5CursorRow {
                                rowid: *rid,
                                cols: Vec::new(),
                                aux_handle: 0,
                                rank: None,
                            });
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
                    return self.filter(ctx, IDX_FULLSCAN | (idx_num & !0xff), None, &[]);
                }
                let mut candidate_rowids: Option<BTreeSet<i64>> = None;
                {
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.ensure_structure_coherent(ctx)?;
                    for run in &runs {
                        for tg in self.cfg.tokenizer.tokenize(run, true) {
                            let tkey = make_term_key(0, &tg.term);
                            let hits = st.lookup_exact_term(&self.cfg, ctx, &tkey)?;
                            let set: BTreeSet<i64> = hits.iter().map(|&(rid, _)| rid).collect();
                            candidate_rowids = Some(match candidate_rowids {
                                None => set,
                                Some(prev) => prev.intersection(&set).copied().collect(),
                            });
                            if candidate_rowids.as_ref().is_some_and(BTreeSet::is_empty) {
                                break;
                            }
                        }
                    }
                }
                let mut rids: Vec<i64> = candidate_rowids.unwrap_or_default().into_iter().collect();
                if desc_rowid {
                    rids.reverse();
                }
                for rid in rids {
                    let cols = self.load_content_vals(&shadows, ctx, rid)?;
                    self.rows.push(Fts5CursorRow {
                        rowid: rid,
                        cols,
                        aux_handle: 0,
                        rank: None,
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

                let is_trigram = self.cfg.tokenizer.is_trigram();
                let has_short_trigram = is_trigram
                    && phrases
                        .iter()
                        .any(|p| p.tokens.is_empty() && !p.raw_text.is_empty());

                let need_rank_val = (idx_num & IDX_FLAG_NEEDS_RANK_COL) != 0;
                let need_aux_texts = self.aux_registry.needs_aux_text() || is_trigram;
                let need_content = (idx_num & IDX_FLAG_NEEDS_CONTENT) != 0 || need_aux_texts;
                let n_cols = self.cfg.columns.len();

                let (
                    rank_weights,
                    phrase_doc_hits,
                    mut matched_docs,
                    total_rows,
                    total_tokens,
                    mut prebuilt_aux_rows,
                ) = {
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.ensure_structure_coherent(ctx)?;

                    let rw = if (idx_num & IDX_FLAG_HAS_RANK_CFG) != 0 {
                        let rv = args.get(1).cloned().unwrap_or(Value::Null);
                        parse_rank_expression(&rv.to_text_lossy())?
                    } else if need_rank_val {
                        st.get_configured_rank_weights(ctx)?
                    } else {
                        Vec::new()
                    };

                    let mut precomputed_phrases = Vec::with_capacity(phrases.len());
                    let mut pd_hits = Vec::with_capacity(phrases.len());
                    for (pi, ph) in phrases.iter().enumerate() {
                        let pdocs = st.eval_phrase_selective(&self.cfg, ctx, pi, ph)?;
                        pd_hits.push(pdocs.len());
                        precomputed_phrases.push(pdocs);
                    }

                    let mut mdocs = if has_short_trigram {
                        EvalDocSet::new()
                    } else {
                        eval_query_expr_precomputed(
                            &qexpr,
                            &phrases,
                            &self.cfg,
                            &mut precomputed_phrases,
                            None,
                        )
                    };

                    let avgs = st.read_averages(&self.cfg, ctx)?;
                    let tr = avgs[0] as usize;
                    let tt: usize = avgs[1..].iter().map(|&x| x as usize).sum();

                    let prebuilt = if !need_content && !has_short_trigram {
                        let zero_sizes = DocColSizes::zeros(n_cols);
                        let mut arows: Vec<AuxRowEntry> = Vec::with_capacity(mdocs.len());
                        self.rows.reserve(mdocs.len());
                        for (rid, insts) in std::mem::take(&mut mdocs) {
                            let col_sizes = match st.get_or_load_docsize(&self.cfg, ctx, rid)? {
                                Some(sz) => sz,
                                None => zero_sizes.clone(),
                            };
                            self.rows.push(Fts5CursorRow {
                                rowid: rid,
                                cols: Vec::new(),
                                aux_handle: 0,
                                rank: None,
                            });
                            arows.push(AuxRowEntry {
                                col_sizes,
                                instances: insts,
                                texts: None,
                            });
                        }
                        Some(arows)
                    } else {
                        None
                    };

                    (rw, pd_hits, mdocs, tr, tt, prebuilt)
                };

                if has_short_trigram {
                    let mut dummy_cur = Fts5Cursor {
                        cfg: self.cfg.clone(),
                        aux_registry: Arc::clone(&self.aux_registry),
                        state: Arc::clone(&self.state),
                        rows: Vec::new(),
                        pos: 0,
                    };
                    dummy_cur.filter(ctx, IDX_FULLSCAN, None, &[])?;
                    for r in dummy_cur.rows {
                        matched_docs.push((r.rowid, PhraseHits::new()));
                    }
                }

                if !need_content {
                    let aux_rows = if let Some(arows) = prebuilt_aux_rows.take() {
                        arows
                    } else {
                        let mut arows: Vec<AuxRowEntry> = Vec::with_capacity(matched_docs.len());
                        self.rows.reserve(matched_docs.len());
                        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                        let zero_sizes = DocColSizes::zeros(n_cols);
                        for (rid, insts) in matched_docs {
                            let col_sizes = match st.get_or_load_docsize(&self.cfg, ctx, rid)? {
                                Some(sz) => sz,
                                None => zero_sizes.clone(),
                            };
                            self.rows.push(Fts5CursorRow {
                                rowid: rid,
                                cols: Vec::new(),
                                aux_handle: 0,
                                rank: None,
                            });
                            arows.push(AuxRowEntry {
                                col_sizes,
                                instances: insts,
                                texts: None,
                            });
                        }
                        arows
                    };
                    let row_count = self.rows.len();
                    let qctx = Arc::new(AuxQueryContext::build(
                        n_cols,
                        self.cfg.tokenizer.clone(),
                        is_trigram,
                        phrases.iter().map(|p| p.tokens.len().max(1)).collect(),
                        &phrase_doc_hits,
                        total_rows.max(row_count),
                        total_tokens,
                        aux_rows,
                    ));
                    let qid = self.aux_registry.register_query(Arc::clone(&qctx));
                    for (idx, r) in self.rows.iter_mut().enumerate() {
                        r.aux_handle = Fts5AuxRegistry::encode_handle(qid, idx);
                        if need_rank_val {
                            r.rank = Some(compute_bm25_for_row(&qctx, idx, &rank_weights));
                        }
                    }
                } else {
                    let mut aux_rows: Vec<AuxRowEntry> = Vec::with_capacity(matched_docs.len());
                    let mut matched_row_cols: Vec<(i64, Vec<Value>)> =
                        Vec::with_capacity(matched_docs.len());
                    for (rid, insts) in matched_docs {
                        let cols = self.load_content_vals(&shadows, ctx, rid)?;
                        let col_sizes = {
                            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            match st.get_or_load_docsize(&self.cfg, ctx, rid)? {
                                Some(sz) => sz,
                                None => self.compute_fallback_col_sizes(&cols),
                            }
                        };
                        let texts = if need_aux_texts {
                            let col_texts: Vec<Option<String>> = cols
                                .iter()
                                .map(|v| (!v.is_null()).then(|| v.to_text_lossy()))
                                .collect();
                            let mut trigram_byte_spans = if is_trigram {
                                vec![Vec::new(); n_cols]
                            } else {
                                Vec::new()
                            };
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
                                            let spans = find_trigram_substring_spans(
                                                ct,
                                                &ph.raw_text,
                                                cs,
                                                rd,
                                            );
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
                            Some(Box::new(AuxRowTexts {
                                col_texts,
                                trigram_byte_spans,
                            }))
                        } else {
                            None
                        };

                        matched_row_cols.push((rid, cols));
                        aux_rows.push(AuxRowEntry {
                            col_sizes,
                            instances: insts,
                            texts,
                        });
                    }

                    let qctx = Arc::new(AuxQueryContext::build(
                        n_cols,
                        self.cfg.tokenizer.clone(),
                        is_trigram,
                        phrases.iter().map(|p| p.tokens.len().max(1)).collect(),
                        &phrase_doc_hits,
                        total_rows.max(matched_row_cols.len()),
                        total_tokens,
                        aux_rows,
                    ));
                    let qid = self.aux_registry.register_query(Arc::clone(&qctx));

                    self.rows.reserve(matched_row_cols.len());
                    for (idx, (rid, cols)) in matched_row_cols.into_iter().enumerate() {
                        let rank_score =
                            need_rank_val.then(|| compute_bm25_for_row(&qctx, idx, &rank_weights));
                        self.rows.push(Fts5CursorRow {
                            rowid: rid,
                            cols,
                            aux_handle: Fts5AuxRegistry::encode_handle(qid, idx),
                            rank: rank_score,
                        });
                    }
                }

                if (idx_num & IDX_FLAG_ORDER_RANK_ASC) != 0 {
                    self.rows.sort_by(|a, b| {
                        a.rank
                            .unwrap_or(0.0)
                            .total_cmp(&b.rank.unwrap_or(0.0))
                            .then_with(|| a.rowid.cmp(&b.rowid))
                    });
                } else if (idx_num & IDX_FLAG_ORDER_RANK_DESC) != 0 {
                    self.rows.sort_by(|a, b| {
                        b.rank
                            .unwrap_or(0.0)
                            .total_cmp(&a.rank.unwrap_or(0.0))
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
            if row.aux_handle > 0 {
                Ok(Value::Integer(row.aux_handle))
            } else {
                Ok(Value::Null)
            }
        } else if col_idx == n_cols + 1 {
            Ok(row.rank.map_or(Value::Null, Value::real))
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
