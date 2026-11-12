//! Pure safe Rust implementation of SQLite's `fts5` and `fts5vocab` full-text search
//! virtual table modules (`sqlite/ext/fts5/`).
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::fts5_fp::real_to_text;
use crate::fts5_tokenize::{FTS5_MAX_TOKEN_SIZE, Tokenizer, TokenizerError, TokenizerPattern};
use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConflictMode, VTabConstraintOp,
    VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor,
    VirtualTable, VirtualTableModule, get_varint, put_varint,
};

const FTS5_AVERAGES_ROWID: i64 = 1;
const FTS5_STRUCTURE_ROWID: i64 = 10;
const FTS5_LEAF_TARGET_PGSZ: usize = 4000;
/// Hard upper bound for the bytes placed on one leaf by a single position list; a larger
/// list continues on term-less leaves.
const FTS5_LEAF_MAX_PGSZ: usize = 16_000;
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
                let mut sum = 0usize;
                for &v in &vals[..(*len as usize)] {
                    sum += v as usize;
                }
                sum
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

/// Text of every user column of one result row as `xColumnText` returns it (`None` for a
/// NULL value or a column the table does not store).
#[derive(Debug, Clone)]
struct AuxRowTexts {
    col_texts: Vec<Option<Vec<u8>>>,
}

#[derive(Debug, Clone)]
struct AuxRowEntry {
    /// Tokens per column (`xColumnSize`); see `AuxQueryContext::unknown_size_cols`.
    col_sizes: DocColSizes,
    /// The phrase instances of the row in `xInst` order (column, offset, phrase).
    instances: PhraseHits,
    texts: Option<Box<AuxRowTexts>>,
    /// The row is in the index but not in the content source (`fts5SeekCursor` fails): the
    /// functions that need its text report SQLITE_CORRUPT.
    missing_content: bool,
}

/// Per-query state of the auxiliary functions (the `Fts5Cursor` side of `Fts5ExtensionApi`).
#[derive(Debug)]
struct AuxQueryContext {
    num_cols: usize,
    tokenizer: TokenizerSpec,
    /// `xPhraseSize` of every phrase of the query.
    phrase_lengths: Vec<usize>,
    /// `Fts5Bm25Data.aIDF`.
    phrase_idfs: Vec<f64>,
    /// `Fts5Bm25Data.avgdl` (not finite for an empty index, like in C).
    avgdl: f64,
    /// `xRowCount`: the row count of the averages record.
    total_rows: u64,
    /// Columns whose size `xColumnSize` reports as `-1`: the indexed columns of a table that
    /// stores neither the sizes nor the content. Empty when every size is known.
    unknown_size_cols: Vec<bool>,
    /// `bm25()` reads the row text (column sizes without %_docsize, or the phrase instances
    /// of a detail=column / detail=none table).
    bm25_needs_content: bool,
    rows: Vec<AuxRowEntry>,
}

impl AuxQueryContext {
    /// `fts5Bm25GetData`: `phrase_doc_hits[i]` is the number of rows phrase `i` matches on
    /// its own (`xQueryPhrase`), `total_rows` / `total_tokens` come from the averages record.
    #[allow(clippy::too_many_arguments)]
    fn build(
        num_cols: usize,
        tokenizer: TokenizerSpec,
        phrase_lengths: Vec<usize>,
        phrase_doc_hits: &[usize],
        total_rows: u64,
        total_tokens: u64,
        unknown_size_cols: Vec<bool>,
        rows: Vec<AuxRowEntry>,
    ) -> Self {
        let n_row = total_rows as f64;
        let avgdl = (total_tokens as f64) / n_row;
        let phrase_idfs = phrase_doc_hits
            .iter()
            .map(|&hit| {
                let n_hit = hit as f64;
                let idf = ((n_row - n_hit + 0.5) / (n_hit + 0.5)).ln();
                if idf <= 0.0 { 1.0e-6 } else { idf }
            })
            .collect();
        Self {
            num_cols,
            tokenizer,
            phrase_lengths,
            phrase_idfs,
            avgdl,
            total_rows,
            unknown_size_cols,
            bm25_needs_content: false,
            rows,
        }
    }

    /// `xColumnSize(iCol)` of a row.
    #[inline]
    fn col_size(&self, row: &AuxRowEntry, col: usize) -> i64 {
        if self.unknown_size_cols.get(col).copied().unwrap_or(false) {
            -1
        } else {
            row.col_sizes.get(col) as i64
        }
    }

    /// `xColumnSize(-1)`: the size of a row in tokens.
    #[inline]
    fn row_size(&self, row: &AuxRowEntry) -> f64 {
        if self.unknown_size_cols.is_empty() {
            row.col_sizes.total_len() as f64
        } else {
            (0..self.num_cols)
                .map(|i| self.col_size(row, i))
                .sum::<i64>() as f64
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
#[derive(Debug)]
pub struct Fts5AuxRegistry {
    inner: Mutex<AuxRegistryInner>,
    aux_text_depth: AtomicUsize,
    /// Shared state of rows that carry none (a scan without auxiliary text functions).
    rowless: Arc<AuxQueryContext>,
}

impl Default for Fts5AuxRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::default(),
            aux_text_depth: AtomicUsize::new(0),
            rowless: Arc::new(AuxQueryContext::build(
                0,
                TokenizerSpec::default(),
                Vec::new(),
                &[],
                0,
                0,
                Vec::new(),
                Vec::new(),
            )),
        }
    }
}

/// Table-column value of a row that has no per-row auxiliary state. The query id part is
/// never handed out by `register_query`.
const FTS5_AUX_ROWLESS_HANDLE: i64 = 0x7fff_ffff_i64 << 32;

fn aux_context_error(func: &str) -> SqliteError {
    SqliteError::new(
        ErrorCode::Error,
        format!("unable to use function {func} in the requested context"),
    )
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

    /// Row handles are `(query id << 32) | (row index + 1)`.
    #[inline]
    fn encode_handle(qid: u32, row_idx: usize) -> i64 {
        ((qid as i64) << 32) | (((row_idx + 1) as u32) as i64)
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

    /// Resolves the value of the table-name column. `Ok(None)` for NULL (no current row);
    /// the row index is `None` for a row without per-row state.
    #[inline]
    fn lookup(
        &self,
        val: &Value,
        func: &str,
    ) -> Result<Option<(Arc<AuxQueryContext>, Option<usize>)>> {
        let handle = match val {
            Value::Null => return Ok(None),
            Value::Integer(h) if *h > 0 => *h,
            _ => return Err(aux_context_error(func)),
        };
        let qid = ((handle as u64) >> 32) as u32;
        let row_part = (handle as u32) as usize;
        if qid == 0 {
            return Err(aux_context_error(func));
        }
        if handle == FTS5_AUX_ROWLESS_HANDLE {
            return Ok(Some((Arc::clone(&self.rowless), None)));
        }
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((slot_qid, ref qctx)) = guard.slots[(qid as usize) & (FTS5_AUX_RING_SLOTS - 1)]
            && slot_qid == qid
            && row_part <= qctx.rows.len()
        {
            return Ok(Some((Arc::clone(qctx), row_part.checked_sub(1))));
        }
        Err(aux_context_error(func))
    }

    #[inline]
    pub fn eval_bm25_fast(&self, handle_val: &Value, weights: &[f64]) -> Result<Value> {
        let handle = match handle_val {
            Value::Null => return Ok(Value::Null),
            Value::Integer(h) if *h > 0 => *h,
            _ => return Err(aux_context_error("bm25")),
        };
        if handle == FTS5_AUX_ROWLESS_HANDLE {
            return Ok(Value::real(-0.0));
        }
        let qid = ((handle as u64) >> 32) as u32;
        let row_part = (handle as u32) as usize;
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if qid != 0
            && let Some((slot_qid, ref qctx)) =
                guard.slots[(qid as usize) & (FTS5_AUX_RING_SLOTS - 1)]
            && slot_qid == qid
            && row_part <= qctx.rows.len()
        {
            let score = compute_bm25_for_row(qctx, row_part.checked_sub(1), weights)?;
            return Ok(real_result(score));
        }
        Err(aux_context_error("bm25"))
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

    /// `fts5HighlightFunction`.
    pub fn eval_highlight(&self, args: &[Value]) -> Result<Value> {
        if args.len() != 4 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "wrong number of arguments to function highlight()",
            ));
        }
        let Some((qctx, row_idx)) = self.lookup(&args[0], "highlight")? else {
            return Ok(Value::Null);
        };
        let col = args[1].to_integer_lossy().clamp(-1, 0x7fff_ffff);
        if col < 0 || (col as usize) >= qctx.num_cols {
            return Ok(Value::Text(String::new()));
        }
        let col = col as usize;
        let open = fts5_value_bytes(&args[2]);
        let close = fts5_value_bytes(&args[3]);
        let Some(row) = row_idx.and_then(|i| qctx.rows.get(i)) else {
            return Ok(Value::Null);
        };
        if row.missing_content {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        }
        let Some(text) = row
            .texts
            .as_ref()
            .and_then(|t| t.col_texts.get(col))
            .and_then(|t| t.as_deref())
        else {
            return Ok(Value::Null);
        };
        let mut hl = HighlightCtx::new(
            &qctx,
            row,
            col,
            text,
            open.as_deref().unwrap_or(b""),
            close.as_deref().unwrap_or(b""),
        );
        hl.run();
        if hl.open_now {
            hl.out.extend_from_slice(hl.close);
        }
        hl.append_tail();
        Ok(Value::Text(String::from_utf8_lossy(&hl.out).into_owned()))
    }

    /// `fts5SnippetFunction`.
    pub fn eval_snippet(&self, args: &[Value]) -> Result<Value> {
        if args.len() != 6 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "wrong number of arguments to function snippet()",
            ));
        }
        let Some((qctx, row_idx)) = self.lookup(&args[0], "snippet")? else {
            return Ok(Value::Null);
        };
        let Some(row) = row_idx.and_then(|i| qctx.rows.get(i)) else {
            return Ok(Value::Null);
        };
        if row.missing_content {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        }
        let i_col = args[1].to_integer_lossy().clamp(-1, 0x7fff_ffff);
        let open = fts5_value_bytes(&args[2]).unwrap_or_default();
        let close = fts5_value_bytes(&args[3]).unwrap_or_default();
        let ellipsis = fts5_value_bytes(&args[4]).unwrap_or_default();
        let n_token = args[5].to_integer_lossy().clamp(0, 64);
        render_snippet(&qctx, row, i_col, &open, &close, &ellipsis, n_token)
    }

    pub fn eval_fts5_source_id() -> Value {
        Value::Text(FTS5_SOURCE_ID.to_owned())
    }

    pub fn eval_fts5_ptr_func(_arg: &Value) -> Value {
        Value::Null
    }
}

/// `sqlite3_result_double`: NaN becomes NULL.
fn real_result(r: f64) -> Value {
    if r.is_nan() {
        Value::Null
    } else {
        Value::real(r)
    }
}

/// `fts5Bm25Function`. `row_idx` is `None` for a row without match state (no phrases).
fn compute_bm25_for_row(
    qctx: &AuxQueryContext,
    row_idx: Option<usize>,
    weights: &[f64],
) -> Result<f64> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let n_phrase = qctx.phrase_idfs.len();
    let Some(row) = row_idx.and_then(|i| qctx.rows.get(i)) else {
        return Ok(-0.0);
    };
    // `sqlite3Fts5StorageRowCount`: an index that claims to hold no rows is corrupt; so is a
    // row whose text is needed but missing from the content source.
    if qctx.total_rows == 0 || (row.missing_content && qctx.bm25_needs_content) {
        return Err(SqliteError::new(
            ErrorCode::Corrupt,
            "database disk image is malformed",
        ));
    }
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
    // The total size of the current row in tokens (`xColumnSize(-1)`).
    let d: f64 = qctx.row_size(row);
    let mut score = 0.0f64;
    for (i, &f) in freq.iter().enumerate() {
        score +=
            qctx.phrase_idfs[i] * ((f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * d / qctx.avgdl)));
    }
    Ok(-score)
}

/// `CInstIter` (fts5_aux.c): iterates the phrase instances of one column, coalescing
/// instances that overlap (adjacent instances stay separate).
struct CInstIter<'a> {
    insts: &'a [PhraseInstance],
    phrase_lengths: &'a [usize],
    col: u32,
    next_inst: usize,
    /// First / last token of the current coalesced instance, `-1` when exhausted.
    start: i64,
    end: i64,
}

impl<'a> CInstIter<'a> {
    fn new(qctx: &'a AuxQueryContext, row: &'a AuxRowEntry, col: usize) -> Self {
        let mut it = Self {
            insts: row.instances.as_slice(),
            phrase_lengths: &qctx.phrase_lengths,
            col: col as u32,
            next_inst: 0,
            start: -1,
            end: -1,
        };
        it.next();
        it
    }

    /// `fts5CInstIterNext`.
    fn next(&mut self) {
        self.start = -1;
        self.end = -1;
        while self.next_inst < self.insts.len() {
            let inst = self.insts[self.next_inst];
            if inst.col_idx == self.col {
                let io = i64::from(inst.token_offset);
                let size = self
                    .phrase_lengths
                    .get(inst.phrase_idx as usize)
                    .copied()
                    .unwrap_or(0) as i64;
                let i_end = io - 1 + size;
                if self.start < 0 {
                    self.start = io;
                    self.end = i_end;
                } else if io <= self.end {
                    if i_end > self.end {
                        self.end = i_end;
                    }
                } else {
                    break;
                }
            }
            self.next_inst += 1;
        }
    }
}

/// `HighlightContext` + `fts5HighlightCb`.
struct HighlightCtx<'a> {
    tokenizer: &'a TokenizerSpec,
    range_start: i64,
    /// Last token to include, `-1` for no range.
    range_end: i64,
    open: &'a [u8],
    close: &'a [u8],
    text: &'a [u8],
    iter: CInstIter<'a>,
    pos: i64,
    off: usize,
    open_now: bool,
    out: Vec<u8>,
}

impl<'a> HighlightCtx<'a> {
    fn new(
        qctx: &'a AuxQueryContext,
        row: &'a AuxRowEntry,
        col: usize,
        text: &'a [u8],
        open: &'a [u8],
        close: &'a [u8],
    ) -> Self {
        Self {
            tokenizer: &qctx.tokenizer,
            range_start: 0,
            range_end: -1,
            open,
            close,
            text,
            iter: CInstIter::new(qctx, row, col),
            pos: 0,
            off: 0,
            open_now: false,
            out: Vec::with_capacity(text.len() + 16),
        }
    }

    /// `fts5HighlightAppend(&zIn[iOff], n)`: a negative length copies the rest of the text.
    fn append_text(&mut self, end: usize) {
        let from = self.off.min(self.text.len());
        let to = if end < self.off {
            self.text.len()
        } else {
            end.min(self.text.len())
        };
        self.out.extend_from_slice(&self.text[from..to]);
    }

    fn append_tail(&mut self) {
        let from = self.off.min(self.text.len());
        self.out.extend_from_slice(&self.text[from..]);
    }

    fn run(&mut self) {
        let tokenizer = self.tokenizer;
        let text = self.text;
        tokenizer.tok.tokenize(text, &mut |_, start_off, end_off| {
            self.token(start_off, end_off);
        });
    }

    /// `fts5HighlightCb`.
    fn token(&mut self, start_off: usize, end_off: usize) {
        let pos = self.pos;
        self.pos += 1;
        if self.range_end >= 0 {
            if pos < self.range_start || pos > self.range_end {
                return;
            }
            if self.range_start != 0 && pos == self.range_start {
                self.off = start_off;
            }
        }
        // Close the highlight when this token is not part of the current phrase and starts
        // past the text copied so far.
        if self.open_now && (pos <= self.iter.start || self.iter.start < 0) && start_off > self.off
        {
            self.out.extend_from_slice(self.close);
            self.open_now = false;
        }
        // The start of a new phrase: copy the text up to it and open the highlight.
        if pos == self.iter.start && !self.open_now {
            self.append_text(start_off);
            self.out.extend_from_slice(self.open);
            self.off = start_off;
            self.open_now = true;
        }
        if pos == self.iter.end {
            if !self.open_now {
                self.out.extend_from_slice(self.open);
                self.open_now = true;
            }
            self.append_text(end_off);
            self.off = end_off;
            self.iter.next();
        }
        if pos == self.range_end {
            if self.open_now {
                if self.iter.start >= 0 && pos >= self.iter.start {
                    self.append_text(end_off);
                    self.off = end_off;
                }
                self.out.extend_from_slice(self.close);
                self.open_now = false;
            }
            self.append_text(end_off);
            self.off = end_off;
        }
    }
}

/// `fts5SnippetScore`.
#[allow(clippy::too_many_arguments)]
fn snippet_score(
    qctx: &AuxQueryContext,
    row: &AuxRowEntry,
    n_docsize: i64,
    seen: &mut [bool],
    col: usize,
    pos: i64,
    n_token: i64,
    want_adjusted: bool,
) -> (i64, i64) {
    let mut first: i64 = -1;
    let mut last: i64 = 0;
    let mut score: i64 = 0;
    let end = pos + n_token;
    for inst in row.instances.as_slice() {
        let off = i64::from(inst.token_offset);
        let ip = inst.phrase_idx as usize;
        if inst.col_idx as usize == col && off >= pos && off < end {
            score += if seen.get(ip).copied().unwrap_or(false) {
                1
            } else {
                1000
            };
            if let Some(s) = seen.get_mut(ip) {
                *s = true;
            }
            if first < 0 {
                first = off;
            }
            last = off + qctx.phrase_lengths.get(ip).copied().unwrap_or(0) as i64;
        }
    }
    let mut adj = 0i64;
    if want_adjusted {
        adj = first - (n_token - (last - first)) / 2;
        if adj + n_token > n_docsize {
            adj = n_docsize - n_token;
        }
        if adj < 0 {
            adj = 0;
        }
    }
    (score, adj)
}

/// `fts5SnippetFunction` (after argument decoding).
fn render_snippet(
    qctx: &AuxQueryContext,
    row: &AuxRowEntry,
    i_col: i64,
    open: &[u8],
    close: &[u8],
    ellipsis: &[u8],
    n_token: i64,
) -> Result<Value> {
    let n_col = qctx.num_cols;
    let n_phrase = qctx.phrase_lengths.len();
    let mut best_col: usize = if i_col >= 0 { i_col as usize } else { 0 };
    let mut best_start: i64 = 0;
    let mut best_score: i64 = 0;
    let mut n_col_size: i64 = 0;
    let mut seen = vec![false; n_phrase];
    let col_text = |c: usize| -> Option<&[u8]> {
        row.texts
            .as_ref()
            .and_then(|t| t.col_texts.get(c))
            .and_then(|t| t.as_deref())
    };

    // `rc` of the C function: an instance beyond the column size sets FTS5_CORRUPT and stops
    // the scan of that column, but the next column starts over with a fresh `rc`.
    let mut corrupt = false;
    for i in 0..n_col {
        if i_col >= 0 && i_col as usize != i {
            continue;
        }
        corrupt = false;
        // `fts5SentenceFinderCb`: the first token of every sentence of the column.
        let mut first_tokens: Vec<i64> = Vec::new();
        if let Some(doc) = col_text(i) {
            let mut pos = 0i64;
            qctx.tokenizer.tok.tokenize(doc, &mut |_, start_off, _| {
                if pos > 0 {
                    let mut k = start_off as i64 - 1;
                    let mut c = 0u8;
                    while k >= 0 {
                        c = doc[k as usize];
                        if !matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
                            break;
                        }
                        k -= 1;
                    }
                    if k != start_off as i64 - 1 && (c == b'.' || c == b':') {
                        first_tokens.push(pos);
                    }
                } else {
                    first_tokens.push(0);
                }
                pos += 1;
            });
        }
        let n_docsize = qctx.col_size(row, i);
        for inst in row.instances.as_slice() {
            if inst.col_idx as usize != i {
                continue;
            }
            let io = i64::from(inst.token_offset);
            if io > n_docsize {
                corrupt = true;
                break;
            }
            seen.fill(false);
            let (score, adj) = snippet_score(qctx, row, n_docsize, &mut seen, i, io, n_token, true);
            if score > best_score {
                best_score = score;
                best_col = i;
                best_start = adj;
                n_col_size = n_docsize;
            }
            if !first_tokens.is_empty() && n_docsize > n_token {
                let mut jj = 0usize;
                while jj + 1 < first_tokens.len() {
                    if first_tokens[jj + 1] > io {
                        break;
                    }
                    jj += 1;
                }
                if first_tokens[jj] < io {
                    seen.fill(false);
                    let (mut score, _) = snippet_score(
                        qctx,
                        row,
                        n_docsize,
                        &mut seen,
                        i,
                        first_tokens[jj],
                        n_token,
                        false,
                    );
                    score += if first_tokens[jj] == 0 { 120 } else { 100 };
                    if score > best_score {
                        best_score = score;
                        best_col = i;
                        best_start = first_tokens[jj];
                        n_col_size = n_docsize;
                    }
                }
            }
        }
    }

    if corrupt {
        return Err(SqliteError::new(
            ErrorCode::Corrupt,
            "database disk image is malformed",
        ));
    }
    if best_col >= n_col {
        // xColumnText() reports SQLITE_RANGE for the requested column.
        return Err(SqliteError::new(
            ErrorCode::Range,
            "column index out of range",
        ));
    }
    if n_col_size == 0 {
        n_col_size = qctx.col_size(row, best_col);
    }
    let Some(text) = col_text(best_col) else {
        return Ok(Value::Null);
    };
    let mut hl = HighlightCtx::new(qctx, row, best_col, text, open, close);
    hl.range_start = best_start;
    hl.range_end = best_start + n_token - 1;
    if best_start > 0 {
        hl.out.extend_from_slice(ellipsis);
    }
    // Advance to the first coalesced instance at or after the snippet start.
    while hl.iter.start >= 0 && hl.iter.start < best_start {
        hl.iter.next();
    }
    hl.run();
    if hl.open_now {
        hl.out.extend_from_slice(hl.close);
    }
    if hl.range_end >= n_col_size - 1 {
        hl.append_tail();
    } else {
        hl.out.extend_from_slice(ellipsis);
    }
    Ok(Value::Text(String::from_utf8_lossy(&hl.out).into_owned()))
}

// ============================================================================
// Tokenizers (`unicode61`, `ascii`, `porter`, `trigram`)
// ============================================================================

#[derive(Debug, Clone)]
struct EmittedToken {
    term: String,
}

/// The tokenizer of one FTS5 table (`Fts5Config.t`): a C-exact built-in tokenizer plus the
/// conversions the rest of this module needs.
#[derive(Debug, Clone)]
struct TokenizerSpec {
    tok: Tokenizer,
}

impl Default for TokenizerSpec {
    fn default() -> Self {
        Self::load(&[]).expect("default unicode61 tokenizer")
    }
}

impl TokenizerSpec {
    /// `sqlite3Fts5LoadTokenizer`: `args` is the dequoted word list of the `tokenize=` option
    /// (empty = default `unicode61`).
    fn load(args: &[String]) -> Result<Self> {
        match Tokenizer::load(args) {
            Ok(tok) => Ok(Self { tok }),
            Err(TokenizerError::NoSuch(name)) => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such tokenizer: {name}"),
            )),
            Err(TokenizerError::Constructor) => Err(SqliteError::new(
                ErrorCode::Error,
                "error in tokenizer constructor",
            )),
        }
    }

    fn tokenize(&self, text: &str, _for_query: bool) -> Vec<EmittedToken> {
        let mut out = Vec::new();
        self.tok.tokenize(text.as_bytes(), &mut |tok, _, _| {
            out.push(EmittedToken {
                term: token_bytes_to_string(tok),
            });
        });
        out
    }

    /// Invokes `cb` with the bytes of every token of a document column, truncated like
    /// `fts5StorageInsertCallback` does.
    fn for_each_doc_token(&self, text: &[u8], mut cb: impl FnMut(&[u8])) {
        self.tok.tokenize(text, &mut |tok, _, _| {
            cb(&tok[..tok.len().min(FTS5_MAX_TOKEN_SIZE)]);
        });
    }
}

/// Converts a token to the `String` form used by the query side, applying the
/// `FTS5_MAX_TOKEN_SIZE` truncation.
fn token_bytes_to_string(tok: &[u8]) -> String {
    let tok = &tok[..tok.len().min(FTS5_MAX_TOKEN_SIZE)];
    match std::str::from_utf8(tok) {
        Ok(s) => s.to_owned(),
        Err(e) => String::from_utf8_lossy(&tok[..e.valid_up_to()]).into_owned(),
    }
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
    /// Contentless table that still stores its UNINDEXED columns in `%_content`
    /// (`contentless_unindexed=1`, C `FTS5_CONTENT_UNINDEXED`).
    unindexed_content: bool,
    /// `locale=1`: `%_content` carries one `l<N>` column per indexed column.
    locale: bool,
}

/// `FTS5_MAX_PREFIX_INDEXES`.
const FTS5_MAX_PREFIX_INDEXES: usize = 31;

fn cfg_err(msg: impl Into<String>) -> SqliteError {
    SqliteError::new(ErrorCode::Error, msg.into())
}

/// `sqlite3Fts5IsBareword`.
fn fts5_is_bareword(c: u8) -> bool {
    (c & 0x80) != 0 || c.is_ascii_alphanumeric() || c == b'_' || c == 0x1a
}

fn fts5_is_open_quote(c: u8) -> bool {
    matches!(c, b'"' | b'\'' | b'[' | b'`')
}

/// `fts5ConfigSkipWhitespace` (only the space character counts).
fn cfg_skip_ws(z: &[u8]) -> &[u8] {
    let n = z.iter().take_while(|&&c| c == b' ').count();
    &z[n..]
}

/// `fts5ConfigSkipBareword`: length of the leading bareword, `None` if there is none.
fn cfg_bareword_len(z: &[u8]) -> Option<usize> {
    let n = z.iter().take_while(|&&c| fts5_is_bareword(c)).count();
    (n > 0).then_some(n)
}

/// `fts5Dequote`: returns the dequoted text and the number of input bytes consumed. An
/// unterminated quote runs to the end of the input.
fn cfg_dequote(z: &[u8]) -> (Vec<u8>, usize) {
    let q = if z[0] == b'[' { b']' } else { z[0] };
    let mut i = 1usize;
    let mut out = Vec::with_capacity(z.len());
    while i < z.len() {
        if z[i] == q {
            if z.get(i + 1) != Some(&q) {
                i += 1;
                break;
            }
            i += 2;
            out.push(q);
        } else {
            out.push(z[i]);
            i += 1;
        }
    }
    (out, i)
}

/// `sqlite3Fts5Dequote`.
fn cfg_dequote_word(z: &[u8]) -> Vec<u8> {
    if !z.is_empty() && fts5_is_open_quote(z[0]) {
        cfg_dequote(z).0
    } else {
        z.to_vec()
    }
}

/// `fts5ConfigGobbleWord`: `Some((word, quoted, rest))` or `None` on a parse error.
fn cfg_gobble_word(z: &[u8]) -> Option<(Vec<u8>, bool, &[u8])> {
    if !z.is_empty() && fts5_is_open_quote(z[0]) {
        let (word, used) = cfg_dequote(z);
        Some((word, true, &z[used..]))
    } else {
        let n = cfg_bareword_len(z)?;
        Some((z[..n].to_vec(), false, &z[n..]))
    }
}

/// `fts5ConfigSkipLiteral`: length of the SQL literal at the start of `z`.
fn cfg_literal_len(z: &[u8]) -> Option<usize> {
    let at = |i: usize| z.get(i).copied().unwrap_or(0);
    match at(0) {
        b'n' | b'N' => (z.len() >= 4 && z[..4].eq_ignore_ascii_case(b"null")).then_some(4),
        b'x' | b'X' => {
            if at(1) != b'\'' {
                return None;
            }
            let mut p = 2usize;
            while at(p).is_ascii_hexdigit() {
                p += 1;
            }
            (at(p) == b'\'' && p.is_multiple_of(2)).then_some(p + 1)
        }
        b'\'' => {
            let mut p = 1usize;
            loop {
                if at(p) == b'\'' {
                    p += 1;
                    if at(p) != b'\'' {
                        return Some(p);
                    }
                }
                p += 1;
                if at(p) == 0 {
                    return None;
                }
            }
        }
        _ => {
            let mut p = 0usize;
            if at(p) == b'+' || at(p) == b'-' {
                p += 1;
            }
            while at(p).is_ascii_digit() {
                p += 1;
            }
            if at(p) == b'.' && at(p + 1).is_ascii_digit() {
                p += 2;
                while at(p).is_ascii_digit() {
                    p += 1;
                }
            }
            (p > 0).then_some(p)
        }
    }
}

/// `sqlite3_strnicmp(name, cmd, strlen(cmd))==0`: `cmd` is a case-insensitive prefix of
/// `name` (the way `fts5ConfigParseSpecial` recognises option names).
fn cfg_is_option(name: &str, cmd: &[u8]) -> bool {
    cmd.len() <= name.len() && name.as_bytes()[..cmd.len()].eq_ignore_ascii_case(cmd)
}

fn cfg_bool_arg(arg: &[u8]) -> Option<bool> {
    match arg {
        b"0" => Some(false),
        b"1" => Some(true),
        _ => None,
    }
}

/// Intermediate state of `sqlite3Fts5ConfigParse`.
#[derive(Default)]
struct Fts5ConfigBuilder {
    columns: Vec<String>,
    unindexed: Vec<bool>,
    tokenize_args: Option<Vec<String>>,
    prefixes: Vec<usize>,
    /// `None` = normal content, `Some("")` = contentless, `Some(tbl)` = external content.
    content: Option<String>,
    content_rowid: Option<String>,
    columnsize: Option<bool>,
    detail: Option<DetailMode>,
    contentless_delete: bool,
    contentless_unindexed: bool,
    locale: bool,
}

impl Fts5ConfigBuilder {
    /// `fts5ConfigParseSpecial`.
    fn parse_special(&mut self, cmd: &[u8], arg: &[u8]) -> Result<()> {
        if cfg_is_option("prefix", cmd) {
            let at = |i: usize| arg.get(i).copied().unwrap_or(0);
            let mut p = 0usize;
            let mut first = true;
            loop {
                while at(p) == b' ' {
                    p += 1;
                }
                if !first && at(p) == b',' {
                    p += 1;
                    while at(p) == b' ' {
                        p += 1;
                    }
                } else if at(p) == 0 {
                    break;
                }
                if !at(p).is_ascii_digit() {
                    return Err(cfg_err("malformed prefix=... directive"));
                }
                if self.prefixes.len() == FTS5_MAX_PREFIX_INDEXES {
                    return Err(cfg_err(format!(
                        "too many prefix indexes (max {FTS5_MAX_PREFIX_INDEXES})"
                    )));
                }
                let mut n_pre = 0usize;
                while at(p).is_ascii_digit() && n_pre < 1000 {
                    n_pre = n_pre * 10 + (at(p) - b'0') as usize;
                    p += 1;
                }
                if n_pre == 0 || n_pre >= 1000 {
                    return Err(cfg_err("prefix length out of range (max 999)"));
                }
                self.prefixes.push(n_pre);
                first = false;
            }
            return Ok(());
        }

        if cfg_is_option("tokenize", cmd) {
            if self.tokenize_args.is_some() {
                return Err(cfg_err("multiple tokenize=... directives"));
            }
            let mut args = Vec::new();
            let mut p = arg;
            while !p.is_empty() {
                let p2 = cfg_skip_ws(p);
                let len = if p2.first() == Some(&b'\'') {
                    cfg_literal_len(p2)
                } else {
                    cfg_bareword_len(p2)
                };
                let Some(len) = len else {
                    return Err(cfg_err("parse error in tokenize directive"));
                };
                args.push(String::from_utf8_lossy(&cfg_dequote_word(&p2[..len])).into_owned());
                p = cfg_skip_ws(&p2[len..]);
            }
            self.tokenize_args = Some(args);
            return Ok(());
        }

        if cfg_is_option("content", cmd) {
            if self.content.is_some() {
                return Err(cfg_err("multiple content=... directives"));
            }
            self.content = Some(String::from_utf8_lossy(arg).into_owned());
            return Ok(());
        }

        if cfg_is_option("contentless_delete", cmd) {
            self.contentless_delete = cfg_bool_arg(arg)
                .ok_or_else(|| cfg_err("malformed contentless_delete=... directive"))?;
            return Ok(());
        }

        if cfg_is_option("contentless_unindexed", cmd) {
            // (the C message names contentless_delete here as well)
            self.contentless_unindexed = cfg_bool_arg(arg)
                .ok_or_else(|| cfg_err("malformed contentless_delete=... directive"))?;
            return Ok(());
        }

        if cfg_is_option("content_rowid", cmd) {
            if self.content_rowid.is_some() {
                return Err(cfg_err("multiple content_rowid=... directives"));
            }
            self.content_rowid = Some(String::from_utf8_lossy(arg).into_owned());
            return Ok(());
        }

        if cfg_is_option("columnsize", cmd) {
            self.columnsize = Some(
                cfg_bool_arg(arg).ok_or_else(|| cfg_err("malformed columnsize=... directive"))?,
            );
            return Ok(());
        }

        if cfg_is_option("locale", cmd) {
            self.locale =
                cfg_bool_arg(arg).ok_or_else(|| cfg_err("malformed locale=... directive"))?;
            return Ok(());
        }

        if cfg_is_option("detail", cmd) {
            // `fts5ConfigSetEnum`: the value may be any unambiguous prefix of a name.
            let mut found: Option<DetailMode> = None;
            for (name, mode) in [
                ("none", DetailMode::None),
                ("full", DetailMode::Full),
                ("columns", DetailMode::Col),
            ] {
                if cfg_is_option(name, arg) {
                    if found.is_some() {
                        return Err(cfg_err("malformed detail=... directive"));
                    }
                    found = Some(mode);
                }
            }
            self.detail = Some(found.ok_or_else(|| cfg_err("malformed detail=... directive"))?);
            return Ok(());
        }

        if cfg_is_option("tokendata", cmd) {
            cfg_bool_arg(arg).ok_or_else(|| cfg_err("malformed tokendata=... directive"))?;
            return Ok(());
        }

        Err(cfg_err(format!(
            "unrecognized option: \"{}\"",
            String::from_utf8_lossy(cmd)
        )))
    }

    /// `fts5ConfigParseColumn`.
    fn parse_column(&mut self, col: &[u8], arg: Option<&[u8]>) -> Result<()> {
        let name = String::from_utf8_lossy(col).into_owned();
        if name.eq_ignore_ascii_case("rank") || name.eq_ignore_ascii_case("rowid") {
            return Err(cfg_err(format!("reserved fts5 column name: {name}")));
        }
        let mut unindexed = false;
        if let Some(arg) = arg {
            if arg.eq_ignore_ascii_case(b"unindexed") {
                unindexed = true;
            } else {
                return Err(cfg_err(format!(
                    "unrecognized column option: {}",
                    String::from_utf8_lossy(arg)
                )));
            }
        }
        self.columns.push(name);
        self.unindexed.push(unindexed);
        Ok(())
    }
}

impl Fts5Config {
    /// `sqlite3Fts5ConfigParse`.
    fn parse(table_name: &str, raw_args: &[String]) -> Result<Self> {
        if table_name.eq_ignore_ascii_case("rank") {
            return Err(cfg_err(format!("reserved fts5 table name: {table_name}")));
        }
        let mut b = Fts5ConfigBuilder::default();
        for raw in raw_args {
            let orig = raw.as_bytes();
            let parse_error = || {
                cfg_err(format!(
                    "parse error in \"{}\"",
                    String::from_utf8_lossy(orig)
                ))
            };
            let Some((one, must_be_col, rest)) = cfg_gobble_word(orig) else {
                return Err(parse_error());
            };
            let mut z = Some(cfg_skip_ws(rest));
            let mut is_option = false;
            if let Some(r) = z
                && r.first() == Some(&b'=')
            {
                is_option = true;
                z = if must_be_col { None } else { Some(&r[1..]) };
            }
            z = z.map(cfg_skip_ws);
            let mut two: Option<Vec<u8>> = None;
            if let Some(r) = z
                && !r.is_empty()
            {
                match cfg_gobble_word(r) {
                    Some((w, _, rest2)) => {
                        two = Some(w);
                        z = if rest2.is_empty() { Some(rest2) } else { None };
                    }
                    None => z = None,
                }
            }
            if z.is_none() {
                return Err(parse_error());
            }
            if is_option {
                b.parse_special(&one, two.as_deref().unwrap_or(b""))?;
            } else {
                b.parse_column(&one, two.as_deref())?;
            }
        }

        let contentless = b.content.as_deref() == Some("");
        if b.contentless_delete && !contentless {
            return Err(cfg_err("contentless_delete=1 requires a contentless table"));
        }
        let columnsize = b.columnsize.unwrap_or(true);
        if b.contentless_delete && !columnsize {
            return Err(cfg_err(
                "contentless_delete=1 is incompatible with columnsize=0",
            ));
        }
        if b.contentless_unindexed && !contentless {
            return Err(cfg_err(
                "contentless_unindexed=1 requires a contentless table",
            ));
        }

        // The hidden columns of the declared table (`sqlite3Fts5ConfigDeclareVtab`) make a
        // user column of the same name a duplicate.
        for (i, c) in b.columns.iter().enumerate() {
            if b.columns[..i].iter().any(|p| p.eq_ignore_ascii_case(c))
                || c.eq_ignore_ascii_case(table_name)
            {
                return Err(cfg_err(format!("duplicate column name: {c}")));
            }
        }

        let tokenizer = TokenizerSpec::load(b.tokenize_args.as_deref().unwrap_or(&[]))?;
        let content_mode = match b.content {
            None => ContentMode::Normal,
            Some(s) if s.is_empty() => ContentMode::Contentless,
            Some(tbl) => ContentMode::External {
                table: tbl,
                rowid_col: b.content_rowid,
            },
        };
        let unindexed_content = content_mode == ContentMode::Contentless
            && b.contentless_unindexed
            && b.unindexed.iter().any(|&u| u);

        Ok(Self {
            columns: b.columns,
            unindexed: b.unindexed,
            tokenizer,
            prefixes: b.prefixes,
            content_mode,
            columnsize,
            detail: b.detail.unwrap_or(DetailMode::Full),
            contentless_delete: b.contentless_delete,
            unindexed_content,
            locale: b.locale,
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

    /// True when the table has a `%_content` shadow table (normal content, or a contentless
    /// table that stores its UNINDEXED columns: C `FTS5_CONTENT_UNINDEXED`).
    fn has_content_table(&self) -> bool {
        self.content_mode == ContentMode::Normal || self.unindexed_content
    }

    /// Column definitions of `%_content` (`sqlite3Fts5StorageOpen`).
    fn content_table_defn(&self) -> String {
        let mut defn = String::from("id INTEGER PRIMARY KEY");
        for i in 0..self.columns.len() {
            if self.content_mode == ContentMode::Normal || self.unindexed[i] {
                defn.push_str(&format!(", c{i}"));
            }
        }
        if self.locale {
            for i in 0..self.columns.len() {
                if !self.unindexed[i] {
                    defn.push_str(&format!(", l{i}"));
                }
            }
        }
        defn
    }

    /// The `%_content` row for `col_vals` (`FTS5_STMT_INSERT_CONTENT` bindings).
    fn content_row(&self, rowid: i64, col_vals: &[Value]) -> Vec<Value> {
        let n = self.columns.len();
        let mut row = Vec::with_capacity(1 + 2 * n);
        row.push(Value::Integer(rowid));
        for i in 0..n {
            if self.content_mode == ContentMode::Normal || self.unindexed[i] {
                row.push(col_vals.get(i).cloned().unwrap_or(Value::Null));
            }
        }
        if self.locale {
            for i in 0..n {
                if !self.unindexed[i] {
                    row.push(Value::Null);
                }
            }
        }
        row
    }

    /// The SELECT list that reads the user columns back from `%_content`
    /// (`fts5ConfigMakeExprlist`, without the locale columns).
    fn content_select_list(&self) -> String {
        let mut cols = vec!["id".to_owned()];
        for i in 0..self.columns.len() {
            if self.content_mode == ContentMode::Normal || self.unindexed[i] {
                cols.push(format!("c{i}"));
            } else {
                cols.push("NULL".to_owned());
            }
        }
        cols.join(", ")
    }

    /// Maps a raw `%_content` row (as returned by `SELECT *`) to one value per user column.
    fn content_row_to_cols(&self, mut row: Vec<Value>) -> Vec<Value> {
        let n = self.columns.len();
        if self.content_mode == ContentMode::Normal {
            if !row.is_empty() {
                row.drain(..1);
            }
            row.truncate(n);
            row.resize(n, Value::Null);
            return row;
        }
        let mut it = row.into_iter().skip(1);
        (0..n)
            .map(|i| {
                if self.unindexed[i] {
                    it.next().unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            })
            .collect()
    }
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
    v2: Option<SegOrigin>,
    cookie: u32,
    segid: u64,
    pgno_last: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&cookie.to_be_bytes());
    if v2.is_some() {
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
    if let Some(origin) = v2 {
        put_uvarint(&mut out, origin.origin1);
        put_uvarint(&mut out, origin.origin2);
        put_uvarint(&mut out, 0); // nPgTombstone
        put_uvarint(&mut out, 0); // nEntryTombstone
        put_uvarint(&mut out, origin.n_entry);
    }
    out
}

/// Origin range and row count of a segment of a `contentless_delete=1` table (V2 structure
/// record, `Fts5StructureSegment.iOrigin1/iOrigin2/nEntry`).
#[derive(Debug, Clone, Copy)]
struct SegOrigin {
    origin1: u64,
    origin2: u64,
    n_entry: u64,
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

/// True if a pending posting payload carries the delete flag: a pure delete (`01`, or `00`
/// with detail=none) or an entry already marked by [`payload_with_delete_flag`].
fn payload_has_delete_flag(detail: DetailMode, payload: &[u8]) -> bool {
    match detail {
        DetailMode::None => payload.first() == Some(&0x00),
        DetailMode::Col | DetailMode::Full => {
            // The size field is a varint; its last byte holds the low bits.
            let mut i = 0;
            while i < payload.len() && i < 8 && payload[i] & 0x80 != 0 {
                i += 1;
            }
            payload.get(i).is_some_and(|b| b & 0x01 != 0)
        }
    }
}

/// `fts5HashAddPoslistSize` (fts5_hash.c): a row that is deleted and written again within
/// one flush keeps the delete flag on its new entry - the size field becomes `nPos*2 + 1`,
/// or with detail=none the entry is the two bytes `00 00`. The flag says "this entry hides
/// an older one". C's secure-delete (`fts5DoSecureDelete`) depends on it: an entry without
/// the flag is removed outright, an entry with it is turned into a delete marker, so a
/// missing flag lets the older entry of the row reappear.
fn payload_with_delete_flag(detail: DetailMode, payload: &[u8]) -> SmallBytes22 {
    match detail {
        DetailMode::None => {
            if payload.is_empty() {
                SmallBytes22::from_slice(&[0x00, 0x00])
            } else {
                SmallBytes22::from_slice(payload)
            }
        }
        DetailMode::Col | DetailMode::Full => {
            let mut v = payload.to_vec();
            let mut i = 0;
            while i < v.len() && i < 8 && v[i] & 0x80 != 0 {
                i += 1;
            }
            if let Some(b) = v.get_mut(i) {
                *b |= 0x01;
            }
            SmallBytes22::from_slice(&v)
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

/// `fts5PoslistPrefix`: number of bytes of whole leading varints of `buf` that fit in
/// `n_max` bytes (always at least the first varint).
fn poslist_prefix_len(buf: &[u8], n_max: usize) -> usize {
    let varint_len = |at: usize| -> usize {
        let mut pos = at;
        match get_uvarint(buf, &mut pos) {
            Some(_) => pos - at,
            None => buf.len() - at,
        }
    };
    let mut ret = varint_len(0);
    if ret < n_max {
        while ret < buf.len() {
            let i = varint_len(ret);
            if ret + i > n_max {
                break;
            }
            ret += i;
        }
    }
    ret.max(1).min(buf.len())
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
            if page_buf.len() + pgidx.len() + poslist.len() <= FTS5_LEAF_MAX_PGSZ {
                page_buf.extend_from_slice(poslist);
            } else {
                // The position list does not fit on this leaf (`fts5FlushOneHash`): break it
                // into sections, keeping every varint contiguous, and continue on term-less
                // leaves. The 16-bit leaf header fields cannot describe a larger leaf.
                let mut rest = poslist;
                loop {
                    let space = FTS5_LEAF_MAX_PGSZ.saturating_sub(page_buf.len() + pgidx.len());
                    let n = if rest.len() <= space {
                        rest.len()
                    } else {
                        poslist_prefix_len(rest, space)
                    };
                    page_buf.extend_from_slice(&rest[..n]);
                    rest = &rest[n..];
                    if rest.is_empty() {
                        break;
                    }
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
            }
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
    origin: Option<SegOrigin>,
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
    let origin = if cfg.contentless_delete {
        Some(origin.unwrap_or(SegOrigin {
            origin1: 1,
            origin2: 1,
            n_entry: 0,
        }))
    } else {
        None
    };
    let sblob =
        encode_single_seg_structure_record_with_cookie(origin, next_cookie, segid, pgno_last);
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
    n_entry_tombstone: u64,
    n_entry: u64,
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

    /// `Fts5Structure.nOriginCntr` of a V2 structure: the origin the next level-0 segment
    /// gets (`fts5StructureDecode`: largest `iOrigin2` + 1).
    fn origin_counter(&self) -> u64 {
        self.levels
            .iter()
            .flat_map(|(_, s)| s.iter().map(|r| r.origin2))
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Smallest `iOrigin1` of any segment (`None` for an empty structure).
    fn min_origin(&self) -> Option<u64> {
        self.levels
            .iter()
            .flat_map(|(_, s)| s.iter().map(|r| r.origin1))
            .min()
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
                    put_uvarint(&mut out, seg.n_entry_tombstone);
                    put_uvarint(&mut out, seg.n_entry);
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
            // V2 (contentless_delete=1) segments carry five more varints
            // (`fts5StructureDecode`): iOrigin1, iOrigin2, nPgTombstone, nEntryTombstone, nEntry.
            let (origin1, origin2, n_tombstone_pages, n_entry_tombstone, n_entry) = if is_v2 {
                let o1 = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                let o2 = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                let ntp = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                let net = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                let ne = get_uvarint(blob, &mut pos).ok_or_else(err_corrupt)?;
                (o1, o2, ntp, net, ne)
            } else {
                (0, 0, 0, 0, 0)
            };
            lsegs.push(Fts5SegRecord {
                segid,
                pgno_first,
                pgno_last,
                n_tombstone_pages,
                origin1,
                origin2,
                n_entry_tombstone,
                n_entry,
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

fn read_full_structure(
    ctx: &mut dyn VTabHostContext,
    data_shadow: &str,
) -> Result<FullStructureRecord> {
    let srow = ctx.shadow_get_rowid(data_shadow, FTS5_STRUCTURE_ROWID)?;
    let Some(Value::Blob(sblob)) = srow.as_ref().and_then(|r| r.get(1)) else {
        return Err(SqliteError::new(
            ErrorCode::Corrupt,
            "database disk image is malformed",
        ));
    };
    decode_full_structure(sblob)
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

        // `fts5SegIterLoadNPos` (fts5_index.c): the size field is `nPos*2 + bDel`. An entry
        // always replaces the entries for the same (term, rowid) in older segments; it only
        // removes the row from the term when it carries no positions (`nPos == 0`). The
        // delete flag on an entry that does have positions (delete + re-insert of a rowid
        // inside one flush, i.e. every UPDATE / REPLACE written by C) does not hide it.
        match detail {
            DetailMode::None => {
                // detail=none: no byte = present, `00` = deleted, `00 00` = deleted and
                // re-inserted (present).
                let mut empty = false;
                if pos < slice_end && block[pos] == 0 {
                    pos += 1;
                    if pos < slice_end && block[pos] == 0 {
                        pos += 1;
                    } else {
                        empty = true;
                    }
                }
                apply_posting(*cur_rowid, empty, &[])?;
            }
            DetailMode::Col | DetailMode::Full => {
                let sz_flags = get_uvarint(block, &mut pos).ok_or_else(err_corrupt)? as usize;
                let plen = sz_flags >> 1;
                let empty = plen == 0;
                let avail = slice_end.saturating_sub(pos);
                if plen <= avail {
                    let pbody = &block[pos..pos + plen];
                    pos += plen;
                    apply_posting(*cur_rowid, empty, pbody)?;
                } else {
                    if !continues_to_next_page {
                        return Err(err_corrupt());
                    }
                    let mut pbuf = Vec::with_capacity(plen.min(65_536));
                    pbuf.extend_from_slice(&block[pos..slice_end]);
                    *pending_poslist = Some((empty, plen - avail, pbuf));
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
    db_q: &str,
    tbl_name: &str,
    detail: DetailMode,
) -> Result<MergedIndexMap> {
    load_merged_index_with_deletes(ctx, db_q, tbl_name, detail, &[])
}

/// `load_merged_index` with additional tombstones: `(origin, rowid)` pairs hide `rowid` in
/// every segment whose origin range contains `origin` (contentless_delete=1 deletes that
/// are about to be applied).
fn load_merged_index_with_deletes(
    ctx: &mut dyn VTabHostContext,
    _db_q: &str,
    tbl_name: &str,
    detail: DetailMode,
    origin_deletes: &[(u64, i64)],
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
        let mut tombstones = if seg.n_tombstone_pages > 0 {
            load_segment_tombstones(ctx, &data_shadow, seg.segid, seg.n_tombstone_pages)?
        } else {
            BTreeSet::new()
        };
        for &(origin, rowid) in origin_deletes {
            if seg.origin1 <= origin && origin <= seg.origin2 {
                tombstones.insert(rowid);
            }
        }

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

fn make_term_key_bytes(prefix_slot: u8, term: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + term.len());
    key.push(b'0' + prefix_slot);
    key.extend_from_slice(term);
    key
}

/// `sqlite3Fts5IndexCharlenToBytelen`: number of bytes taken by the first `n_char`
/// characters of `p`, or 0 if `p` has fewer characters.
fn charlen_to_bytelen(p: &[u8], n_char: usize) -> usize {
    let n_byte = p.len();
    let mut n = 0usize;
    for i in 0..n_char {
        if n >= n_byte {
            return 0;
        }
        let c = p[n];
        n += 1;
        if c >= 0xc0 {
            if n >= n_byte {
                return 0;
            }
            while (p[n] & 0xc0) == 0x80 {
                n += 1;
                if n >= n_byte {
                    if i + 1 == n_char {
                        break;
                    }
                    return 0;
                }
            }
        }
    }
    n
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
        if cfg.unindexed[col_idx] {
            continue;
        }
        let Some(text) = fts5_value_bytes(val) else {
            continue;
        };
        let mut tok_pos = 0usize;
        cfg.tokenizer.for_each_doc_token(&text, |term| {
            let main_key = make_term_key_bytes(0, term);
            record_posting_hit(
                &mut term_cols,
                main_key,
                col_idx,
                tok_pos,
                cfg.detail,
                deleted,
            );
            for (p_idx, &plen) in cfg.prefixes.iter().enumerate() {
                let n_byte = charlen_to_bytelen(term, plen);
                if n_byte > 0 {
                    let p_key = make_term_key_bytes((p_idx + 1) as u8, &term[..n_byte]);
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
            tok_pos += 1;
        });
        col_sizes[col_idx] = tok_pos as u64;
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
        if cfg.unindexed[col_idx] {
            continue;
        }
        let Some(text) = fts5_value_bytes(val) else {
            continue;
        };
        let col_u32 = col_idx as u32;
        let mut tok_pos = 0u32;
        cfg.tokenizer.for_each_doc_token(&text, |term| {
            key_buf.clear();
            key_buf.push(b'0');
            key_buf.extend_from_slice(term);
            hit_key(&mut term_acc, &key_buf, col_u32, tok_pos);

            // `sqlite3Fts5IndexWrite`: one entry per prefix index the token is long enough for.
            for (p_idx, &plen) in cfg.prefixes.iter().enumerate() {
                let n_byte = charlen_to_bytelen(term, plen);
                if n_byte > 0 {
                    key_buf.clear();
                    key_buf.push(b'0'.wrapping_add((p_idx + 1) as u8));
                    key_buf.extend_from_slice(&term[..n_byte]);
                    hit_key(&mut term_acc, &key_buf, col_u32, tok_pos);
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

/// One phrase of a query (`Fts5ExprPhrase`).
#[derive(Debug, Clone, Default)]
struct QueryPhrase {
    tokens: Vec<QueryPhraseToken>,
    /// `aTerm[0].bFirst`: the phrase must start at the first token of a column (`^`).
    anchored_start: bool,
}

/// A NEAR group of phrases with an optional column filter (`Fts5ExprNearset` together with
/// its `FTS5_STRING` / `FTS5_TERM` / `FTS5_EOF` node).
#[derive(Debug, Clone)]
struct QueryNearset {
    /// Index of the first phrase of the group in the query's phrase list; assigned by
    /// `number_phrases` once parsing is finished.
    first_phrase: usize,
    phrases: Vec<QueryPhrase>,
    /// `nNear`.
    distance: i64,
    /// `pColset` (sorted column numbers).
    colset: Option<Vec<usize>>,
    /// An `FTS5_EOF` node: a phrase without terms or an empty column set. Never matches.
    eof: bool,
}

#[derive(Debug, Clone)]
enum FtsQueryExpr {
    Near(QueryNearset),
    And(Vec<FtsQueryExpr>),
    Or(Vec<FtsQueryExpr>),
    Not(Box<FtsQueryExpr>, Box<FtsQueryExpr>),
}

impl FtsQueryExpr {
    fn is_eof(&self) -> bool {
        matches!(self, Self::Near(n) if n.eof)
    }

    /// `Fts5ExprNode.iHeight`.
    fn height(&self) -> usize {
        match self {
            Self::Near(_) => 0,
            Self::And(ch) | Self::Or(ch) => 1 + ch.iter().map(Self::height).max().unwrap_or(0),
            Self::Not(l, r) => 1 + l.height().max(r.height()),
        }
    }

    /// Assigns phrase numbers in query order and returns the phrases.
    fn number_phrases(&mut self, out: &mut Vec<QueryPhrase>) {
        match self {
            Self::Near(n) => {
                n.first_phrase = out.len();
                out.extend(n.phrases.iter().cloned());
            }
            Self::And(ch) | Self::Or(ch) => {
                for c in ch {
                    c.number_phrases(out);
                }
            }
            Self::Not(l, r) => {
                l.number_phrases(out);
                r.number_phrases(out);
            }
        }
    }

    /// `fts5ParseSetColset`: restricts every phrase group below this node to `colset`.
    fn set_colset(&mut self, colset: &[usize]) {
        match self {
            Self::Near(n) => match &mut n.colset {
                Some(cur) => {
                    cur.retain(|c| colset.contains(c));
                    if cur.is_empty() {
                        n.eof = true;
                    }
                }
                None => n.colset = Some(colset.to_vec()),
            },
            Self::And(ch) | Self::Or(ch) => {
                for c in ch {
                    c.set_colset(colset);
                }
            }
            Self::Not(l, r) => {
                l.set_colset(colset);
                r.set_colset(colset);
            }
        }
    }
}

/// Token types of the FTS5 query language (`fts5ExprGetToken`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QTok {
    Lp,
    Rp,
    Lcp,
    Rcp,
    Colon,
    Comma,
    Plus,
    Star,
    Minus,
    Caret,
    Eof,
    Or,
    Not,
    And,
    Str,
}

fn fts5_query_err(msg: impl Into<String>) -> SqliteError {
    SqliteError::new(ErrorCode::Error, msg.into())
}

/// Recursive-descent equivalent of the `fts5parse.y` grammar:
///
/// ```text
/// expr     ::= expr OR expr | expr AND expr | expr NOT expr      (OR < AND < NOT)
///            | colset COLON LP expr RP | LP expr RP | exprlist
/// exprlist ::= cnearset | exprlist cnearset                      (implicit AND)
/// cnearset ::= nearset | colset COLON nearset
/// nearset  ::= phrase | CARET phrase | STRING LP nearphrases [COMMA STRING] RP
/// phrase   ::= STRING [STAR] | phrase PLUS STRING [STAR]
/// colset   ::= [MINUS] STRING | [MINUS] LCP STRING... RCP
/// ```
struct QueryParser<'a> {
    cfg: &'a Fts5Config,
    z: &'a [u8],
    pos: usize,
    tok: QTok,
    tok_text: &'a [u8],
    /// Number of grammar symbols on the stack of the equivalent LALR parser; the generated
    /// C parser has room for 99 (`fts5: parser stack overflow`).
    stack: usize,
}

/// Symbols the lemon-generated FTS5 parser can hold (`YYSTACKDEPTH` 100 minus the base entry).
const FTS5_PARSER_STACK_MAX: usize = 99;

impl<'a> QueryParser<'a> {
    /// `sqlite3Fts5ExprNew`: parses `text` and numbers its phrases.
    fn parse_query(cfg: &'a Fts5Config, text: &'a str) -> Result<(FtsQueryExpr, Vec<QueryPhrase>)> {
        let mut p = Self {
            cfg,
            z: text.as_bytes(),
            pos: 0,
            tok: QTok::Eof,
            tok_text: b"",
            stack: 0,
        };
        p.advance()?;
        let mut expr = if p.tok == QTok::Eof {
            // An empty query is a syntax error at the end-of-input token.
            return Err(p.syntax_error());
        } else {
            p.parse_or()?
        };
        if p.tok != QTok::Eof {
            return Err(p.syntax_error());
        }
        let mut phrases = Vec::new();
        expr.number_phrases(&mut phrases);
        Ok((expr, phrases))
    }

    /// `fts5ExprGetToken`.
    fn advance(&mut self) -> Result<()> {
        let z = self.z;
        let mut i = self.pos;
        while i < z.len() && matches!(z[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        if i >= z.len() || z[i] == 0 {
            self.tok = QTok::Eof;
            self.tok_text = b"";
            self.pos = z.len();
            return Ok(());
        }
        let start = i;
        let simple = match z[i] {
            b'(' => Some(QTok::Lp),
            b')' => Some(QTok::Rp),
            b'{' => Some(QTok::Lcp),
            b'}' => Some(QTok::Rcp),
            b':' => Some(QTok::Colon),
            b',' => Some(QTok::Comma),
            b'+' => Some(QTok::Plus),
            b'*' => Some(QTok::Star),
            b'-' => Some(QTok::Minus),
            b'^' => Some(QTok::Caret),
            _ => None,
        };
        if let Some(t) = simple {
            self.tok = t;
            self.tok_text = &z[start..start + 1];
            self.pos = start + 1;
            return Ok(());
        }
        if z[i] == b'"' {
            i += 1;
            loop {
                if i < z.len() && z[i] == b'"' {
                    i += 1;
                    if i >= z.len() || z[i] != b'"' {
                        break;
                    }
                }
                if i >= z.len() || z[i] == 0 {
                    return Err(fts5_query_err("unterminated string"));
                }
                i += 1;
            }
            self.tok = QTok::Str;
        } else {
            if !fts5_is_bareword(z[i]) {
                return Err(fts5_query_err(format!(
                    "fts5: syntax error near \"{}\"",
                    String::from_utf8_lossy(&z[i..i + 1])
                )));
            }
            while i < z.len() && fts5_is_bareword(z[i]) {
                i += 1;
            }
            self.tok = match &z[start..i] {
                b"OR" => QTok::Or,
                b"NOT" => QTok::Not,
                b"AND" => QTok::And,
                _ => QTok::Str,
            };
        }
        self.tok_text = &z[start..i];
        self.pos = i;
        Ok(())
    }

    /// A shift (or the reduction of an empty rule) in the LALR parser: one more symbol.
    fn push(&mut self) -> Result<()> {
        if self.stack >= FTS5_PARSER_STACK_MAX {
            return Err(fts5_query_err("fts5: parser stack overflow"));
        }
        self.stack += 1;
        Ok(())
    }

    /// A reduction that replaces `n + 1` symbols by one.
    fn pop(&mut self, n: usize) {
        self.stack = self.stack.saturating_sub(n);
    }

    /// The lemon `%syntax_error` action.
    fn syntax_error(&self) -> SqliteError {
        fts5_query_err(format!(
            "fts5: syntax error near \"{}\"",
            String::from_utf8_lossy(self.tok_text)
        ))
    }

    /// `sqlite3Fts5ParseNode` for AND / OR / NOT: children of the same type are merged into
    /// one node and the tree height is limited.
    fn make_node(kind: QTok, left: FtsQueryExpr, right: FtsQueryExpr) -> Result<FtsQueryExpr> {
        let node = match kind {
            QTok::Not => FtsQueryExpr::Not(Box::new(left), Box::new(right)),
            QTok::And => {
                let mut ch = Vec::new();
                for side in [left, right] {
                    match side {
                        FtsQueryExpr::And(c) => ch.extend(c),
                        other => ch.push(other),
                    }
                }
                FtsQueryExpr::And(ch)
            }
            _ => {
                let mut ch = Vec::new();
                for side in [left, right] {
                    match side {
                        FtsQueryExpr::Or(c) => ch.extend(c),
                        other => ch.push(other),
                    }
                }
                FtsQueryExpr::Or(ch)
            }
        };
        if node.height() > FTS5_MAX_EXPR_DEPTH {
            return Err(fts5_query_err(format!(
                "fts5 expression tree is too large (maximum depth {FTS5_MAX_EXPR_DEPTH})"
            )));
        }
        Ok(node)
    }

    fn parse_or(&mut self) -> Result<FtsQueryExpr> {
        let mut lhs = self.parse_and()?;
        while self.tok == QTok::Or {
            self.push()?;
            self.advance()?;
            let rhs = self.parse_and()?;
            self.pop(2);
            lhs = Self::make_node(QTok::Or, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<FtsQueryExpr> {
        let mut lhs = self.parse_not()?;
        while self.tok == QTok::And {
            self.push()?;
            self.advance()?;
            let rhs = self.parse_not()?;
            self.pop(2);
            lhs = Self::make_node(QTok::And, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<FtsQueryExpr> {
        let mut lhs = self.parse_primary()?;
        while self.tok == QTok::Not {
            self.push()?;
            self.advance()?;
            let rhs = self.parse_primary()?;
            self.pop(2);
            lhs = Self::make_node(QTok::Not, lhs, rhs)?;
        }
        Ok(lhs)
    }

    /// True if the next two tokens are `STRING COLON` (a one-column colset).
    fn string_then_colon(&self) -> bool {
        let mut look = Self {
            cfg: self.cfg,
            z: self.z,
            pos: self.pos,
            tok: self.tok,
            tok_text: self.tok_text,
            stack: 0,
        };
        look.advance().is_ok() && look.tok == QTok::Colon
    }

    fn at_colset_start(&self) -> bool {
        matches!(self.tok, QTok::Minus | QTok::Lcp)
            || (self.tok == QTok::Str && self.string_then_colon())
    }

    /// `LP expr RP`, `colset COLON LP expr RP` or an `exprlist`.
    fn parse_primary(&mut self) -> Result<FtsQueryExpr> {
        if self.tok == QTok::Lp {
            self.push()?;
            self.advance()?;
            let inner = self.parse_or()?;
            if self.tok != QTok::Rp {
                return Err(self.syntax_error());
            }
            self.push()?;
            self.advance()?;
            self.pop(2);
            return Ok(inner);
        }
        let mut list: Option<FtsQueryExpr> = None;
        loop {
            let node = if self.at_colset_start() {
                let colset = self.parse_colset()?;
                if self.tok != QTok::Colon {
                    return Err(self.syntax_error());
                }
                self.push()?;
                self.advance()?;
                if self.tok == QTok::Lp {
                    // `colset : ( expr )` is an expression of its own, not part of a list.
                    if list.is_some() {
                        return Err(self.syntax_error());
                    }
                    self.push()?;
                    self.advance()?;
                    let mut inner = self.parse_or()?;
                    if self.tok != QTok::Rp {
                        return Err(self.syntax_error());
                    }
                    self.push()?;
                    self.advance()?;
                    self.pop(4);
                    self.apply_colset(&mut inner, &colset)?;
                    return Ok(inner);
                }
                let mut node = self.parse_nearset()?;
                self.pop(2);
                self.apply_colset(&mut node, &colset)?;
                node
            } else if matches!(self.tok, QTok::Str | QTok::Caret) {
                self.parse_nearset()?
            } else if list.is_none() {
                return Err(self.syntax_error());
            } else {
                break;
            };
            list = Some(match list {
                None => node,
                Some(left) => {
                    self.pop(1);
                    Self::implicit_and(left, node)?
                }
            });
            if !(matches!(self.tok, QTok::Str | QTok::Caret) || self.at_colset_start()) {
                break;
            }
        }
        list.ok_or_else(|| self.syntax_error())
    }

    /// `sqlite3Fts5ParseImplicitAnd`: phrases without terms disappear from a list.
    fn implicit_and(left: FtsQueryExpr, right: FtsQueryExpr) -> Result<FtsQueryExpr> {
        if right.is_eof() {
            return Ok(left);
        }
        match left {
            FtsQueryExpr::And(mut ch) => {
                if ch.last().is_some_and(FtsQueryExpr::is_eof) {
                    ch.pop();
                    ch.push(right);
                    Ok(FtsQueryExpr::And(ch))
                } else {
                    Self::make_node(QTok::And, FtsQueryExpr::And(ch), right)
                }
            }
            other if other.is_eof() => Ok(right),
            other => Self::make_node(QTok::And, other, right),
        }
    }

    /// `sqlite3Fts5ParseSetColset`.
    fn apply_colset(&self, node: &mut FtsQueryExpr, colset: &[usize]) -> Result<()> {
        if self.cfg.detail == DetailMode::None {
            return Err(fts5_query_err(
                "fts5: column queries are not supported (detail=none)",
            ));
        }
        node.set_colset(colset);
        Ok(())
    }

    /// `sqlite3Fts5ParseColset`: the column number of a (possibly quoted) column name.
    fn column_number(&self, raw: &[u8]) -> Result<usize> {
        let name = String::from_utf8_lossy(&cfg_dequote_word(raw)).into_owned();
        self.cfg
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&name))
            .ok_or_else(|| fts5_query_err(format!("no such column: {name}")))
    }

    fn parse_colset(&mut self) -> Result<Vec<usize>> {
        let base = self.stack;
        let invert = self.tok == QTok::Minus;
        if invert {
            self.push()?;
            self.advance()?;
        }
        let mut cols: Vec<usize> = Vec::new();
        if self.tok == QTok::Lcp {
            self.push()?;
            self.advance()?;
            if self.tok != QTok::Str {
                return Err(self.syntax_error());
            }
            let list_base = self.stack;
            while self.tok == QTok::Str {
                self.push()?;
                let c = self.column_number(self.tok_text)?;
                if !cols.contains(&c) {
                    cols.push(c);
                }
                self.advance()?;
                self.stack = list_base + 1;
            }
            if self.tok != QTok::Rcp {
                return Err(self.syntax_error());
            }
            self.push()?;
            self.advance()?;
        } else if self.tok == QTok::Str {
            self.push()?;
            cols.push(self.column_number(self.tok_text)?);
            self.advance()?;
        } else {
            return Err(self.syntax_error());
        }
        self.stack = base + 1;
        cols.sort_unstable();
        if invert {
            // `sqlite3Fts5ParseColsetInvert`
            cols = (0..self.cfg.columns.len())
                .filter(|c| !cols.contains(c))
                .collect();
        }
        Ok(cols)
    }

    /// `phrase ::= STRING [STAR] (PLUS STRING [STAR])*` (`sqlite3Fts5ParseTerm`).
    fn parse_phrase(&mut self) -> Result<QueryPhrase> {
        let mut phrase = QueryPhrase::default();
        let base = self.stack;
        loop {
            if self.tok != QTok::Str {
                return Err(self.syntax_error());
            }
            self.push()?;
            let text = cfg_dequote_word(self.tok_text);
            self.advance()?;
            let is_prefix = self.tok == QTok::Star;
            // STAR is shifted, an absent one is the empty `star_opt` rule: one symbol each way
            self.push()?;
            if is_prefix {
                self.advance()?;
            }
            self.stack = base + 1;
            self.cfg.tokenizer.tok.tokenize(&text, &mut |tok, _, _| {
                phrase.tokens.push(QueryPhraseToken {
                    term: token_bytes_to_string(tok),
                    is_prefix: false,
                });
            });
            // The prefix flag always lands on the last term of the phrase so far.
            if let Some(last) = phrase.tokens.last_mut() {
                last.is_prefix = is_prefix;
            }
            if self.tok == QTok::Plus {
                self.push()?;
                self.advance()?;
            } else {
                break;
            }
        }
        Ok(phrase)
    }

    /// `nearset` followed by `sqlite3Fts5ParseNode(FTS5_STRING)`.
    fn parse_nearset(&mut self) -> Result<FtsQueryExpr> {
        let mut near = QueryNearset {
            first_phrase: 0,
            phrases: Vec::new(),
            distance: 10,
            colset: None,
            eof: false,
        };
        let mut is_near_group = false;
        let base = self.stack;
        if self.tok == QTok::Caret {
            self.push()?;
            self.advance()?;
            let mut phrase = self.parse_phrase()?;
            // `sqlite3Fts5ParseSetCaret`
            phrase.anchored_start = !phrase.tokens.is_empty();
            near.phrases.push(phrase);
        } else if self.tok == QTok::Str && {
            let mut look = Self {
                cfg: self.cfg,
                z: self.z,
                pos: self.pos,
                tok: self.tok,
                tok_text: self.tok_text,
                stack: 0,
            };
            look.advance().is_ok() && look.tok == QTok::Lp
        } {
            // STRING LP nearphrases neardist_opt RP
            let name = self.tok_text;
            self.push()?;
            self.advance()?;
            self.push()?;
            self.advance()?;
            if self.tok != QTok::Str {
                return Err(self.syntax_error());
            }
            let list_base = self.stack;
            while self.tok == QTok::Str {
                let phrase = self.parse_phrase()?;
                self.stack = list_base + 1;
                // `sqlite3Fts5ParseNearset`: a phrase without terms is dropped from a group
                // that has another phrase.
                if near.phrases.is_empty() {
                    near.phrases.push(phrase);
                } else if phrase.tokens.is_empty() {
                    // dropped
                } else if near.phrases.last().is_some_and(|l| l.tokens.is_empty()) {
                    near.phrases.pop();
                    near.phrases.push(phrase);
                } else {
                    near.phrases.push(phrase);
                }
            }
            let mut dist_tok: Option<&[u8]> = None;
            if self.tok == QTok::Comma {
                self.push()?;
                self.advance()?;
                if self.tok != QTok::Str {
                    return Err(self.syntax_error());
                }
                self.push()?;
                dist_tok = Some(self.tok_text);
                self.advance()?;
                self.pop(1);
            } else {
                self.push()?;
            }
            if self.tok != QTok::Rp {
                return Err(self.syntax_error());
            }
            self.push()?;
            self.advance()?;
            // `sqlite3Fts5ParseNear`
            if name != b"NEAR" {
                return Err(fts5_query_err(format!(
                    "fts5: syntax error near \"{}\"",
                    String::from_utf8_lossy(name)
                )));
            }
            // `sqlite3Fts5ParseSetDistance`
            if let Some(d) = dist_tok {
                let mut n_near: i64 = 0;
                for &c in d {
                    if !c.is_ascii_digit() {
                        return Err(fts5_query_err(format!(
                            "expected integer, got \"{}\"",
                            String::from_utf8_lossy(d)
                        )));
                    }
                    if n_near < 214_748_363 {
                        n_near = n_near * 10 + i64::from(c - b'0');
                    }
                }
                near.distance = n_near;
            }
            is_near_group = true;
        } else {
            let phrase = self.parse_phrase()?;
            near.phrases.push(phrase);
        }

        // `sqlite3Fts5ParseNode(FTS5_STRING)`
        near.eof = near.phrases.iter().any(|p| p.tokens.is_empty());
        if self.cfg.detail != DetailMode::Full {
            let first = &near.phrases[0];
            if near.phrases.len() != 1
                || first.tokens.len() > 1
                || (!first.tokens.is_empty() && first.anchored_start)
            {
                return Err(fts5_query_err(format!(
                    "fts5: {} queries are not supported (detail!=full)",
                    if near.phrases.len() == 1 {
                        "phrase"
                    } else {
                        "NEAR"
                    }
                )));
            }
        }
        let _ = is_near_group;
        self.stack = base + 1;
        Ok(FtsQueryExpr::Near(near))
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

/// `FTS5_LOOKAHEAD_EOF`.
const FTS5_LOOKAHEAD_EOF: i64 = 1 << 62;

/// `Fts5LookaheadReader` over a sorted position list.
struct LookaheadReader<'a> {
    a: &'a [i64],
    i: usize,
    pos: i64,
    lookahead: i64,
}

impl<'a> LookaheadReader<'a> {
    fn new(a: &'a [i64]) -> Self {
        let mut r = Self {
            a,
            i: 0,
            pos: 0,
            lookahead: 0,
        };
        r.next();
        r.next();
        r
    }

    /// `fts5LookaheadReaderNext`: true when the reader is exhausted.
    fn next(&mut self) -> bool {
        self.pos = self.lookahead;
        if self.i < self.a.len() {
            self.lookahead = self.a[self.i];
            self.i += 1;
        } else {
            self.lookahead = FTS5_LOOKAHEAD_EOF;
        }
        self.pos == FTS5_LOOKAHEAD_EOF
    }
}

/// `fts5ExprNearIsMatch`: `lists[i]` holds the positions (`column << 32 | offset`) of phrase
/// `i` in one row. Returns the positions that take part in a NEAR match, one list per
/// phrase, or `None` if the row does not match.
fn near_is_match(lists: &[Vec<i64>], n_terms: &[i64], n_near: i64) -> Option<Vec<Vec<i64>>> {
    let n = lists.len();
    let mut readers: Vec<LookaheadReader<'_>> =
        lists.iter().map(|l| LookaheadReader::new(l)).collect();
    let mut out: Vec<Vec<i64>> = vec![Vec::new(); n];
    'outer: loop {
        // Advance the phrase iterators until they point to a set of entries that together
        // comprise a match.
        let mut i_max = readers[0].pos;
        loop {
            let mut is_match = true;
            for i in 0..n {
                let i_min = i_max - n_terms[i] - n_near;
                if readers[i].pos < i_min || readers[i].pos > i_max {
                    is_match = false;
                    while readers[i].pos < i_min {
                        if readers[i].next() {
                            break 'outer;
                        }
                    }
                    if readers[i].pos > i_max {
                        i_max = readers[i].pos;
                    }
                }
            }
            if is_match {
                break;
            }
        }
        // Add an entry to each output position list.
        for i in 0..n {
            let pos = readers[i].pos;
            if out[i].last() != Some(&pos) {
                out[i].push(pos);
            }
        }
        let mut i_adv = 0usize;
        let mut i_min = readers[0].lookahead;
        for (i, r) in readers.iter().enumerate() {
            if r.lookahead < i_min {
                i_min = r.lookahead;
                i_adv = i;
            }
        }
        if readers[i_adv].next() {
            break;
        }
    }
    (!out[0].is_empty()).then_some(out)
}

/// Evaluates a query tree against the per-phrase match sets (`fts5ExprNodeTest_*`).
///
/// `precomputed_phrases[i]` holds the rows phrase `i` matches with every phrase position;
/// `phrase_doc_hits[i]` receives the number of rows the phrase matches on its own within
/// the column set of its group (`xQueryPhrase`, used by `bm25()`). A returned row carries
/// the positions of the phrases whose node is positioned on it (`sqlite3Fts5ExprPoslist`).
fn eval_query_expr_precomputed(
    expr: &FtsQueryExpr,
    phrases: &[QueryPhrase],
    precomputed_phrases: &mut [EvalDocSet],
    phrase_doc_hits: &mut [usize],
) -> EvalDocSet {
    match expr {
        FtsQueryExpr::Near(near) => {
            let mut per_phrase: Vec<EvalDocSet> = Vec::with_capacity(near.phrases.len());
            for k in 0..near.phrases.len() {
                let pi = near.first_phrase + k;
                let mut docs = precomputed_phrases
                    .get_mut(pi)
                    .map(std::mem::take)
                    .unwrap_or_default();
                if let Some(allowed) = &near.colset {
                    docs.retain_mut(|(_, insts)| {
                        insts.retain(|inst| allowed.contains(&(inst.col_idx as usize)));
                        !insts.is_empty()
                    });
                }
                if let Some(slot) = phrase_doc_hits.get_mut(pi) {
                    *slot = docs.len();
                }
                per_phrase.push(docs);
            }
            if near.eof || per_phrase.iter().any(Vec::is_empty) {
                return Vec::new();
            }
            if per_phrase.len() == 1 {
                return per_phrase.pop().unwrap_or_default();
            }
            let n_terms: Vec<i64> = near.phrases.iter().map(|p| p.tokens.len() as i64).collect();
            let Some((first, rest)) = per_phrase.split_first() else {
                return Vec::new();
            };
            let _ = phrases;
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
                let to_list = |insts: &PhraseHits| -> Vec<i64> {
                    let mut v: Vec<i64> = insts
                        .as_slice()
                        .iter()
                        .map(|i| (i64::from(i.col_idx) << 32) + i64::from(i.token_offset))
                        .collect();
                    v.sort_unstable();
                    v
                };
                let mut lists = Vec::with_capacity(1 + rest.len());
                lists.push(to_list(first_insts));
                for (ri, pm) in rest.iter().enumerate() {
                    lists.push(to_list(&pm[cursors[ri]].1));
                }
                if let Some(matched) = near_is_match(&lists, &n_terms, near.distance) {
                    let mut hits = PhraseHits::new();
                    for (k, list) in matched.iter().enumerate() {
                        for &pos in list {
                            hits.push(PhraseInstance {
                                phrase_idx: (near.first_phrase + k) as u32,
                                col_idx: (pos >> 32) as u32,
                                token_offset: (pos & 0x7fff_ffff) as u32,
                            });
                        }
                    }
                    out.push((rowid, hits));
                }
            }
            out
        }
        FtsQueryExpr::And(children) => {
            let mut acc: Option<EvalDocSet> = None;
            let mut dead = false;
            for child in children {
                // Every child is evaluated: the per-phrase hit counts are needed even when
                // the conjunction is already known to be empty.
                let r = eval_query_expr_precomputed(
                    child,
                    phrases,
                    precomputed_phrases,
                    phrase_doc_hits,
                );
                if dead {
                    continue;
                }
                acc = Some(match acc {
                    None => r,
                    Some(l) => {
                        let mut out = Vec::with_capacity(l.len().min(r.len()));
                        let mut l_iter = l.into_iter().peekable();
                        let mut r_iter = r.into_iter().peekable();
                        while let (Some(&(l_rid, _)), Some(&(r_rid, _))) =
                            (l_iter.peek(), r_iter.peek())
                        {
                            match l_rid.cmp(&r_rid) {
                                std::cmp::Ordering::Less => {
                                    l_iter.next();
                                }
                                std::cmp::Ordering::Greater => {
                                    r_iter.next();
                                }
                                std::cmp::Ordering::Equal => {
                                    if let (Some((rid, mut l_insts)), Some((_, r_insts))) =
                                        (l_iter.next(), r_iter.next())
                                    {
                                        l_insts.extend_from(&r_insts);
                                        out.push((rid, l_insts));
                                    }
                                }
                            }
                        }
                        out
                    }
                });
                if acc.as_ref().is_some_and(Vec::is_empty) {
                    dead = true;
                }
            }
            acc.unwrap_or_default()
        }
        FtsQueryExpr::Or(children) => {
            let mut acc: EvalDocSet = Vec::new();
            for child in children {
                let r = eval_query_expr_precomputed(
                    child,
                    phrases,
                    precomputed_phrases,
                    phrase_doc_hits,
                );
                if r.is_empty() {
                    continue;
                }
                if acc.is_empty() {
                    acc = r;
                    continue;
                }
                let l = std::mem::take(&mut acc);
                let mut out = Vec::with_capacity(l.len() + r.len());
                let mut l_iter = l.into_iter().peekable();
                let mut r_iter = r.into_iter().peekable();
                loop {
                    match (l_iter.peek(), r_iter.peek()) {
                        (Some(&(l_rid, _)), Some(&(r_rid, _))) => match l_rid.cmp(&r_rid) {
                            std::cmp::Ordering::Less => out.extend(l_iter.next()),
                            std::cmp::Ordering::Greater => out.extend(r_iter.next()),
                            std::cmp::Ordering::Equal => {
                                if let (Some((rid, mut l_insts)), Some((_, r_insts))) =
                                    (l_iter.next(), r_iter.next())
                                {
                                    l_insts.extend_from(&r_insts);
                                    out.push((rid, l_insts));
                                }
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
                acc = out;
            }
            acc
        }
        FtsQueryExpr::Not(lhs, rhs) => {
            let mut l =
                eval_query_expr_precomputed(lhs, phrases, precomputed_phrases, phrase_doc_hits);
            let r = eval_query_expr_precomputed(rhs, phrases, precomputed_phrases, phrase_doc_hits);
            if l.is_empty() || r.is_empty() {
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

/// The phrase group (column filter) every phrase of the query belongs to, by phrase number.
fn collect_phrase_colsets(expr: &FtsQueryExpr, out: &mut Vec<Option<Vec<usize>>>) {
    match expr {
        FtsQueryExpr::Near(n) => {
            for _ in &n.phrases {
                out.push(n.colset.clone());
            }
        }
        FtsQueryExpr::And(ch) | FtsQueryExpr::Or(ch) => {
            for c in ch {
                collect_phrase_colsets(c, out);
            }
        }
        FtsQueryExpr::Not(l, r) => {
            collect_phrase_colsets(l, out);
            collect_phrase_colsets(r, out);
        }
    }
}

/// `sqlite3Fts5ExprPopulatePoslists` for detail=column / detail=none tables: the index does
/// not store token offsets, so the instances of the phrases positioned on a row are found
/// by tokenizing the row's text again. `active` lists those phrases.
fn populate_instances_from_text(
    cfg: &Fts5Config,
    phrases: &[QueryPhrase],
    colsets: &[Option<Vec<usize>>],
    active: &[bool],
    col_texts: &[Option<Vec<u8>>],
) -> PhraseHits {
    let mut hits = PhraseHits::new();
    for (col, text) in col_texts.iter().enumerate() {
        let Some(text) = text else {
            continue;
        };
        let ok: Vec<bool> = (0..phrases.len())
            .map(|i| {
                active.get(i).copied().unwrap_or(false)
                    && colsets
                        .get(i)
                        .and_then(|c| c.as_ref())
                        .is_none_or(|c| c.contains(&col))
            })
            .collect();
        if !ok.iter().any(|&b| b) {
            continue;
        }
        let mut off = 0u32;
        cfg.tokenizer.tok.tokenize(text, &mut |tok, _, _| {
            let tok = &tok[..tok.len().min(FTS5_MAX_TOKEN_SIZE)];
            for (i, phrase) in phrases.iter().enumerate() {
                if !ok[i] {
                    continue;
                }
                let Some(term) = phrase.tokens.first() else {
                    continue;
                };
                let t = term.term.as_bytes();
                if (t.len() == tok.len() || (t.len() < tok.len() && term.is_prefix))
                    && &tok[..t.len()] == t
                {
                    hits.push(PhraseInstance {
                        phrase_idx: i as u32,
                        col_idx: col as u32,
                        token_offset: off,
                    });
                }
            }
            off += 1;
        });
    }
    hits
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
        /// The origin recorded in the row's %_docsize record: only segments whose origin
        /// range contains it lose the row (`sqlite3Fts5IndexContentlessDelete`).
        origin: u64,
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

    /// Drops the pending operations before the index is re-initialised (`'delete-all'`,
    /// `'rebuild'`; `fts5IndexDiscardData`), keeping what open savepoints need to restore.
    fn discard_pending_for_reset(&mut self) {
        for sp in &mut self.pending.savepoints {
            if sp.flushed_pre_ops.is_none() && sp.ops_len > 0 {
                sp.flushed_pre_ops = Some(self.pending.ops[..sp.ops_len].to_vec());
            }
            sp.flushed_since = true;
        }
        self.pending.ops.clear();
        self.pending.approx_bytes = 0;
    }

    /// The row count and per-column token totals as of now: the averages record plus the
    /// operations that are still pending (`Fts5Storage.nTotalRow` / `aTotalSize`).
    fn current_totals(
        &mut self,
        cfg: &Fts5Config,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<Vec<i64>> {
        let mut totals: Vec<i64> = self
            .read_averages(cfg, ctx)?
            .into_iter()
            .map(|v| v as i64)
            .collect();
        for op in &self.pending.ops {
            match op {
                PendingOp::DocPostings {
                    is_delete,
                    col_sizes,
                    ..
                } => {
                    let sign = if *is_delete { -1 } else { 1 };
                    totals[0] += sign;
                    for (i, &sz) in col_sizes.iter().enumerate() {
                        totals[1 + i] += sign * sz as i64;
                    }
                }
                PendingOp::ContentlessRowidDelete {
                    old_col_sizes,
                    had_docsize,
                    ..
                } => {
                    for (i, &sz) in old_col_sizes.iter().enumerate() {
                        totals[1 + i] -= sz as i64;
                    }
                    if *had_docsize {
                        totals[0] -= 1;
                    }
                }
            }
        }
        Ok(totals)
    }

    /// `sqlite3Fts5IndexSetCookie`: rewrites the structure record with the cookie
    /// incremented (done after every configuration change).
    fn bump_structure_cookie(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let srow = ctx.shadow_get_rowid(&self.shadows.data, FTS5_STRUCTURE_ROWID)?;
        let Some(Value::Blob(mut blob)) = srow.and_then(|mut r| {
            if r.len() >= 2 {
                Some(r.swap_remove(1))
            } else {
                None
            }
        }) else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        };
        if blob.len() < 4 {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        }
        let coherent = self.read_cache.structure_blob.as_deref() == Some(blob.as_slice());
        let cookie = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]).wrapping_add(1);
        blob[..4].copy_from_slice(&cookie.to_be_bytes());
        ctx.shadow_put_rowid(
            &self.shadows.data,
            FTS5_STRUCTURE_ROWID,
            &[
                Value::Integer(FTS5_STRUCTURE_ROWID),
                Value::Blob(blob.clone()),
            ],
            true,
        )?;
        if coherent {
            self.read_cache.structure_blob = Some(blob);
        }
        Ok(())
    }

    /// `sqlite3Fts5IndexGetOrigin`: the origin value the next level-0 segment will get.
    fn current_origin_counter(&mut self, ctx: &mut dyn VTabHostContext) -> Result<u64> {
        self.ensure_structure_coherent(ctx)?;
        Ok(self
            .read_cache
            .segments
            .iter()
            .map(|s| s.origin2)
            .max()
            .unwrap_or(0)
            + 1)
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
        // contentless_delete=1: the rewritten segment covers the origins of every segment it
        // replaces plus, when this flush adds documents, the origin those documents were
        // given in %_docsize (the current origin counter, see `current_origin_counter`).
        let prev_struct = if cfg.contentless_delete {
            Some(read_full_structure(ctx, &self.shadows.data)?)
        } else {
            None
        };
        let origin_cntr = prev_struct
            .as_ref()
            .map_or(0, FullStructureRecord::origin_counter);
        // A contentless delete hides the row in the segments whose origin range holds the
        // origin of its %_docsize record; rows of this flush carry the current counter.
        let origin_deletes: Vec<(u64, i64)> = ops
            .iter()
            .filter_map(|op| match op {
                PendingOp::ContentlessRowidDelete {
                    rowid,
                    had_docsize: true,
                    origin,
                    ..
                } if *origin != origin_cntr => Some((*origin, *rowid)),
                _ => None,
            })
            .collect();
        let mut merged = load_merged_index_with_deletes(
            ctx,
            &db_q,
            &self.shadows.table_name,
            cfg.detail,
            &origin_deletes,
        )?;
        let mut avgs = self.read_averages(cfg, ctx)?;
        let has_inserts = ops.iter().any(|op| {
            matches!(
                op,
                PendingOp::DocPostings {
                    is_delete: false,
                    ..
                }
            )
        });

        // The entries written by this flush, applied on top of the stored segments below.
        let mut flushed: MergedIndexMap = BTreeMap::new();
        for op in ops {
            match op {
                PendingOp::DocPostings {
                    rowid,
                    is_delete,
                    col_sizes,
                    doc_terms,
                } => {
                    for (term, poslist) in doc_terms {
                        let key = term.as_slice().to_vec();
                        if is_delete {
                            if let Some(entry) = merged.get_mut(&key) {
                                entry.remove(&rowid);
                            }
                            if let Some(entry) = flushed.get_mut(&key) {
                                entry.remove(&rowid);
                            }
                        } else {
                            let col_hits =
                                decode_full_poslist_payload(cfg.detail, poslist.as_slice())?;
                            flushed.entry(key).or_default().insert(rowid, col_hits);
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
                    origin,
                } => {
                    // `fts5StorageContentlessDelete`: the column totals always shrink; the
                    // row itself is removed when its %_docsize record names an origin.
                    for (i, &sz) in old_col_sizes.iter().enumerate() {
                        avgs[1 + i] = avgs[1 + i].saturating_sub(sz);
                    }
                    if had_docsize {
                        if origin == origin_cntr {
                            for docs in flushed.values_mut() {
                                docs.remove(&rowid);
                            }
                        }
                        avgs[0] = avgs[0].saturating_sub(1);
                    }
                }
            }
        }
        for (term, docs) in flushed {
            let entry = merged.entry(term).or_default();
            for (rowid, hits) in docs {
                entry.insert(rowid, hits);
            }
        }
        merged.retain(|_, docs| !docs.is_empty());
        let origin = prev_struct.map(|ps| {
            let cntr = origin_cntr;
            let origin1 = ps.min_origin().unwrap_or(cntr);
            let origin2 = if has_inserts {
                cntr
            } else {
                (cntr - 1).max(origin1)
            };
            SegOrigin {
                origin1,
                origin2,
                n_entry: avgs[0],
            }
        });
        let sblob = write_c_fts5_index(ctx, &db_q, &self.shadows.table_name, cfg, &merged, origin)?;
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
        let n_inserted_rows = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    PendingOp::DocPostings {
                        is_delete: false,
                        ..
                    }
                )
            })
            .count() as u64;
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
                            Ok(idx) => {
                                // Delete followed by insert of the same row in one flush:
                                // the new entry keeps the delete flag, as in C's hash.
                                let poslist = if !is_delete
                                    && payload_has_delete_flag(cfg.detail, entry[idx].2.as_slice())
                                {
                                    payload_with_delete_flag(cfg.detail, poslist.as_slice())
                                } else {
                                    poslist
                                };
                                entry[idx] = (rowid, is_delete, poslist);
                            }
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

        // `fts5FlushOneHash`: a new level-0 segment of a contentless_delete=1 table takes the
        // current origin counter for both ends of its origin range and counts the rows
        // written into it.
        let origin_cntr = if full_struct.is_v2 {
            full_struct.origin_counter()
        } else {
            0
        };
        let new_rec = Fts5SegRecord {
            segid: new_segid,
            pgno_first: 1,
            pgno_last,
            n_tombstone_pages: 0,
            origin1: origin_cntr,
            origin2: origin_cntr,
            n_entry_tombstone: 0,
            n_entry: if full_struct.is_v2 {
                n_inserted_rows
            } else {
                0
            },
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

        if cfg.has_content_table() {
            let content_sq = quote_single(&format!("{tbl_name}_content"));
            ctx.execute_sql(
                &format!(
                    "CREATE TABLE {db_q}.{content_sq}({})",
                    cfg.content_table_defn()
                ),
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
        if cfg.content_mode == ContentMode::Normal {
            let _ = ctx.set_vtab_config(VTabConfigOption::ConstraintSupport);
        }
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
        if cfg.content_mode == ContentMode::Normal {
            let _ = ctx.set_vtab_config(VTabConfigOption::ConstraintSupport);
        }
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

    /// `FTS5_STMT_LOOKUP`: the stored values of row `rowid`, `None` if the content source has
    /// no such row. Contentless tables answer from `%_docsize` (all values NULL) or, with
    /// `contentless_unindexed=1`, from `%_content` (UNINDEXED columns only).
    fn read_content_row(
        &self,
        state: &Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<Vec<Value>>> {
        match &self.cfg.content_mode {
            ContentMode::Contentless if !self.cfg.unindexed_content => {
                if self.cfg.columnsize
                    && ctx
                        .shadow_get_rowid(&state.shadows.docsize, rowid)?
                        .is_some()
                {
                    return Ok(Some(vec![Value::Null; self.cfg.columns.len()]));
                }
                Ok(None)
            }
            ContentMode::Normal | ContentMode::Contentless => Ok(ctx
                .shadow_get_rowid(&state.shadows.content, rowid)?
                .map(|r| self.cfg.content_row_to_cols(r))),
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

    /// `sqlite3Fts5StorageContentInsert`: stores the row in `%_content` (when the table has
    /// one) and returns its rowid, allocating one when `new_rowid` is not an integer.
    fn content_insert(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        replace: bool,
        new_rowid: Option<&Value>,
        col_vals: &[Value],
    ) -> Result<i64> {
        if !self.cfg.has_content_table() {
            if let Some(Value::Integer(rid)) = new_rowid {
                return Ok(*rid);
            }
            // `fts5StorageNewRowid`: the next rowid of %_docsize; without that table the
            // caller has to supply the rowid.
            if !self.cfg.columnsize {
                return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
            }
            return Ok(ctx
                .shadow_max_rowid(&state.shadows.docsize)?
                .map_or(1, |m| m.saturating_add(1)));
        }
        if self.cfg.unindexed_content && self.cfg.locale {
            // C prepares `INSERT INTO %_content VALUES(...)` without the l<N> columns for a
            // contentless_unindexed=1 + locale=1 table and reports the failure as corruption;
            // no row can be written to such a table by either engine.
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        }
        let rid = match content_rowid_from_value(new_rowid)? {
            Some(r) => r,
            None => ctx
                .shadow_max_rowid(&state.shadows.content)?
                .map_or(1, |m| m.saturating_add(1)),
        };
        let row = self.cfg.content_row(rid, col_vals);
        ctx.shadow_put_rowid(&state.shadows.content, rid, &row, replace)
            .map_err(|e| {
                if e.code == ErrorCode::Constraint {
                    // the C module reports the bare result code of the %_content insert
                    SqliteError::new(ErrorCode::Constraint, "constraint failed")
                } else {
                    e
                }
            })?;
        Ok(rid)
    }

    /// `sqlite3Fts5StorageIndexInsert`: index entries, `%_docsize` row and totals for a row.
    fn index_insert(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        col_vals: &[Value],
    ) -> Result<()> {
        let (doc_terms, col_sizes) = build_doc_term_encoded_postings(&self.cfg, col_vals, false);

        if self.cfg.columnsize {
            let sz_blob = Value::Blob(encode_varint_list(&col_sizes));
            if self.cfg.contentless_delete {
                // `fts5StorageInsertDocsize`: the origin of the segment this row will be
                // flushed into (`sqlite3Fts5IndexGetOrigin`).
                let origin = state.current_origin_counter(ctx)?;
                ctx.shadow_put_rowid(
                    &state.shadows.docsize,
                    rowid,
                    &[
                        Value::Integer(rowid),
                        sz_blob,
                        Value::Integer(origin as i64),
                    ],
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
        )
    }

    /// `fts5StorageInsert`.
    fn storage_insert(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        new_rowid: Option<&Value>,
        col_vals: &[Value],
    ) -> Result<i64> {
        let rid = self.content_insert(state, ctx, false, new_rowid, col_vals)?;
        self.index_insert(state, ctx, rid, col_vals)?;
        Ok(rid)
    }

    /// `sqlite3Fts5StorageDelete`: removes row `rowid` from the index, `%_docsize` and
    /// `%_content`. `vals` are the values to remove from the index (the `'delete'` command);
    /// without them they are looked up in the content source, and a row that is not found
    /// there leaves the index untouched.
    fn storage_delete(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        vals: Option<&[Value]>,
    ) -> Result<()> {
        if self.cfg.contentless_delete {
            self.contentless_delete_row(state, ctx, rowid)?;
        } else {
            let looked_up;
            let old_vals = match vals {
                Some(v) => Some(v),
                None => {
                    looked_up = self.read_content_row(state, ctx, rowid)?;
                    looked_up.as_deref()
                }
            };
            if let Some(old_vals) = old_vals {
                let (doc_terms, col_sizes) =
                    build_doc_term_encoded_postings(&self.cfg, old_vals, true);
                // `fts5StorageDeleteFromIndex`: the totals may not go negative.
                let totals = state.current_totals(&self.cfg, ctx)?;
                let corrupt =
                    || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
                for (i, &sz) in col_sizes.iter().enumerate() {
                    if !self.cfg.unindexed[i] && totals[1 + i] < sz as i64 {
                        return Err(corrupt());
                    }
                }
                if totals[0] < 1 {
                    return Err(corrupt());
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
                )?;
            }
        }

        if self.cfg.columnsize {
            ctx.shadow_delete_rowid(&state.shadows.docsize, rowid)?;
            state.read_cache.remove_docsize(rowid);
        }
        if self.cfg.has_content_table() {
            ctx.shadow_delete_rowid(&state.shadows.content, rowid)?;
        }
        Ok(())
    }

    /// `sqlite3Fts5StorageDelete` for the old row of an UPDATE that changes the rowid; the
    /// new `%_content` row has already been written (`fts5UpdateMethod`, case 3).
    fn storage_delete_keep_new_content(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        old_rowid: i64,
    ) -> Result<()> {
        self.storage_delete(state, ctx, old_rowid, None)
    }

    /// `fts5StorageContentlessDelete`: a row of a contentless_delete=1 table is known by its
    /// `%_docsize` record; without one there is nothing to delete.
    fn contentless_delete_row(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<()> {
        let Some(row) = ctx.shadow_get_rowid(&state.shadows.docsize, rowid)? else {
            return Ok(());
        };
        let n_cols = self.cfg.columns.len();
        let sizes = match row.get(1) {
            Some(Value::Blob(b)) => decode_varint_list(b).filter(|l| l.len() == n_cols),
            _ => None,
        };
        let Some(old_col_sizes) = sizes else {
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        };
        let origin = row.get(2).map_or(0, Value::to_integer_lossy);
        self.push_pending_and_maybe_flush(
            state,
            ctx,
            PendingOp::ContentlessRowidDelete {
                rowid,
                old_col_sizes,
                had_docsize: origin != 0,
                origin: origin.max(0) as u64,
            },
        )
    }

    /// `fts5SpecialInsert` / `fts5SpecialDelete`: `INSERT INTO ft(ft, ...) VALUES(cmd, ...)`.
    fn execute_special_command(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        cmd: &str,
        new_rowid: Option<Value>,
        values: &[Value],
    ) -> Result<()> {
        let n_cols = self.cfg.columns.len();
        let rank_val = values.get(n_cols + 1).cloned().unwrap_or(Value::Null);
        let contentless = self.cfg.content_mode == ContentMode::Contentless;

        if self.cfg.content_mode != ContentMode::Normal && cmd.eq_ignore_ascii_case("delete") {
            if self.cfg.contentless_delete {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "'delete' may not be used with a contentless_delete=1 table",
                ));
            }
            if let Some(Value::Integer(rid)) = new_rowid {
                self.storage_delete(state, ctx, rid, Some(&values[..n_cols]))?;
            }
            return Ok(());
        }

        if cmd.eq_ignore_ascii_case("delete-all") {
            if self.cfg.content_mode == ContentMode::Normal {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "'delete-all' may only be used with a contentless or external content fts5 table",
                ));
            }
            state.discard_pending_for_reset();
            self.reset_index_storage(state, ctx)?;
            if self.cfg.unindexed_content {
                let db_q = quote_ident(ctx.db_name());
                let content_q = quote_ident(&state.shadows.content);
                ctx.execute_sql(&format!("DELETE FROM {db_q}.{content_q}"), &[])?;
            }
            return self.write_config_version(state, ctx);
        }
        if cmd.eq_ignore_ascii_case("rebuild") {
            if contentless {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "'rebuild' may not be used with a contentless fts5 table",
                ));
            }
            state.discard_pending_for_reset();
            self.rebuild_index(state, ctx)?;
            return self.write_config_version(state, ctx);
        }
        if cmd.eq_ignore_ascii_case("optimize") || cmd.eq_ignore_ascii_case("merge") {
            return self.compact_segments(state, ctx);
        }
        if cmd.eq_ignore_ascii_case("integrity-check") {
            // `sqlite3Fts5StorageIntegrity(p, iArg)`: an external content table is only
            // compared with the index when the rank argument is non-zero.
            let check_external = rank_val.to_integer_lossy() as i32 != 0;
            state.flush_pending(&self.cfg, ctx)?;
            return self.verify_integrity(state, ctx, check_external);
        }
        if cmd.eq_ignore_ascii_case("flush") {
            return state.flush_pending(&self.cfg, ctx);
        }

        // Everything else is a configuration setting (`sqlite3Fts5ConfigSetValue`); an
        // unknown key or an out-of-range value is SQLITE_ERROR without a message.
        state.flush_pending(&self.cfg, ctx)?;
        if !config_value_is_valid(cmd, &rank_val) {
            return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
        }
        let db_q = quote_ident(ctx.db_name());
        let config_q = quote_ident(&state.shadows.config);
        ctx.execute_sql(
            &format!("REPLACE INTO {db_q}.{config_q} VALUES(?1, ?2)"),
            &[Value::Text(cmd.to_owned()), rank_val],
        )?;
        state.read_cache.configured_rank_weights = None;
        state.read_cache.secure_delete = None;
        // `sqlite3Fts5StorageConfigValue`: a configuration change bumps the cookie.
        state.bump_structure_cookie(ctx)
    }

    /// `sqlite3Fts5StorageConfigValue(p, "version", 0, FTS5_CURRENT_VERSION)`.
    fn write_config_version(
        &self,
        state: &Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<()> {
        let db_q = quote_ident(ctx.db_name());
        let config_q = quote_ident(&state.shadows.config);
        ctx.execute_sql(
            &format!("REPLACE INTO {db_q}.{config_q} VALUES('version', 4)"),
            &[],
        )?;
        Ok(())
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

        let origin = self.cfg.contentless_delete.then_some(SegOrigin {
            origin1: 1,
            origin2: 1,
            n_entry: avgs[0],
        });
        let sblob = write_c_fts5_index(
            ctx,
            &db_q,
            &state.shadows.table_name,
            &self.cfg,
            &merged,
            origin,
        )?;
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
        let origin = if self.cfg.contentless_delete {
            let ps = read_full_structure(ctx, &state.shadows.data)?;
            let cntr = ps.origin_counter();
            let origin1 = ps.min_origin().unwrap_or(cntr);
            let n_entry = ps
                .levels
                .iter()
                .flat_map(|(_, segs)| segs.iter())
                .map(|seg| seg.n_entry.saturating_sub(seg.n_entry_tombstone))
                .sum();
            Some(SegOrigin {
                origin1,
                origin2: (cntr - 1).max(origin1),
                n_entry,
            })
        } else {
            None
        };
        let sblob = write_c_fts5_index(
            ctx,
            &db_q,
            &state.shadows.table_name,
            &self.cfg,
            &merged,
            origin,
        )?;
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

        let corrupt = || SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed");
        let n_cols = self.cfg.columns.len();
        let n_src_rows = src_rows.len() as u64;
        let mut total_sizes = vec![0u64; n_cols];
        let mut expected: MergedIndexMap = BTreeMap::new();
        for row in src_rows {
            let rid = row.first().map_or(0, Value::to_integer_lossy);
            let cvals = &row[1..];
            let (doc_terms, col_sizes) = build_doc_term_postings(&self.cfg, cvals, false);
            for (term, chits) in doc_terms {
                expected.entry(term).or_default().insert(rid, chits);
            }
            // `sqlite3Fts5StorageIntegrity`: the %_docsize record of every row must hold the
            // token counts of its text.
            if self.cfg.columnsize {
                let stored =
                    ctx.shadow_get_rowid(&state.shadows.docsize, rid)?
                        .and_then(|r| match r.get(1) {
                            Some(Value::Blob(b)) => decode_varint_list(b),
                            _ => None,
                        });
                if stored.as_deref() != Some(col_sizes.as_slice()) {
                    return Err(corrupt());
                }
            }
            for (i, &sz) in col_sizes.iter().enumerate() {
                total_sizes[i] += sz;
            }
        }
        // The totals record and the row counts of %_content / %_docsize must agree with
        // the rows just scanned.
        let avg_row = ctx.shadow_get_rowid(&state.shadows.data, FTS5_AVERAGES_ROWID)?;
        let mut avgs = match avg_row.as_ref().and_then(|r| r.get(1)) {
            Some(Value::Blob(b)) => decode_varint_list(b).unwrap_or_default(),
            _ => Vec::new(),
        };
        avgs.resize(1 + n_cols, 0);
        if avgs[1..] != total_sizes[..] {
            return Err(corrupt());
        }
        if self.cfg.content_mode == ContentMode::Normal && n_src_rows != avgs[0] {
            return Err(corrupt());
        }
        if self.cfg.columnsize {
            let docsize_q = quote_ident(&state.shadows.docsize);
            let n_docsize = ctx
                .query_sql(&format!("SELECT count(*) FROM {db_q}.{docsize_q}"), &[])?
                .first()
                .and_then(|r| r.first())
                .map_or(0, Value::to_integer_lossy);
            if n_docsize as u64 != avgs[0] {
                return Err(corrupt());
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

/// The text of a value as `sqlite3_value_text()` returns it: `None` for NULL, the bytes of a
/// BLOB, C's `"%!.17g"` rendering of a REAL.
fn fts5_value_bytes(v: &Value) -> Option<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    match v {
        Value::Null => None,
        Value::Text(s) => Some(Cow::Borrowed(s.as_bytes())),
        Value::Blob(b) => Some(Cow::Borrowed(b.as_slice())),
        Value::Integer(i) => Some(Cow::Owned(i.to_string().into_bytes())),
        other => Some(Cow::Owned(real_to_text(other.to_real_lossy()).into_bytes())),
    }
}

/// `fts5_value_bytes` as (lossy) text.
fn fts5_value_text(v: &Value) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    match v {
        Value::Null => None,
        Value::Text(s) => Some(Cow::Borrowed(s.as_str())),
        Value::Blob(b) => Some(String::from_utf8_lossy(b)),
        Value::Integer(i) => Some(Cow::Owned(i.to_string())),
        other => Some(Cow::Owned(real_to_text(other.to_real_lossy()))),
    }
}

/// `sqlite3_value_numeric_type(v)==SQLITE_INTEGER` followed by `sqlite3_value_int64(v)`.
fn numeric_integer_value(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Integer(i) => Some(*i),
        Value::Text(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// The rowid SQLite stores when `v` is bound to the INTEGER PRIMARY KEY of `%_content`:
/// `None` = allocate one, an error for a value that is not an integer.
fn content_rowid_from_value(v: Option<&Value>) -> Result<Option<i64>> {
    let mismatch = || SqliteError::new(ErrorCode::Mismatch, "datatype mismatch");
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Integer(i)) => Ok(Some(*i)),
        Some(Value::Text(s)) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                return Ok(Some(i));
            }
            match t.parse::<f64>() {
                Ok(r) if r.fract() == 0.0 && r.abs() < 9.2e18 => Ok(Some(r as i64)),
                _ => Err(mismatch()),
            }
        }
        Some(Value::Blob(_)) => Err(mismatch()),
        Some(other) => {
            let r = other.to_real_lossy();
            if r.fract() == 0.0 && r.abs() < 9.2e18 {
                Ok(Some(r as i64))
            } else {
                Err(mismatch())
            }
        }
    }
}

/// `sqlite3_value_int()` of a value whose numeric type is INTEGER, else `None`.
fn config_int_arg(v: &Value) -> Option<i32> {
    numeric_integer_value(Some(v)).map(|i| i as i32)
}

/// `sqlite3Fts5ConfigParseRank`: `bareword ( literal, ... )`.
fn config_rank_is_valid(z: &[u8]) -> bool {
    let p = cfg_skip_ws(z);
    let Some(n) = cfg_bareword_len(p) else {
        return false;
    };
    let p = cfg_skip_ws(&p[n..]);
    if p.first() != Some(&b'(') {
        return false;
    }
    let mut p = cfg_skip_ws(&p[1..]);
    if p.first() == Some(&b')') {
        return true;
    }
    loop {
        p = cfg_skip_ws(p);
        let Some(n) = cfg_literal_len(p) else {
            return false;
        };
        p = cfg_skip_ws(&p[n..]);
        match p.first() {
            Some(b')') => return true,
            Some(b',') => p = &p[1..],
            _ => return false,
        }
    }
}

/// `sqlite3Fts5ConfigSetValue`: false for an unknown key or a value the key does not accept.
fn config_value_is_valid(key: &str, val: &Value) -> bool {
    let int = config_int_arg(val);
    if key.eq_ignore_ascii_case("pgsz") {
        int.is_some_and(|v| (32..=64 * 1024).contains(&v))
    } else if key.eq_ignore_ascii_case("hashsize") {
        int.is_some_and(|v| v > 0)
    } else if key.eq_ignore_ascii_case("automerge") {
        int.is_some_and(|v| (0..=64).contains(&v))
    } else if key.eq_ignore_ascii_case("usermerge") {
        int.is_some_and(|v| (2..=16).contains(&v))
    } else if key.eq_ignore_ascii_case("crisismerge") {
        int.is_some_and(|v| v >= 0)
    } else if key.eq_ignore_ascii_case("deletemerge") {
        int.is_some()
    } else if key.eq_ignore_ascii_case("rank") {
        fts5_value_bytes(val).is_some_and(|z| config_rank_is_valid(&z))
    } else if key.eq_ignore_ascii_case("secure-delete") || key.eq_ignore_ascii_case("insttoken") {
        int.is_some_and(|v| v >= 0)
    } else {
        false
    }
}

/// The column weights of a rank specification `bm25(w0, w1, ...)` (`fts5FindRankFunction`:
/// the arguments are SQL literals, read as doubles). `bm25` is the only ranking function.
fn parse_rank_expression(s: &str) -> Result<Vec<f64>> {
    let trimmed = s.trim_matches(' ');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if !config_rank_is_valid(trimmed.as_bytes()) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("parse error in rank function: {s}"),
        ));
    }
    let open_p = trimmed.find('(').unwrap_or(trimmed.len());
    let fn_name = trimmed[..open_p].trim_matches(' ');
    if !fn_name.eq_ignore_ascii_case("bm25") {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such function: {fn_name}"),
        ));
    }
    let close_p = trimmed.rfind(')').unwrap_or(trimmed.len());
    let mut weights = Vec::new();
    let mut p = cfg_skip_ws(trimmed.as_bytes()[open_p + 1..close_p].as_ref());
    while !p.is_empty() {
        let Some(n) = cfg_literal_len(p) else {
            break;
        };
        let lit = std::str::from_utf8(&p[..n]).unwrap_or("");
        // sqlite3_value_double() of the literal: numbers as such, numeric text prefixes of
        // strings, 0.0 for NULL and blobs.
        let w = if lit.starts_with('\'') {
            Value::Text(String::from_utf8_lossy(&cfg_dequote_word(lit.as_bytes())).into_owned())
                .to_real_lossy()
        } else {
            lit.parse::<f64>().unwrap_or(0.0)
        };
        weights.push(w);
        p = cfg_skip_ws(&p[n..]);
        if p.first() == Some(&b',') {
            p = cfg_skip_ws(&p[1..]);
        }
    }
    Ok(weights)
}

impl VirtualTable for Fts5Table {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let tbl_col = self.hidden_tbl_col();
        let rank_col = self.hidden_rank_col();

        // `fts5BestIndexMethod`: every MATCH (or `ft = ?`) constraint is consumed; all of them
        // are ANDed together. `rank MATCH ?` / `rank = ?` selects the ranking function.
        let is_match_like = |c: &sqliters_types::VTabConstraint| {
            c.op == VTabConstraintOp::Match
                || (c.op == VTabConstraintOp::Eq && c.i_column >= tbl_col)
        };
        let mut plan: Vec<String> = Vec::new();
        let mut argv_pos = 0usize;
        let mut n_match = 0usize;
        let mut seen_rank = false;
        for (i, c) in info.constraints.iter().enumerate() {
            if !is_match_like(c) {
                continue;
            }
            if !c.usable || c.i_column < 0 {
                // An unusable MATCH constraint makes this an unusable plan.
                return Err(SqliteError::new(
                    ErrorCode::Constraint,
                    "unusable fts5 match constraint",
                ));
            }
            if c.i_column == rank_col {
                if seen_rank {
                    continue;
                }
                seen_rank = true;
                plan.push("r".to_owned());
            } else {
                n_match += 1;
                plan.push(format!("M{}", c.i_column));
            }
            argv_pos += 1;
            info.constraint_usage[i].argv_index = argv_pos;
            info.constraint_usage[i].omit = true;
        }

        let mut idx_num = IDX_FULLSCAN;
        let mut idx_str = None;
        if n_match > 0 {
            idx_num = IDX_MATCH;
            idx_str = Some(plan.join(","));
            if seen_rank {
                idx_num |= IDX_FLAG_HAS_RANK_CFG;
            }
            info.estimated_cost = 10.0;
            info.estimated_rows = 10;
        } else {
            // Without a MATCH the rank constraint is left to the caller (it never holds).
            for (i, c) in info.constraints.iter().enumerate() {
                if is_match_like(c) {
                    info.constraint_usage[i].argv_index = 0;
                    info.constraint_usage[i].omit = false;
                }
            }
            argv_pos = 0;
            let mut like_cidx: Option<(usize, VTabConstraintOp)> = None;
            let mut rowid_eq_cidx: Option<usize> = None;
            for (i, c) in info.constraints.iter().enumerate() {
                if !c.usable || is_match_like(c) {
                    continue;
                }
                if c.i_column == -1 && c.op == VTabConstraintOp::Eq {
                    if rowid_eq_cidx.is_none() {
                        rowid_eq_cidx = Some(i);
                    }
                } else if matches!(c.op, VTabConstraintOp::Like | VTabConstraintOp::Glob)
                    && c.i_column >= 0
                    && c.i_column < tbl_col
                    && like_cidx.is_none()
                {
                    // `fts5UsePatternMatch`: a case-insensitive trigram tokenizer without
                    // diacritic removal serves LIKE and GLOB, a case-sensitive one GLOB.
                    let ok = match self.cfg.tokenizer.tok.pattern() {
                        TokenizerPattern::Like => true,
                        TokenizerPattern::Glob => c.op == VTabConstraintOp::Glob,
                        TokenizerPattern::None => false,
                    };
                    if ok {
                        like_cidx = Some((i, c.op));
                    }
                }
            }
            if let Some((li, lop)) = like_cidx {
                argv_pos += 1;
                info.constraint_usage[li].argv_index = argv_pos;
                info.constraint_usage[li].omit = false;
                idx_num = if lop == VTabConstraintOp::Like {
                    IDX_LIKE
                } else {
                    IDX_GLOB
                };
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
        }
        let _ = argv_pos;

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
        if (info.col_used & vis_mask) != 0 {
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

    /// `fts5UpdateMethod`.
    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let n_cols = self.cfg.columns.len();
        let name = st.shadows.table_name.clone();
        // fts5IsContentless(pTab, 1)
        let contentless = self.cfg.content_mode == ContentMode::Contentless;
        // The conflict mode only matters where a rowid conflict can be detected.
        let conflict =
            if self.cfg.content_mode == ContentMode::Normal || self.cfg.contentless_delete {
                ctx.vtab_on_conflict()
            } else {
                VTabConflictMode::Abort
            };
        match op {
            VTabUpdateOperation::Insert { new_rowid, values } => {
                // A "special" INSERT: any non-NULL value in the table-name column.
                if let Some(cmd_val) = values.get(n_cols)
                    && !cmd_val.is_null()
                {
                    let cmd = fts5_value_text(cmd_val).unwrap_or_default().into_owned();
                    self.execute_special_command(&mut st, ctx, &cmd, new_rowid, &values)?;
                    return Ok(None);
                }
                let new_rowid = new_rowid.filter(|v| !v.is_null());
                if conflict == VTabConflictMode::Replace
                    && let Some(rid) = numeric_integer_value(new_rowid.as_ref())
                {
                    self.storage_delete(&mut st, ctx, rid, None)?;
                }
                let rid =
                    self.storage_insert(&mut st, ctx, new_rowid.as_ref(), &values[..n_cols])?;
                Ok(Some(rid))
            }
            VTabUpdateOperation::Delete { old_rowid } => {
                // It is only possible to DELETE from a contentless table if the
                // contentless_delete=1 flag is set.
                if contentless && !self.cfg.contentless_delete {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot DELETE from contentless fts5 table: {name}"),
                    ));
                }
                let rid = old_rowid.to_integer_lossy();
                self.storage_delete(&mut st, ctx, rid, None)?;
                Ok(None)
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                let old_rid = old_rowid.to_integer_lossy();
                let new_rid_opt = match &new_rowid {
                    Value::Null => Some(old_rid),
                    other => numeric_integer_value(Some(other)),
                };
                let new_rid = new_rid_opt.unwrap_or(old_rid);
                let col_vals = &values[..n_cols];

                // `fts5ContentlessUpdate`
                let mut content_only = false;
                if contentless {
                    let mut seen_index = false;
                    let mut seen_index_nc = false;
                    for i in 0..n_cols {
                        if !self.cfg.unindexed[i] {
                            if ctx.vtab_nochange(i) {
                                seen_index_nc = true;
                            } else {
                                seen_index = true;
                            }
                        }
                    }
                    if !seen_index && old_rid == new_rid {
                        content_only = true;
                    } else if seen_index_nc || !self.cfg.contentless_delete {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            if self.cfg.contentless_delete {
                                format!(
                                    "cannot UPDATE a subset of columns on fts5 contentless-delete table: {name}"
                                )
                            } else {
                                format!("cannot UPDATE contentless fts5 table: {name}")
                            },
                        ));
                    }
                }

                if new_rid_opt.is_none() {
                    return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                }
                let new_val = Value::Integer(new_rid);
                if old_rid != new_rid {
                    if conflict == VTabConflictMode::Replace {
                        self.storage_delete(&mut st, ctx, old_rid, None)?;
                        self.storage_delete(&mut st, ctx, new_rid, None)?;
                        self.storage_insert(&mut st, ctx, Some(&new_val), col_vals)?;
                    } else {
                        // Detect a conflict on the new rowid before the index is touched.
                        self.content_insert(&mut st, ctx, false, Some(&new_val), col_vals)?;
                        self.storage_delete_keep_new_content(&mut st, ctx, old_rid)?;
                        self.index_insert(&mut st, ctx, new_rid, col_vals)?;
                    }
                } else if content_only {
                    // Only UNINDEXED columns of a contentless table change: a write to
                    // %_content for contentless_unindexed=1 tables, a no-op otherwise.
                    if self.cfg.unindexed_content
                        && self.read_content_row(&st, ctx, old_rid)?.is_some()
                    {
                        self.content_insert(&mut st, ctx, true, Some(&new_val), col_vals)?;
                    }
                } else {
                    self.storage_delete(&mut st, ctx, old_rid, None)?;
                    self.storage_insert(&mut st, ctx, Some(&new_val), col_vals)?;
                }
                Ok(Some(new_rid))
            }
        }
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.flush_pending(&self.cfg, ctx)?;
        let db_q = quote_ident(ctx.db_name());
        let old_name = st.shadows.table_name.clone();
        let mut suffixes = vec!["data", "idx", "config"];
        if self.cfg.has_content_table() {
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
        // `sqlite3Fts5DropAll`: data, idx, config, then docsize, then content.
        for suf in ["data", "idx", "config", "docsize", "content"] {
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
    /// `fts5ApiColumnSize` for a table without %_docsize: count the tokens of the stored text.
    fn compute_fallback_col_sizes(&self, col_vals: &[Value]) -> DocColSizes {
        let mut out = vec![0usize; self.cfg.columns.len()];
        for (i, v) in col_vals.iter().enumerate().take(self.cfg.columns.len()) {
            if !self.cfg.unindexed[i]
                && let Some(text) = fts5_value_bytes(v)
            {
                let mut n = 0usize;
                self.cfg
                    .tokenizer
                    .tok
                    .tokenize(&text, &mut |_, _, _| n += 1);
                out[i] = n;
            }
        }
        DocColSizes::from_usize_vec(out)
    }

    /// The columns `xColumnSize` cannot size (`-1`): indexed columns of a contentless table
    /// without %_docsize.
    fn unknown_size_cols(&self) -> Vec<bool> {
        if !self.cfg.columnsize && self.cfg.content_mode == ContentMode::Contentless {
            self.cfg.unindexed.iter().map(|&u| !u).collect()
        } else {
            Vec::new()
        }
    }

    /// The column values of a matched row (`fts5SeekCursor`). A row that is in the index
    /// but not in the content source is an error.
    fn load_content_vals(
        &self,
        shadows: &Fts5ShadowNames,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Vec<Value>> {
        let db_name = ctx.db_name().to_owned();
        let missing = |content: String| {
            SqliteError::new(
                ErrorCode::Corrupt,
                format!("fts5: missing row {rowid} from content table {content}"),
            )
        };
        match &self.cfg.content_mode {
            ContentMode::Normal | ContentMode::Contentless if self.cfg.has_content_table() => {
                match ctx.shadow_get_rowid(&shadows.content, rowid)? {
                    Some(r) => Ok(self.cfg.content_row_to_cols(r)),
                    None => Err(missing(format!(
                        "{}.'{}'",
                        quote_single(&db_name),
                        shadows.content.replace('\'', "''")
                    ))),
                }
            }
            ContentMode::Normal | ContentMode::Contentless => {
                Ok(vec![Value::Null; self.cfg.columns.len()])
            }
            ContentMode::External { table, rowid_col } => {
                let db_q = quote_ident(&db_name);
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
                r.pop().ok_or_else(|| {
                    missing(format!(
                        "{}.{}",
                        quote_single(&db_name),
                        quote_single(table)
                    ))
                })
            }
        }
    }
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
                    ContentMode::Normal | ContentMode::Contentless
                        if self.cfg.has_content_table() =>
                    {
                        let content_q = quote_ident(&shadows.content);
                        let qrows = ctx.query_sql(
                            &format!(
                                "SELECT {} FROM {db_q}.{content_q} ORDER BY id {order_kw}",
                                self.cfg.content_select_list()
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
                    ContentMode::Normal | ContentMode::Contentless => {
                        // A contentless table is scanned through %_docsize; without that
                        // table there is nothing to scan (`fts5FilterMethod`).
                        if !self.cfg.columnsize {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("{}: table does not support scanning", shadows.table_name),
                            ));
                        }
                        let docsize_q = quote_ident(&shadows.docsize);
                        let qrows = ctx.query_sql(
                            &format!("SELECT id FROM {db_q}.{docsize_q} ORDER BY id {order_kw}"),
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
            IDX_ROWID_EQ => {
                let Some(Value::Integer(rid)) = args.first() else {
                    return Ok(());
                };
                // `FTS5_PLAN_ROWID`: one lookup in the content source.
                match &self.cfg.content_mode {
                    ContentMode::Normal | ContentMode::Contentless
                        if self.cfg.has_content_table() =>
                    {
                        if let Some(r) = ctx.shadow_get_rowid(&shadows.content, *rid)? {
                            self.rows.push(Fts5CursorRow {
                                rowid: *rid,
                                cols: self.cfg.content_row_to_cols(r),
                                aux_handle: 0,
                                rank: None,
                            });
                        }
                    }
                    ContentMode::External { table, rowid_col } => {
                        let ext_q = quote_ident(table);
                        let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                        let cols: Vec<String> =
                            self.cfg.columns.iter().map(|c| quote_ident(c)).collect();
                        let mut r = ctx.query_sql(
                            &format!(
                                "SELECT {} FROM {db_q}.{ext_q} WHERE {rid_q} = ?1",
                                cols.join(", ")
                            ),
                            &[Value::Integer(*rid)],
                        )?;
                        if let Some(vals) = r.pop() {
                            self.rows.push(Fts5CursorRow {
                                rowid: *rid,
                                cols: vals,
                                aux_handle: 0,
                                rank: None,
                            });
                        }
                    }
                    ContentMode::Normal | ContentMode::Contentless => {
                        if !self.cfg.columnsize {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("{}: table does not support scanning", shadows.table_name),
                            ));
                        }
                        if ctx.shadow_get_rowid(&shadows.docsize, *rid)?.is_some() {
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
                // `fts5FilterMethod`: the plan string lists the consumed constraints in
                // argument order: `M<col>` for a MATCH (col == number of columns for the
                // whole table) and `r` for the rank configuration.
                let n_cols = self.cfg.columns.len();
                let mut qexpr: Option<FtsQueryExpr> = None;
                let mut phrases: Vec<QueryPhrase> = Vec::new();
                let mut rank_arg: Option<&Value> = None;
                for (arg_i, item) in idx_str.unwrap_or("").split(',').enumerate() {
                    let val = args.get(arg_i);
                    if item == "r" {
                        rank_arg = val;
                        continue;
                    }
                    let Some(col) = item.strip_prefix('M').and_then(|c| c.parse::<usize>().ok())
                    else {
                        continue;
                    };
                    let text = val.and_then(fts5_value_text).unwrap_or_default();
                    let (mut new_expr, _) = QueryParser::parse_query(&self.cfg, &text)?;
                    if col < n_cols {
                        if self.cfg.detail == DetailMode::None {
                            return Err(fts5_query_err(
                                "fts5: column queries are not supported (detail=none)",
                            ));
                        }
                        new_expr.set_colset(&[col]);
                    }
                    // `sqlite3Fts5ExprAnd`: the phrases of the new expression come first.
                    let mut renumbered = Vec::new();
                    new_expr.number_phrases(&mut renumbered);
                    qexpr = Some(match qexpr.take() {
                        None => new_expr,
                        Some(mut old) => {
                            old.number_phrases(&mut renumbered);
                            QueryParser::make_node(QTok::And, old, new_expr)?
                        }
                    });
                    phrases = renumbered;
                }
                let Some(qexpr) = qexpr else {
                    return Ok(());
                };

                let need_rank_val = (idx_num & IDX_FLAG_NEEDS_RANK_COL) != 0;
                let contentless = self.cfg.content_mode == ContentMode::Contentless;
                // detail=column / detail=none: phrase instances come from the row text.
                let populate_from_text = self.cfg.detail != DetailMode::Full && !contentless;
                let need_aux_texts = self.aux_registry.needs_aux_text() || populate_from_text;
                // Column values are read for the result; the text is also needed for the
                // column sizes of a table without %_docsize. A row missing from the content
                // source is an error only where C would look it up (`fts5SeekCursor`).
                let need_cols = (idx_num & IDX_FLAG_NEEDS_CONTENT) != 0;
                let sizes_from_text = !self.cfg.columnsize && !contentless;
                let need_content = need_cols || need_aux_texts || sizes_from_text;

                let (rank_weights, phrase_doc_hits, matched_docs, total_rows, total_tokens) = {
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.ensure_structure_coherent(ctx)?;

                    // `fts5CursorParseRank`
                    let rw = if let Some(rv) = rank_arg {
                        let z = fts5_value_text(rv);
                        let spec = match z {
                            Some(z) if config_rank_is_valid(z.as_bytes()) => z.into_owned(),
                            other => {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    format!(
                                        "parse error in rank function: {}",
                                        other.unwrap_or_default()
                                    ),
                                ));
                            }
                        };
                        if need_rank_val {
                            parse_rank_expression(&spec)?
                        } else {
                            Vec::new()
                        }
                    } else if need_rank_val {
                        st.get_configured_rank_weights(ctx)?
                    } else {
                        Vec::new()
                    };

                    let mut precomputed_phrases = Vec::with_capacity(phrases.len());
                    for (pi, ph) in phrases.iter().enumerate() {
                        precomputed_phrases.push(st.eval_phrase_selective(&self.cfg, ctx, pi, ph)?);
                    }
                    let mut pd_hits = vec![0usize; phrases.len()];
                    let mdocs = eval_query_expr_precomputed(
                        &qexpr,
                        &phrases,
                        &mut precomputed_phrases,
                        &mut pd_hits,
                    );

                    let avgs = st.read_averages(&self.cfg, ctx)?;
                    let tr = avgs[0];
                    let tt: u64 = avgs[1..].iter().sum();
                    (rw, pd_hits, mdocs, tr, tt)
                };

                let colsets = if populate_from_text {
                    let mut v = Vec::with_capacity(phrases.len());
                    collect_phrase_colsets(&qexpr, &mut v);
                    v
                } else {
                    Vec::new()
                };

                let mut aux_rows: Vec<AuxRowEntry> = Vec::with_capacity(matched_docs.len());
                self.rows.reserve(matched_docs.len());
                let inst_key = |x: &PhraseInstance| (x.col_idx, x.token_offset, x.phrase_idx);
                if !need_content && self.cfg.detail == DetailMode::Full {
                    // Rows identified by the index alone: sizes from %_docsize (or unknown).
                    let zero_sizes = DocColSizes::zeros(n_cols);
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    for (rid, mut insts) in matched_docs {
                        let col_sizes = match st.get_or_load_docsize(&self.cfg, ctx, rid)? {
                            Some(sz) => sz,
                            None => zero_sizes.clone(),
                        };
                        let slice = insts.as_mut_slice();
                        if !slice.is_sorted_by_key(inst_key) {
                            slice.sort_unstable_by_key(inst_key);
                        }
                        self.rows.push(Fts5CursorRow {
                            rowid: rid,
                            cols: Vec::new(),
                            aux_handle: 0,
                            rank: None,
                        });
                        aux_rows.push(AuxRowEntry {
                            col_sizes,
                            instances: insts,
                            texts: None,
                            missing_content: false,
                        });
                    }
                } else {
                    for (rid, mut insts) in matched_docs {
                        let mut missing_content = false;
                        let cols = if need_content {
                            match self.load_content_vals(&shadows, ctx, rid) {
                                Ok(v) => v,
                                Err(e)
                                    if !need_cols
                                        && e.code == ErrorCode::Corrupt
                                        && e.message.starts_with("fts5: missing row") =>
                                {
                                    missing_content = true;
                                    Vec::new()
                                }
                                Err(e) => return Err(e),
                            }
                        } else {
                            Vec::new()
                        };
                        let texts = if need_aux_texts && !missing_content {
                            let col_texts: Vec<Option<Vec<u8>>> =
                                if contentless && !self.cfg.unindexed_content {
                                    vec![None; n_cols]
                                } else {
                                    (0..n_cols)
                                        .map(|i| {
                                            cols.get(i)
                                                .and_then(fts5_value_bytes)
                                                .map(std::borrow::Cow::into_owned)
                                        })
                                        .collect()
                                };
                            Some(Box::new(AuxRowTexts { col_texts }))
                        } else {
                            None
                        };
                        // `fts5ApiColumnSize`
                        let col_sizes = if self.cfg.columnsize {
                            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            match st.get_or_load_docsize(&self.cfg, ctx, rid)? {
                                Some(sz) => sz,
                                None => DocColSizes::zeros(n_cols),
                            }
                        } else if contentless {
                            DocColSizes::zeros(n_cols)
                        } else {
                            self.compute_fallback_col_sizes(&cols)
                        };
                        if self.cfg.detail != DetailMode::Full {
                            insts = match texts.as_ref() {
                                Some(t) if populate_from_text => {
                                    let mut active = vec![false; phrases.len()];
                                    for inst in insts.as_slice() {
                                        if let Some(a) = active.get_mut(inst.phrase_idx as usize) {
                                            *a = true;
                                        }
                                    }
                                    populate_instances_from_text(
                                        &self.cfg,
                                        &phrases,
                                        &colsets,
                                        &active,
                                        &t.col_texts,
                                    )
                                }
                                _ => PhraseHits::new(),
                            };
                        }
                        let slice = insts.as_mut_slice();
                        if !slice.is_sorted_by_key(inst_key) {
                            slice.sort_unstable_by_key(inst_key);
                        }
                        self.rows.push(Fts5CursorRow {
                            rowid: rid,
                            cols,
                            aux_handle: 0,
                            rank: None,
                        });
                        aux_rows.push(AuxRowEntry {
                            col_sizes,
                            instances: insts,
                            texts,
                            missing_content,
                        });
                    }
                }

                let mut qctx = AuxQueryContext::build(
                    n_cols,
                    self.cfg.tokenizer.clone(),
                    phrases.iter().map(|p| p.tokens.len()).collect(),
                    &phrase_doc_hits,
                    total_rows,
                    total_tokens,
                    self.unknown_size_cols(),
                    aux_rows,
                );
                qctx.bm25_needs_content = sizes_from_text || populate_from_text;
                let qctx = Arc::new(qctx);
                let qid = self.aux_registry.register_query(Arc::clone(&qctx));
                for (idx, r) in self.rows.iter_mut().enumerate() {
                    r.aux_handle = Fts5AuxRegistry::encode_handle(qid, idx);
                    if need_rank_val {
                        r.rank = Some(compute_bm25_for_row(&qctx, Some(idx), &rank_weights)?);
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
                return Ok(());
            }
            _ => {}
        }

        // A plan without MATCH: the auxiliary functions still work on its rows, with no
        // phrases (`bm25()` is 0, `highlight()` returns the text unchanged).
        if self.aux_registry.needs_aux_text() && !self.rows.is_empty() {
            let n_cols = self.cfg.columns.len();
            let contentless = self.cfg.content_mode == ContentMode::Contentless;
            let (total_rows, total_tokens) = {
                let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                let avgs = st.read_averages(&self.cfg, ctx)?;
                (avgs[0], avgs[1..].iter().sum::<u64>())
            };
            let mut aux_rows = Vec::with_capacity(self.rows.len());
            for r in &self.rows {
                let col_texts: Vec<Option<Vec<u8>>> = if contentless && !self.cfg.unindexed_content
                {
                    vec![None; n_cols]
                } else {
                    (0..n_cols)
                        .map(|i| {
                            r.cols
                                .get(i)
                                .and_then(fts5_value_bytes)
                                .map(std::borrow::Cow::into_owned)
                        })
                        .collect()
                };
                let col_sizes = if self.cfg.columnsize {
                    let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    match st.get_or_load_docsize(&self.cfg, ctx, r.rowid)? {
                        Some(sz) => sz,
                        None => DocColSizes::zeros(n_cols),
                    }
                } else if contentless {
                    DocColSizes::zeros(n_cols)
                } else {
                    self.compute_fallback_col_sizes(&r.cols)
                };
                aux_rows.push(AuxRowEntry {
                    col_sizes,
                    instances: PhraseHits::new(),
                    texts: Some(Box::new(AuxRowTexts { col_texts })),
                    missing_content: false,
                });
            }
            let qctx = Arc::new(AuxQueryContext::build(
                n_cols,
                self.cfg.tokenizer.clone(),
                Vec::new(),
                &[],
                total_rows,
                total_tokens,
                self.unknown_size_cols(),
                aux_rows,
            ));
            let qid = self.aux_registry.register_query(qctx);
            for (idx, r) in self.rows.iter_mut().enumerate() {
                r.aux_handle = Fts5AuxRegistry::encode_handle(qid, idx);
            }
        } else {
            for r in &mut self.rows {
                r.aux_handle = FTS5_AUX_ROWLESS_HANDLE;
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
            Ok(row.rank.map_or(Value::Null, real_result))
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

/// `fts5VocabColumnMethod`: a count of zero is reported as NULL.
fn vocab_count(n: i64) -> Value {
    if n > 0 {
        Value::Integer(n)
    } else {
        Value::Null
    }
}

fn unquote_sql_token(s: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&cfg_dequote_word(s.as_bytes())).into_owned())
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
    let raw_args: Vec<String> = split_ddl_args(&sql[open + 1..close])
        .iter()
        .map(|a| a.trim().to_owned())
        .collect();
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
                // `fts5VocabNextMethod`: instances are only counted for detail=full.
                VocabType::Row => {
                    let doc_cnt = docs.len() as i64;
                    let mut tot_cnt = 0i64;
                    if target_detail == DetailMode::Full {
                        for cols in docs.values() {
                            for offs in cols.values() {
                                tot_cnt += offs.len() as i64;
                            }
                        }
                    }
                    self.rows.push(vec![
                        Value::Text(term_str),
                        vocab_count(doc_cnt),
                        vocab_count(tot_cnt),
                    ]);
                }
                VocabType::Col => {
                    // One row per column the term occurs in, in column order; detail=none
                    // reports every document under the first column.
                    let mut by_col: BTreeMap<usize, (i64, i64)> = BTreeMap::new();
                    for cols in docs.values() {
                        if target_detail == DetailMode::None {
                            by_col.entry(0).or_insert((0, 0)).0 += 1;
                            continue;
                        }
                        for (&cidx, offs) in cols {
                            let e = by_col.entry(cidx).or_insert((0, 0));
                            e.0 += 1;
                            if target_detail == DetailMode::Full {
                                e.1 += offs.len() as i64;
                            }
                        }
                    }
                    for (cidx, (dc, tc)) in by_col {
                        let cname = if target_detail == DetailMode::None {
                            Value::Null
                        } else {
                            Value::Text(
                                col_names
                                    .get(cidx)
                                    .cloned()
                                    .unwrap_or_else(|| format!("c{cidx}")),
                            )
                        };
                        self.rows.push(vec![
                            Value::Text(term_str.clone()),
                            cname,
                            vocab_count(dc),
                            vocab_count(tc),
                        ]);
                    }
                }
                VocabType::Instance => {
                    // detail=full: one row per position; detail=column: one row per
                    // (document, column) without offset; detail=none: one row per document.
                    for (rowid, cols) in docs {
                        if target_detail == DetailMode::None {
                            self.rows.push(vec![
                                Value::Text(term_str.clone()),
                                Value::Integer(rowid),
                                Value::Null,
                                Value::Null,
                            ]);
                            continue;
                        }
                        for (cidx, offs) in cols {
                            let cname = col_names
                                .get(cidx)
                                .cloned()
                                .unwrap_or_else(|| format!("c{cidx}"));
                            if target_detail != DetailMode::Full {
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
