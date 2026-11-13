//! Pre-compiled zero-allocation streaming read, aggregate, `GROUP BY`, ordered `LIMIT`,
//! and 2-table equi-join execution plans over raw Format-3 B+Tree leaves (`P1-C`).
#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::ControlFlow;

use sqliters_btree::Btree;
use sqliters_syntax::{
    BinaryOp, BindParam, ColumnRef, Expr, FromClause, FunctionArgs, IndexHint, JoinConstraint,
    JoinType, LikeOp, LimitClause, OrderByTerm, ResultColumn, SelectCore, SelectStmt,
    TableOrSubquery, UnaryOp,
};
use sqliters_types::{
    Collation, CompiledJsonArrowPath, DatabaseId, ErrorCode, INT_RECORD_MAX_SLOTS, KeySortOrder,
    Pgno, RawField, Result, RuntimeValue, SqliteError, TextEncoding, TypeAffinity, Value,
    fallible_sort_by, format_sqlite_real, parse_raw_record_prefix,
};
use sqliters_vdbe::{
    AggregateAccumulator, AggregateKind, CompiledLikePattern, FastSumAccum, Opcode, P4, eval_arith,
    eval_cast_with_encoding, eval_comparison_with_affinity, eval_unary_negate,
    format_i64_into_stack, parse_escape_char, sqlite_compare_affinity,
};

use crate::compiler::{
    AccessPlan, QueryPlanContext, choose_access_plan_with_context, expr_contains_aggregate,
    expr_implies_partial_where,
};
use crate::db_set::{DatabaseSet, ResolvedRelation};
use crate::engine::{
    STAR_SLOT_SCHEMA, ScopedColumn, build_table_scoped_columns_pub, eval_rowid_range_bound,
    eval_rowid_target_int, expand_projection, subst_select_aliases_pub,
};
use crate::execution_context::{CollationRegistry, FunctionRegistry};
use crate::schema::{IndexSchema, Schema, TableSchema};
use crate::window::core_has_window;

mod int_kernel;

use int_kernel::{CovIntKernel, CovIntKernelShape, IntRow};

#[cfg(test)]
thread_local! {
    /// `(rows aggregated by an integer kernel, rows a bound kernel handed to the general row
    /// code)` on this thread (test instrumentation).
    pub(crate) static INT_KERNEL_ROWS: std::cell::Cell<(u64, u64)> =
        const { std::cell::Cell::new((0, 0)) };
}

/// Counts one row seen by a bound integer kernel (test instrumentation; compiled out otherwise).
#[inline(always)]
fn note_int_kernel_row(_decoded: bool) {
    #[cfg(test)]
    INT_KERNEL_ROWS.with(|rows| {
        let (hits, misses) = rows.get();
        rows.set(if _decoded {
            (hits + 1, misses)
        } else {
            (hits, misses + 1)
        });
    });
}

// ============================================================================
// Fast Non-Cryptographic Hasher (`std` only, 100% safe Rust)
// ============================================================================

#[derive(Default, Clone, Copy)]
struct FastHasher(u64);

impl std::hash::Hasher for FastHasher {
    #[inline(always)]
    fn finish(&self) -> u64 {
        let h = self.0;
        h ^ (h >> 33)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        self.0 = hash_bytes_with_seed(self.0, bytes);
    }

    #[inline(always)]
    fn write_i64(&mut self, i: i64) {
        self.write_u64(i as u64);
    }

    #[inline(always)]
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0 ^ i).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type FastBuildHasher = std::hash::BuildHasherDefault<FastHasher>;

#[inline]
fn hash_bytes_with_seed(seed: u64, bytes: &[u8]) -> u64 {
    let mut h = seed ^ (bytes.len() as u64).wrapping_mul(0x517C_C1B7_2722_0A95);
    let (chunks, rem) = bytes.as_chunks::<8>();
    for chunk in chunks {
        let w = u64::from_le_bytes(*chunk);
        h = (h ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    for &b in rem {
        h = (h ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    h ^ (h >> 33)
}

#[inline]
fn hash_bytes_fast(bytes: &[u8]) -> u64 {
    let h = hash_bytes_with_seed(0xCBF2_9CE4_8422_2325, bytes);
    if h == 0 { 1 } else { h }
}

// ============================================================================
// Arena-Backed Open-Addressed Byte Set & Multi-Map (2 Heap Allocations Total)
// ============================================================================

const BLOB_TAG_BIT: u32 = 1u32 << 31;

#[derive(Clone, Copy, Default)]
struct ArenaSetSlot {
    /// Non-zero hash (`0` means empty slot).
    hash: u64,
    off: u32,
    /// Bits `0..31`: byte length; bit `31`: `1` if `Blob`, `0` if `Text`.
    len_and_kind: u32,
}

/// Compact open-addressed set of `Text`/`Blob` keys backed by a single contiguous byte arena.
struct FastArenaByteSet {
    arena: Vec<u8>,
    slots: Vec<ArenaSetSlot>,
    mask: usize,
    len: usize,
}

impl FastArenaByteSet {
    fn with_capacity(est_items: usize, est_key_bytes: usize) -> Self {
        let cap = (est_items.saturating_mul(2).max(16)).next_power_of_two();
        Self {
            arena: Vec::with_capacity(est_items.saturating_mul(est_key_bytes)),
            slots: vec![ArenaSetSlot::default(); cap],
            mask: cap - 1,
            len: 0,
        }
    }

    fn grow(&mut self) {
        let new_cap = (self.slots.len() * 2).max(16);
        let new_mask = new_cap - 1;
        let mut new_slots = vec![ArenaSetSlot::default(); new_cap];
        for s in &self.slots {
            if s.hash != 0 {
                let mut idx = (s.hash as usize) & new_mask;
                while new_slots[idx].hash != 0 {
                    idx = (idx + 1) & new_mask;
                }
                new_slots[idx] = *s;
            }
        }
        self.slots = new_slots;
        self.mask = new_mask;
    }

    #[inline]
    fn insert(&mut self, bytes: &[u8], is_blob: bool) {
        if bytes.len() >= (BLOB_TAG_BIT as usize) {
            return;
        }
        if (self.len + 1) * 10 >= self.slots.len() * 7 {
            self.grow();
        }
        let h = hash_bytes_fast(bytes);
        let lk = (bytes.len() as u32) | if is_blob { BLOB_TAG_BIT } else { 0 };
        let mut idx = (h as usize) & self.mask;
        loop {
            let slot = self.slots[idx];
            if slot.hash == 0 {
                let Ok(off) = u32::try_from(self.arena.len()) else {
                    return;
                };
                self.arena.extend_from_slice(bytes);
                self.slots[idx] = ArenaSetSlot {
                    hash: h,
                    off,
                    len_and_kind: lk,
                };
                self.len += 1;
                return;
            }
            if slot.hash == h && slot.len_and_kind == lk {
                let s_off = slot.off as usize;
                let s_len = (lk & !BLOB_TAG_BIT) as usize;
                if &self.arena[s_off..s_off + s_len] == bytes {
                    return;
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }

    #[inline]
    fn insert_text_affinity_field(&mut self, field: RawField<'_>, enc: TextEncoding) {
        match field {
            RawField::Null => {}
            RawField::Text(b) => {
                if enc == TextEncoding::Utf8 {
                    self.insert(b, false);
                } else {
                    let s = enc.decode_bytes(b);
                    self.insert(s.as_bytes(), false);
                }
            }
            RawField::Blob(b) => self.insert(b, true),
            RawField::Int(i) => {
                let mut buf = [0u8; 24];
                let slice = format_i64_into_stack(i, &mut buf);
                self.insert(slice, false);
            }
            RawField::Float(f) => {
                let s = format_sqlite_real(f);
                self.insert(s.as_bytes(), false);
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ArenaMapBucket {
    hash: u64,
    off: u32,
    len_and_kind: u32,
    head_entry: u32,
}

impl Default for ArenaMapBucket {
    fn default() -> Self {
        Self {
            hash: 0,
            off: 0,
            len_and_kind: 0,
            head_entry: u32::MAX,
        }
    }
}

struct ArenaMultiMap<T> {
    arena: Vec<u8>,
    buckets: Vec<ArenaMapBucket>,
    mask: usize,
    used_buckets: usize,
    entries: Vec<(T, u32)>,
}

impl<T> ArenaMultiMap<T> {
    fn with_capacity(est_items: usize, est_key_bytes: usize) -> Self {
        let cap = (est_items.saturating_mul(2).max(16)).next_power_of_two();
        Self {
            arena: Vec::with_capacity(est_items.saturating_mul(est_key_bytes)),
            buckets: vec![ArenaMapBucket::default(); cap],
            mask: cap - 1,
            used_buckets: 0,
            entries: Vec::with_capacity(est_items),
        }
    }

    fn grow(&mut self) {
        let new_cap = (self.buckets.len() * 2).max(16);
        let new_mask = new_cap - 1;
        let mut new_buckets = vec![ArenaMapBucket::default(); new_cap];
        for b in &self.buckets {
            if b.hash != 0 {
                let mut idx = (b.hash as usize) & new_mask;
                while new_buckets[idx].hash != 0 {
                    idx = (idx + 1) & new_mask;
                }
                new_buckets[idx] = *b;
            }
        }
        self.buckets = new_buckets;
        self.mask = new_mask;
    }

    fn insert(&mut self, bytes: &[u8], is_blob: bool, payload: T) {
        if bytes.len() >= (BLOB_TAG_BIT as usize) {
            return;
        }
        if (self.used_buckets + 1) * 10 >= self.buckets.len() * 7 {
            self.grow();
        }
        let h = hash_bytes_fast(bytes);
        let lk = (bytes.len() as u32) | if is_blob { BLOB_TAG_BIT } else { 0 };
        let entry_idx = self.entries.len() as u32;
        let mut idx = (h as usize) & self.mask;
        loop {
            let b = self.buckets[idx];
            if b.hash == 0 {
                let Ok(off) = u32::try_from(self.arena.len()) else {
                    return;
                };
                self.arena.extend_from_slice(bytes);
                self.entries.push((payload, u32::MAX));
                self.buckets[idx] = ArenaMapBucket {
                    hash: h,
                    off,
                    len_and_kind: lk,
                    head_entry: entry_idx,
                };
                self.used_buckets += 1;
                return;
            }
            if b.hash == h && b.len_and_kind == lk {
                let s_off = b.off as usize;
                let s_len = (lk & !BLOB_TAG_BIT) as usize;
                if &self.arena[s_off..s_off + s_len] == bytes {
                    self.entries.push((payload, b.head_entry));
                    self.buckets[idx].head_entry = entry_idx;
                    return;
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }

    #[inline]
    fn find_head(&self, bytes: &[u8], is_blob: bool) -> u32 {
        if self.used_buckets == 0 || bytes.len() >= (BLOB_TAG_BIT as usize) {
            return u32::MAX;
        }
        let h = hash_bytes_fast(bytes);
        let lk = (bytes.len() as u32) | if is_blob { BLOB_TAG_BIT } else { 0 };
        let mut idx = (h as usize) & self.mask;
        loop {
            let b = self.buckets[idx];
            if b.hash == 0 {
                return u32::MAX;
            }
            if b.hash == h && b.len_and_kind == lk {
                let s_off = b.off as usize;
                let s_len = (lk & !BLOB_TAG_BIT) as usize;
                if &self.arena[s_off..s_off + s_len] == bytes {
                    return b.head_entry;
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }
}

// ============================================================================
// Table & Column Metadata + Raw Row View
// ============================================================================

#[derive(Debug, Clone)]
struct FastColMeta {
    default_val: Value,
}

#[derive(Debug, Clone)]
struct FastTableDesc {
    root_pgno: Pgno,
    columns: Box<[FastColMeta]>,
    needs_record_parse: bool,
    max_record_col: usize,
    /// Columns with REAL affinity. C stores an integral value of such a column as an integer
    /// (`OP_MakeRecord` with an affinity string) and turns it back into a real when the column
    /// is read (`OP_RealAffinity`); every decoded row gets the same treatment here.
    real_cols: Box<[u16]>,
}

impl FastTableDesc {
    /// Number of table column slots a decoded row of this table occupies.
    #[inline]
    const fn col_buf_len(&self) -> usize {
        if self.needs_record_parse {
            self.max_record_col + 1
        } else {
            0
        }
    }

    #[inline]
    fn default_raw_field<'a>(val: &'a Value) -> RawField<'a> {
        match val {
            Value::Null => RawField::Null,
            Value::Integer(i) => RawField::Int(*i),
            Value::Real(r) => RawField::Float(*r),
            Value::Text(s) => RawField::Text(s.as_bytes()),
            Value::Blob(b) => RawField::Blob(b.as_slice()),
        }
    }

    #[inline(always)]
    fn fill_from_table_payload<'a>(
        &'a self,
        payload: &'a [u8],
        out: &mut [RawField<'a>],
    ) -> Result<()> {
        if !self.needs_record_parse {
            return Ok(());
        }
        let prefix_len = self.max_record_col + 1;
        let parsed = parse_raw_record_prefix(payload, &mut out[..prefix_len])?;
        if parsed < prefix_len {
            for (slot, meta) in out[parsed..prefix_len]
                .iter_mut()
                .zip(&self.columns[parsed..prefix_len])
            {
                *slot = Self::default_raw_field(&meta.default_val);
            }
        }
        if !self.real_cols.is_empty() {
            self.apply_real_affinity(out);
        }
        Ok(())
    }

    /// `OP_RealAffinity` for the decoded row (kept out of line: most tables have no REAL column).
    #[cold]
    #[inline(never)]
    fn apply_real_affinity(&self, out: &mut [RawField<'_>]) {
        for &ci in &self.real_cols {
            if let Some(slot) = out.get_mut(ci as usize)
                && let RawField::Int(i) = *slot
            {
                *slot = RawField::Float(i as f64);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FastColSrc {
    Rowid {
        side: u8,
    },
    RecordCol {
        side: u8,
        col_idx: u16,
        is_ipk: bool,
    },
}

impl FastColSrc {
    #[inline]
    fn mark_usage(self, tables: &mut [FastTableDesc]) {
        if let Self::RecordCol {
            side,
            col_idx,
            is_ipk: false,
        } = self
        {
            let td = &mut tables[side as usize];
            td.needs_record_parse = true;
            if (col_idx as usize) > td.max_record_col {
                td.max_record_col = col_idx as usize;
            }
        }
    }
}

struct FastRowCtx<'a> {
    rowid0: i64,
    cols0: &'a [RawField<'a>],
    rowid1: i64,
    cols1: &'a [RawField<'a>],
    params: &'a [Value],
    enc: TextEncoding,
}

impl<'a> FastRowCtx<'a> {
    #[inline(always)]
    fn single(
        rowid: i64,
        cols: &'a [RawField<'a>],
        params: &'a [Value],
        enc: TextEncoding,
    ) -> Self {
        Self {
            rowid0: rowid,
            cols0: cols,
            rowid1: 0,
            cols1: &[],
            params,
            enc,
        }
    }

    #[inline(always)]
    fn get_col(&self, src: FastColSrc) -> RawField<'a> {
        match src {
            FastColSrc::Rowid { side: 0 } => RawField::Int(self.rowid0),
            FastColSrc::Rowid { .. } => RawField::Int(self.rowid1),
            FastColSrc::RecordCol {
                side: 0,
                is_ipk: true,
                ..
            } => RawField::Int(self.rowid0),
            FastColSrc::RecordCol {
                side: _,
                is_ipk: true,
                ..
            } => RawField::Int(self.rowid1),
            FastColSrc::RecordCol {
                side: 0,
                col_idx,
                is_ipk: false,
            } => self
                .cols0
                .get(col_idx as usize)
                .copied()
                .unwrap_or(RawField::Null),
            FastColSrc::RecordCol {
                side: _,
                col_idx,
                is_ipk: false,
            } => self
                .cols1
                .get(col_idx as usize)
                .copied()
                .unwrap_or(RawField::Null),
        }
    }

    #[inline(always)]
    fn get_param(&self, idx: usize) -> &'a Value {
        self.params.get(idx).unwrap_or(&Value::Null)
    }
}

// ============================================================================
// Pre-Compiled Scalar Micro-Evaluator (`FastExpr` + `ConcatChain`)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FastCmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Is,
    IsNot,
}

impl FastCmpOp {
    #[inline]
    const fn to_opcode(self) -> Opcode {
        match self {
            Self::Eq => Opcode::Eq,
            Self::Ne => Opcode::Ne,
            Self::Lt => Opcode::Lt,
            Self::Le => Opcode::Le,
            Self::Gt => Opcode::Gt,
            Self::Ge => Opcode::Ge,
            Self::Is => Opcode::Is,
            Self::IsNot => Opcode::IsNot,
        }
    }

    #[inline]
    const fn flip(self) -> Self {
        match self {
            Self::Eq => Self::Eq,
            Self::Ne => Self::Ne,
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
            Self::Is => Self::Is,
            Self::IsNot => Self::IsNot,
        }
    }

    #[inline(always)]
    const fn matches_ordering(self, ord: Ordering) -> bool {
        match self {
            Self::Eq | Self::Is => matches!(ord, Ordering::Equal),
            Self::Ne | Self::IsNot => !matches!(ord, Ordering::Equal),
            Self::Lt => matches!(ord, Ordering::Less),
            Self::Le => !matches!(ord, Ordering::Greater),
            Self::Gt => matches!(ord, Ordering::Greater),
            Self::Ge => !matches!(ord, Ordering::Less),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FastArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl FastArithOp {
    #[inline]
    const fn to_opcode(self) -> Opcode {
        match self {
            Self::Add => Opcode::Add,
            Self::Sub => Opcode::Subtract,
            Self::Mul => Opcode::Multiply,
            Self::Div => Opcode::Divide,
            Self::Rem => Opcode::Remainder,
        }
    }
}

#[derive(Debug, Clone)]
enum FastScalarRhs {
    Const(Value),
    Param(usize),
}

impl FastScalarRhs {
    #[inline(always)]
    fn resolve<'a>(&'a self, params: &'a [Value]) -> &'a Value {
        match self {
            Self::Const(v) => v,
            Self::Param(idx) => params.get(*idx).unwrap_or(&Value::Null),
        }
    }
}

#[derive(Debug, Clone)]
enum FastConcatPart {
    Literal(Box<str>),
    Col(FastColSrc),
    LowerHexCol(FastColSrc),
    UpperHexCol(FastColSrc),
    CastTextCol(FastColSrc),
    Expr(FastExpr),
}

#[derive(Debug, Clone)]
enum FastExpr {
    Const(Value),
    Param(usize),
    Col(FastColSrc),
    UnaryPlus(Box<FastExpr>),
    Negate(Box<FastExpr>),
    Not(Box<FastExpr>),
    IsNull {
        inner: Box<FastExpr>,
        negated: bool,
    },
    ArithCols {
        left: FastColSrc,
        op: FastArithOp,
        right: FastColSrc,
    },
    Arith {
        left: Box<FastExpr>,
        op: FastArithOp,
        right: Box<FastExpr>,
    },
    ColCmpRhs {
        col: FastColSrc,
        op: FastCmpOp,
        rhs: FastScalarRhs,
        affinity: TypeAffinity,
        collation: Collation,
    },
    ColBetweenRhs {
        col: FastColSrc,
        low: FastScalarRhs,
        high: FastScalarRhs,
        negated: bool,
        affinity: TypeAffinity,
        collation: Collation,
    },
    JsonArrowCmpRhs {
        col: FastColSrc,
        path: CompiledJsonArrowPath,
        as_text: bool,
        op: FastCmpOp,
        rhs: FastScalarRhs,
    },
    ColLike {
        col: FastColSrc,
        compiled: CompiledLikePattern,
        negated: bool,
    },
    Compare {
        left: Box<FastExpr>,
        op: FastCmpOp,
        right: Box<FastExpr>,
        affinity: TypeAffinity,
        collation: Collation,
    },
    And(Box<FastExpr>, Box<FastExpr>),
    Or(Box<FastExpr>, Box<FastExpr>),
    JsonArrow {
        col: FastColSrc,
        path: CompiledJsonArrowPath,
        as_text: bool,
    },
    Cast {
        inner: Box<FastExpr>,
        type_name: Box<str>,
    },
    Coalesce(Box<[FastExpr]>),
    Hex(Box<FastExpr>),
    LowerHex(Box<FastExpr>),
    UpperHex(Box<FastExpr>),
    LowerAscii(Box<FastExpr>),
    UpperAscii(Box<FastExpr>),
    ConcatChain {
        parts: Box<[FastConcatPart]>,
        est_cap: usize,
    },
}

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";

#[inline]
fn push_hex_bytes(out: &mut String, bytes: &[u8], table: &[u8; 16]) {
    out.reserve(bytes.len() * 2);
    for &b in bytes {
        out.push(table[(b >> 4) as usize] as char);
        out.push(table[(b & 0x0f) as usize] as char);
    }
}

#[inline]
fn push_raw_field_hex(
    out: &mut String,
    field: RawField<'_>,
    enc: TextEncoding,
    table: &[u8; 16],
) -> bool {
    match field {
        RawField::Null => false,
        RawField::Blob(b) => {
            push_hex_bytes(out, b, table);
            true
        }
        RawField::Text(b) => {
            if enc == TextEncoding::Utf8 {
                push_hex_bytes(out, b, table);
            } else {
                let s = enc.decode_bytes(b);
                push_hex_bytes(out, s.as_bytes(), table);
            }
            true
        }
        RawField::Int(i) => {
            let mut buf = [0u8; 24];
            let s = format_i64_into_stack(i, &mut buf);
            push_hex_bytes(out, s, table);
            true
        }
        RawField::Float(f) => {
            let s = format_sqlite_real(f);
            push_hex_bytes(out, s.as_bytes(), table);
            true
        }
    }
}

#[inline]
fn push_raw_field_text(
    out: &mut String,
    field: RawField<'_>,
    enc: TextEncoding,
    cast_blob: bool,
) -> bool {
    match field {
        RawField::Null => false,
        RawField::Text(b) => {
            if enc == TextEncoding::Utf8 {
                if let Ok(s) = std::str::from_utf8(b) {
                    out.push_str(s);
                } else {
                    out.push_str(&String::from_utf8_lossy(b));
                }
            } else {
                out.push_str(&enc.decode_bytes(b));
            }
            true
        }
        RawField::Blob(b) => {
            if cast_blob && enc != TextEncoding::Utf8 {
                out.push_str(&enc.decode_bytes(b));
            } else if let Ok(s) = std::str::from_utf8(b) {
                out.push_str(s);
            } else {
                out.push_str(&String::from_utf8_lossy(b));
            }
            true
        }
        RawField::Int(i) => {
            let mut buf = [0u8; 24];
            let s = format_i64_into_stack(i, &mut buf);
            // SAFETY-FREE: `format_i64_into_stack` writes pure ASCII `'-'` and `'0'..'9'`.
            if let Ok(ascii) = std::str::from_utf8(s) {
                out.push_str(ascii);
            }
            true
        }
        RawField::Float(f) => {
            out.push_str(&format_sqlite_real(f));
            true
        }
    }
}

#[inline]
fn compare_raw_field_with_rhs(
    lhs: RawField<'_>,
    op: FastCmpOp,
    rhs: &Value,
    affinity: TypeAffinity,
    collation: Collation,
    enc: TextEncoding,
) -> Option<bool> {
    if matches!(op, FastCmpOp::Is | FastCmpOp::IsNot) {
        let eq = match (matches!(lhs, RawField::Null), rhs.is_null()) {
            (true, true) => true,
            (true, false) | (false, true) => false,
            (false, false) => {
                compare_nonnull_raw_with_rhs(lhs, rhs, affinity, collation, enc) == Ordering::Equal
            }
        };
        return Some(if op == FastCmpOp::Is { eq } else { !eq });
    }
    if matches!(lhs, RawField::Null) || rhs.is_null() {
        return None;
    }
    let ord = compare_nonnull_raw_with_rhs(lhs, rhs, affinity, collation, enc);
    Some(op.matches_ordering(ord))
}

#[inline]
const fn key_order_for_collation(collation: Collation) -> KeySortOrder {
    KeySortOrder {
        desc: false,
        nulls_last: false,
        collation,
        custom_collation: None,
    }
}

#[inline]
fn compare_nonnull_raw_with_rhs(
    lhs: RawField<'_>,
    rhs: &Value,
    affinity: TypeAffinity,
    collation: Collation,
    enc: TextEncoding,
) -> Ordering {
    // Fast zero-allocation paths covering 99.9% of SQL comparisons:
    match (lhs, rhs) {
        (RawField::Int(li), Value::Integer(ri)) if !matches!(affinity, TypeAffinity::Text) => {
            return li.cmp(ri);
        }
        (RawField::Text(lb), Value::Text(rs))
            if enc == TextEncoding::Utf8
                && matches!(affinity, TypeAffinity::Blob | TypeAffinity::Text) =>
        {
            match collation {
                Collation::Binary => return lb.cmp(rs.as_bytes()),
                Collation::RTrim => {
                    let lt = trim_ascii_spaces_end(lb);
                    let rt = trim_ascii_spaces_end(rs.as_bytes());
                    return lt.cmp(rt);
                }
                Collation::NoCase => {
                    return cmp_ascii_nocase_bytes(lb, rs.as_bytes());
                }
            }
        }
        (RawField::Float(lf), Value::Real(rf)) if !matches!(affinity, TypeAffinity::Text) => {
            return lf.total_cmp(rf);
        }
        _ => {}
    }

    if affinity == TypeAffinity::Blob {
        let spec = key_order_for_collation(collation);
        return lhs
            .compare_sql_to_nonnull_value(rhs, &spec, enc)
            .unwrap_or(Ordering::Equal);
    }

    let lv = lhs.to_owned_value(enc);
    match eval_comparison_with_affinity(Opcode::Eq, &lv, rhs, affinity, collation, enc) {
        Value::Integer(1) => Ordering::Equal,
        _ => match eval_comparison_with_affinity(Opcode::Lt, &lv, rhs, affinity, collation, enc) {
            Value::Integer(1) => Ordering::Less,
            _ => Ordering::Greater,
        },
    }
}

#[inline]
fn trim_ascii_spaces_end(mut b: &[u8]) -> &[u8] {
    while let Some((&b' ', rest)) = b.split_last() {
        b = rest;
    }
    b
}

#[inline]
fn cmp_ascii_nocase_bytes(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        let ca = a[i].to_ascii_lowercase();
        let cb = b[i].to_ascii_lowercase();
        if ca != cb {
            return ca.cmp(&cb);
        }
    }
    a.len().cmp(&b.len())
}

impl FastExpr {
    /// Evaluates boolean truthiness (`Some(true)` in SQL `WHERE`), avoiding `Value` heap allocations.
    #[inline]
    fn eval_where_true(&self, ctx: &FastRowCtx<'_>) -> Result<bool> {
        match self {
            Self::ColCmpRhs {
                col,
                op,
                rhs,
                affinity,
                collation,
            } => {
                let lf = ctx.get_col(*col);
                let rv = rhs.resolve(ctx.params);
                Ok(
                    compare_raw_field_with_rhs(lf, *op, rv, *affinity, *collation, ctx.enc)
                        == Some(true),
                )
            }
            Self::ColBetweenRhs {
                col,
                low,
                high,
                negated,
                affinity,
                collation,
            } => {
                let lf = ctx.get_col(*col);
                if matches!(lf, RawField::Null) {
                    return Ok(false);
                }
                let lv = low.resolve(ctx.params);
                let hv = high.resolve(ctx.params);
                if !*negated {
                    if lv.is_null() || hv.is_null() {
                        return Ok(false);
                    }
                    let ge_lo = compare_raw_field_with_rhs(
                        lf,
                        FastCmpOp::Ge,
                        lv,
                        *affinity,
                        *collation,
                        ctx.enc,
                    ) == Some(true);
                    if !ge_lo {
                        return Ok(false);
                    }
                    let le_hi = compare_raw_field_with_rhs(
                        lf,
                        FastCmpOp::Le,
                        hv,
                        *affinity,
                        *collation,
                        ctx.enc,
                    ) == Some(true);
                    Ok(le_hi)
                } else {
                    // `NOT (x >= low AND x <= high)` == `(x < low) OR (x > high)`
                    let lt_lo = !lv.is_null()
                        && compare_raw_field_with_rhs(
                            lf,
                            FastCmpOp::Lt,
                            lv,
                            *affinity,
                            *collation,
                            ctx.enc,
                        ) == Some(true);
                    if lt_lo {
                        return Ok(true);
                    }
                    let gt_hi = !hv.is_null()
                        && compare_raw_field_with_rhs(
                            lf,
                            FastCmpOp::Gt,
                            hv,
                            *affinity,
                            *collation,
                            ctx.enc,
                        ) == Some(true);
                    Ok(gt_hi)
                }
            }
            Self::JsonArrowCmpRhs {
                col,
                path,
                as_text,
                op,
                rhs,
            } => {
                let cf = ctx.get_col(*col);
                let extracted = path.eval_raw_field(cf, ctx.enc, *as_text)?.value;
                let rv = rhs.resolve(ctx.params);
                if let (Value::Integer(li), Value::Integer(ri)) = (&extracted, rv) {
                    return Ok(op.matches_ordering(li.cmp(ri)));
                }
                if matches!(op, FastCmpOp::Is | FastCmpOp::IsNot) {
                    let eq = match (extracted.is_null(), rv.is_null()) {
                        (true, true) => true,
                        (true, false) | (false, true) => false,
                        (false, false) => {
                            extracted.cmp_sqlite_with_encoding(rv, Collation::Binary, ctx.enc)
                                == Ordering::Equal
                        }
                    };
                    return Ok(if *op == FastCmpOp::Is { eq } else { !eq });
                }
                if extracted.is_null() || rv.is_null() {
                    return Ok(false);
                }
                let ord = extracted.cmp_sqlite_with_encoding(rv, Collation::Binary, ctx.enc);
                Ok(op.matches_ordering(ord))
            }
            Self::ColLike {
                col,
                compiled,
                negated,
            } => {
                let cf = ctx.get_col(*col);
                let Some(m) = compiled.matches_raw_field(cf, ctx.enc) else {
                    return Ok(false);
                };
                Ok(if *negated { !m } else { m })
            }
            Self::And(a, b) => Ok(a.eval_where_true(ctx)? && b.eval_where_true(ctx)?),
            Self::Or(a, b) => Ok(a.eval_where_true(ctx)? || b.eval_where_true(ctx)?),
            Self::IsNull { inner, negated } => {
                if let Self::Col(c) = inner.as_ref() {
                    let is_n = matches!(ctx.get_col(*c), RawField::Null);
                    Ok(if *negated { !is_n } else { is_n })
                } else {
                    let is_n = inner.eval_value(ctx)?.is_null();
                    Ok(if *negated { !is_n } else { is_n })
                }
            }
            other => Ok(other.eval_value(ctx)?.truthiness() == Some(true)),
        }
    }

    /// Evaluates directly into a `FastSumAccum` without allocating intermediate `Value`s for
    /// columns, JSONB `->>` numeric extractions, or integer arithmetic (`t1.score * t2.weight`).
    #[inline]
    fn step_into_sum(&self, ctx: &FastRowCtx<'_>, acc: &mut FastSumAccum) -> Result<()> {
        match self {
            Self::Col(c) => {
                acc.step_raw_field(ctx.get_col(*c), ctx.enc);
                Ok(())
            }
            Self::JsonArrow { col, path, as_text } => {
                let v = path
                    .eval_raw_field(ctx.get_col(*col), ctx.enc, *as_text)?
                    .value;
                acc.step_value(&v);
                Ok(())
            }
            Self::ArithCols { left, op, right } => {
                let lf = ctx.get_col(*left);
                let rf = ctx.get_col(*right);
                if let (RawField::Int(a), RawField::Int(b)) = (lf, rf) {
                    match op {
                        FastArithOp::Mul => {
                            if let Some(p) = a.checked_mul(b) {
                                acc.step_i64(p);
                            } else {
                                acc.step_f64((a as f64) * (b as f64));
                            }
                            return Ok(());
                        }
                        FastArithOp::Add => {
                            if let Some(s) = a.checked_add(b) {
                                acc.step_i64(s);
                            } else {
                                acc.step_f64((a as f64) + (b as f64));
                            }
                            return Ok(());
                        }
                        FastArithOp::Sub => {
                            if let Some(d) = a.checked_sub(b) {
                                acc.step_i64(d);
                            } else {
                                acc.step_f64((a as f64) - (b as f64));
                            }
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                let v = self.eval_value(ctx)?;
                acc.step_value(&v);
                Ok(())
            }
            other => {
                let v = other.eval_value(ctx)?;
                acc.step_value(&v);
                Ok(())
            }
        }
    }

    #[inline]
    fn is_nonnull_for_count(&self, ctx: &FastRowCtx<'_>) -> Result<bool> {
        match self {
            Self::Col(c) => Ok(!matches!(ctx.get_col(*c), RawField::Null)),
            other => Ok(!other.eval_value(ctx)?.is_null()),
        }
    }

    fn eval_value(&self, ctx: &FastRowCtx<'_>) -> Result<Value> {
        match self {
            Self::Const(v) => Ok(v.clone()),
            Self::Param(idx) => Ok(ctx.get_param(*idx).clone()),
            Self::Col(c) => Ok(ctx.get_col(*c).to_owned_value(ctx.enc)),
            Self::UnaryPlus(inner) => inner.eval_value(ctx),
            Self::Negate(inner) => Ok(eval_unary_negate(&inner.eval_value(ctx)?)),
            Self::Not(inner) => Ok(match inner.eval_value(ctx)?.truthiness() {
                None => Value::Null,
                Some(b) => Value::Integer(i64::from(!b)),
            }),
            Self::IsNull { inner, negated } => {
                let is_n = if let Self::Col(c) = inner.as_ref() {
                    matches!(ctx.get_col(*c), RawField::Null)
                } else {
                    inner.eval_value(ctx)?.is_null()
                };
                Ok(Value::Integer(i64::from(if *negated {
                    !is_n
                } else {
                    is_n
                })))
            }
            Self::ArithCols { left, op, right } => {
                let lf = ctx.get_col(*left);
                let rf = ctx.get_col(*right);
                if let (RawField::Int(a), RawField::Int(b)) = (lf, rf) {
                    return Ok(match op {
                        FastArithOp::Add => a
                            .checked_add(b)
                            .map_or_else(|| Value::real((a as f64) + (b as f64)), Value::Integer),
                        FastArithOp::Sub => a
                            .checked_sub(b)
                            .map_or_else(|| Value::real((a as f64) - (b as f64)), Value::Integer),
                        FastArithOp::Mul => a
                            .checked_mul(b)
                            .map_or_else(|| Value::real((a as f64) * (b as f64)), Value::Integer),
                        FastArithOp::Div => {
                            if b == 0 {
                                Value::Null
                            } else if a == i64::MIN && b == -1 {
                                Value::real(-(a as f64))
                            } else {
                                Value::Integer(a / b)
                            }
                        }
                        FastArithOp::Rem => {
                            if b == 0 {
                                Value::Null
                            } else if a == i64::MIN && b == -1 {
                                Value::Integer(0)
                            } else {
                                Value::Integer(a % b)
                            }
                        }
                    });
                }
                let lv = lf.to_owned_value(ctx.enc);
                let rv = rf.to_owned_value(ctx.enc);
                Ok(eval_arith(op.to_opcode(), &lv, &rv))
            }
            Self::Arith { left, op, right } => {
                let lv = left.eval_value(ctx)?;
                let rv = right.eval_value(ctx)?;
                Ok(eval_arith(op.to_opcode(), &lv, &rv))
            }
            Self::ColCmpRhs {
                col,
                op,
                rhs,
                affinity,
                collation,
            } => {
                let lf = ctx.get_col(*col);
                let rv = rhs.resolve(ctx.params);
                Ok(
                    match compare_raw_field_with_rhs(lf, *op, rv, *affinity, *collation, ctx.enc) {
                        None => Value::Null,
                        Some(b) => Value::Integer(i64::from(b)),
                    },
                )
            }
            Self::ColBetweenRhs {
                col,
                low,
                high,
                negated,
                affinity,
                collation,
            } => {
                let lf = ctx.get_col(*col);
                let lv = low.resolve(ctx.params);
                let hv = high.resolve(ctx.params);
                let ge = compare_raw_field_with_rhs(
                    lf,
                    FastCmpOp::Ge,
                    lv,
                    *affinity,
                    *collation,
                    ctx.enc,
                );
                let le = compare_raw_field_with_rhs(
                    lf,
                    FastCmpOp::Le,
                    hv,
                    *affinity,
                    *collation,
                    ctx.enc,
                );
                let between = match (ge, le) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
                Ok(match between {
                    None => Value::Null,
                    Some(b) => Value::Integer(i64::from(if *negated { !b } else { b })),
                })
            }
            Self::JsonArrowCmpRhs {
                col,
                path,
                as_text,
                op,
                rhs,
            } => {
                let extracted = path
                    .eval_raw_field(ctx.get_col(*col), ctx.enc, *as_text)?
                    .value;
                let rv = rhs.resolve(ctx.params);
                Ok(eval_comparison_with_affinity(
                    op.to_opcode(),
                    &extracted,
                    rv,
                    TypeAffinity::Blob,
                    Collation::Binary,
                    ctx.enc,
                ))
            }
            Self::ColLike {
                col,
                compiled,
                negated,
            } => Ok(
                match compiled.matches_raw_field(ctx.get_col(*col), ctx.enc) {
                    None => Value::Null,
                    Some(m) => Value::Integer(i64::from(if *negated { !m } else { m })),
                },
            ),
            Self::Compare {
                left,
                op,
                right,
                affinity,
                collation,
            } => {
                let lv = left.eval_value(ctx)?;
                let rv = right.eval_value(ctx)?;
                Ok(eval_comparison_with_affinity(
                    op.to_opcode(),
                    &lv,
                    &rv,
                    *affinity,
                    *collation,
                    ctx.enc,
                ))
            }
            Self::And(a, b) => {
                let av = a.eval_value(ctx)?.truthiness();
                if av == Some(false) {
                    return Ok(Value::Integer(0));
                }
                let bv = b.eval_value(ctx)?.truthiness();
                Ok(match (av, bv) {
                    (Some(false), _) | (_, Some(false)) => Value::Integer(0),
                    (Some(true), Some(true)) => Value::Integer(1),
                    _ => Value::Null,
                })
            }
            Self::Or(a, b) => {
                let av = a.eval_value(ctx)?.truthiness();
                if av == Some(true) {
                    return Ok(Value::Integer(1));
                }
                let bv = b.eval_value(ctx)?.truthiness();
                Ok(match (av, bv) {
                    (Some(true), _) | (_, Some(true)) => Value::Integer(1),
                    (Some(false), Some(false)) => Value::Integer(0),
                    _ => Value::Null,
                })
            }
            Self::JsonArrow { col, path, as_text } => Ok(path
                .eval_raw_field(ctx.get_col(*col), ctx.enc, *as_text)?
                .value),
            Self::Cast { inner, type_name } => {
                let v = inner.eval_value(ctx)?;
                Ok(eval_cast_with_encoding(v, type_name, ctx.enc))
            }
            Self::Coalesce(args) => {
                for a in args.iter() {
                    let v = a.eval_value(ctx)?;
                    if !v.is_null() {
                        return Ok(v);
                    }
                }
                Ok(Value::Null)
            }
            Self::Hex(inner) | Self::UpperHex(inner) => {
                if let Self::Col(c) = inner.as_ref() {
                    let mut out = String::new();
                    if !push_raw_field_hex(&mut out, ctx.get_col(*c), ctx.enc, HEX_UPPER) {
                        return Ok(Value::Null);
                    }
                    return Ok(Value::Text(out));
                }
                let v = inner.eval_value(ctx)?;
                if v.is_null() {
                    return Ok(Value::Null);
                }
                let mut out = String::new();
                match v {
                    Value::Blob(b) => push_hex_bytes(&mut out, &b, HEX_UPPER),
                    Value::Text(s) => push_hex_bytes(&mut out, s.as_bytes(), HEX_UPPER),
                    other => push_hex_bytes(&mut out, other.to_text_lossy().as_bytes(), HEX_UPPER),
                }
                Ok(Value::Text(out))
            }
            Self::LowerHex(inner) => {
                if let Self::Col(c) = inner.as_ref() {
                    let mut out = String::new();
                    if !push_raw_field_hex(&mut out, ctx.get_col(*c), ctx.enc, HEX_LOWER) {
                        return Ok(Value::Null);
                    }
                    return Ok(Value::Text(out));
                }
                let v = inner.eval_value(ctx)?;
                if v.is_null() {
                    return Ok(Value::Null);
                }
                let mut out = String::new();
                match v {
                    Value::Blob(b) => push_hex_bytes(&mut out, &b, HEX_LOWER),
                    Value::Text(s) => push_hex_bytes(&mut out, s.as_bytes(), HEX_LOWER),
                    other => push_hex_bytes(&mut out, other.to_text_lossy().as_bytes(), HEX_LOWER),
                }
                Ok(Value::Text(out))
            }
            Self::LowerAscii(inner) => {
                let v = inner.eval_value(ctx)?;
                if v.is_null() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Text(v.to_text_lossy().to_ascii_lowercase()))
                }
            }
            Self::UpperAscii(inner) => {
                let v = inner.eval_value(ctx)?;
                if v.is_null() {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Text(v.to_text_lossy().to_ascii_uppercase()))
                }
            }
            Self::ConcatChain { parts, est_cap } => {
                let mut out = String::with_capacity(*est_cap);
                for part in parts.iter() {
                    match part {
                        FastConcatPart::Literal(lit) => out.push_str(lit),
                        FastConcatPart::Col(c) => {
                            if !push_raw_field_text(&mut out, ctx.get_col(*c), ctx.enc, false) {
                                return Ok(Value::Null);
                            }
                        }
                        FastConcatPart::CastTextCol(c) => {
                            if !push_raw_field_text(&mut out, ctx.get_col(*c), ctx.enc, true) {
                                return Ok(Value::Null);
                            }
                        }
                        FastConcatPart::LowerHexCol(c) => {
                            if !push_raw_field_hex(&mut out, ctx.get_col(*c), ctx.enc, HEX_LOWER) {
                                return Ok(Value::Null);
                            }
                        }
                        FastConcatPart::UpperHexCol(c) => {
                            if !push_raw_field_hex(&mut out, ctx.get_col(*c), ctx.enc, HEX_UPPER) {
                                return Ok(Value::Null);
                            }
                        }
                        FastConcatPart::Expr(e) => {
                            let v = e.eval_value(ctx)?;
                            if v.is_null() {
                                return Ok(Value::Null);
                            }
                            match v {
                                Value::Text(s) => out.push_str(&s),
                                Value::Integer(i) => {
                                    let mut buf = [0u8; 24];
                                    if let Ok(s) =
                                        std::str::from_utf8(format_i64_into_stack(i, &mut buf))
                                    {
                                        out.push_str(s);
                                    }
                                }
                                Value::Real(r) => out.push_str(&format_sqlite_real(r)),
                                Value::Blob(b) => out.push_str(&String::from_utf8_lossy(&b)),
                                Value::Null => return Ok(Value::Null),
                            }
                        }
                    }
                }
                Ok(Value::Text(out))
            }
        }
    }

    #[inline]
    fn eval_runtime_value(&self, ctx: &FastRowCtx<'_>) -> Result<RuntimeValue> {
        match self {
            Self::JsonArrow { col, path, as_text } => {
                path.eval_raw_field(ctx.get_col(*col), ctx.enc, *as_text)
            }
            _ => Ok(RuntimeValue::plain(self.eval_value(ctx)?)),
        }
    }

    fn mark_usage(&self, tables: &mut [FastTableDesc]) {
        match self {
            Self::Const(_) | Self::Param(_) => {}
            Self::Col(c)
            | Self::ColCmpRhs { col: c, .. }
            | Self::ColBetweenRhs { col: c, .. }
            | Self::JsonArrowCmpRhs { col: c, .. }
            | Self::ColLike { col: c, .. }
            | Self::JsonArrow { col: c, .. } => c.mark_usage(tables),
            Self::ArithCols { left, right, .. } => {
                left.mark_usage(tables);
                right.mark_usage(tables);
            }
            Self::UnaryPlus(e)
            | Self::Negate(e)
            | Self::Not(e)
            | Self::IsNull { inner: e, .. }
            | Self::Cast { inner: e, .. }
            | Self::Hex(e)
            | Self::LowerHex(e)
            | Self::UpperHex(e)
            | Self::LowerAscii(e)
            | Self::UpperAscii(e) => e.mark_usage(tables),
            Self::Arith { left, right, .. }
            | Self::Compare { left, right, .. }
            | Self::And(left, right)
            | Self::Or(left, right) => {
                left.mark_usage(tables);
                right.mark_usage(tables);
            }
            Self::Coalesce(es) => {
                for e in es.iter() {
                    e.mark_usage(tables);
                }
            }
            Self::ConcatChain { parts, .. } => {
                for p in parts.iter() {
                    match p {
                        FastConcatPart::Literal(_) => {}
                        FastConcatPart::Col(c)
                        | FastConcatPart::CastTextCol(c)
                        | FastConcatPart::LowerHexCol(c)
                        | FastConcatPart::UpperHexCol(c) => c.mark_usage(tables),
                        FastConcatPart::Expr(e) => e.mark_usage(tables),
                    }
                }
            }
        }
    }
}

// ============================================================================
// Streaming Aggregate Specs & Accumulators
// ============================================================================

#[derive(Debug, Clone)]
enum FastAggSpec {
    CountStar,
    Count(FastExpr),
    Sum(FastExpr),
    Total(FastExpr),
    Avg(FastExpr),
    Min(FastExpr, Collation),
    Max(FastExpr, Collation),
    JsonGroupArray(FastExpr),
}

impl FastAggSpec {
    fn mark_usage(&self, tables: &mut [FastTableDesc]) {
        match self {
            Self::CountStar => {}
            Self::Count(e)
            | Self::Sum(e)
            | Self::Total(e)
            | Self::Avg(e)
            | Self::Min(e, _)
            | Self::Max(e, _)
            | Self::JsonGroupArray(e) => e.mark_usage(tables),
        }
    }
}

#[derive(Debug, Clone)]
enum FastAggState {
    Count(i64),
    Sum(FastSumAccum),
    MinMax {
        is_max: bool,
        collation: Collation,
        best: Option<Value>,
    },
    JsonArray(AggregateAccumulator),
}

impl FastAggState {
    fn from_spec(spec: &FastAggSpec, enc: TextEncoding) -> Self {
        match spec {
            FastAggSpec::CountStar | FastAggSpec::Count(_) => Self::Count(0),
            FastAggSpec::Sum(_) | FastAggSpec::Total(_) | FastAggSpec::Avg(_) => {
                Self::Sum(FastSumAccum::new())
            }
            FastAggSpec::Min(_, coll) => Self::MinMax {
                is_max: false,
                collation: *coll,
                best: None,
            },
            FastAggSpec::Max(_, coll) => Self::MinMax {
                is_max: true,
                collation: *coll,
                best: None,
            },
            FastAggSpec::JsonGroupArray(_) => Self::JsonArray(
                AggregateAccumulator::new(AggregateKind::JsonGroupArray, false).with_encoding(enc),
            ),
        }
    }

    #[inline]
    fn reset(&mut self, spec: &FastAggSpec, enc: TextEncoding) {
        *self = Self::from_spec(spec, enc);
    }

    #[inline]
    fn step(&mut self, spec: &FastAggSpec, ctx: &FastRowCtx<'_>) -> Result<()> {
        match (self, spec) {
            (Self::Count(n), FastAggSpec::CountStar) => {
                *n += 1;
                Ok(())
            }
            (Self::Count(n), FastAggSpec::Count(e)) => {
                if e.is_nonnull_for_count(ctx)? {
                    *n += 1;
                }
                Ok(())
            }
            (Self::Sum(acc), FastAggSpec::Sum(e) | FastAggSpec::Total(e) | FastAggSpec::Avg(e)) => {
                e.step_into_sum(ctx, acc)
            }
            (
                Self::MinMax {
                    is_max,
                    collation,
                    best,
                },
                FastAggSpec::Min(e, _) | FastAggSpec::Max(e, _),
            ) => {
                let v = e.eval_value(ctx)?;
                if v.is_null() {
                    return Ok(());
                }
                match best {
                    None => *best = Some(v),
                    Some(cur) => {
                        let ord = v.cmp_sqlite_with_encoding(cur, *collation, ctx.enc);
                        let better = if *is_max {
                            ord == Ordering::Greater
                        } else {
                            ord == Ordering::Less
                        };
                        if better {
                            *cur = v;
                        }
                    }
                }
                Ok(())
            }
            (Self::JsonArray(acc), FastAggSpec::JsonGroupArray(e)) => {
                let rv = e.eval_runtime_value(ctx)?;
                acc.step_rt(&[rv])
            }
            _ => unreachable!(),
        }
    }

    #[inline]
    fn finalize(&self, spec: &FastAggSpec) -> Result<Value> {
        match (self, spec) {
            (Self::Count(n), _) => Ok(Value::Integer(*n)),
            (Self::Sum(acc), FastAggSpec::Sum(_)) => acc.finalize_sum(),
            (Self::Sum(acc), FastAggSpec::Total(_)) => Ok(acc.finalize_total()),
            (Self::Sum(acc), FastAggSpec::Avg(_)) => Ok(acc.finalize_avg()),
            (Self::MinMax { best, .. }, _) => Ok(best.clone().unwrap_or(Value::Null)),
            (Self::JsonArray(acc), _) => acc.finalize(),
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone)]
enum FastGroupProjExpr {
    GroupKey(usize),
    AggSlot(usize),
    CoalesceAggConst { slot: usize, fallback: Value },
    Const(Value),
}

impl FastGroupProjExpr {
    #[inline]
    fn eval(&self, keys: &[Value], aggs: &[Value]) -> Value {
        match self {
            Self::GroupKey(i) => keys.get(*i).cloned().unwrap_or(Value::Null),
            Self::AggSlot(i) => aggs.get(*i).cloned().unwrap_or(Value::Null),
            Self::CoalesceAggConst { slot, fallback } => {
                let v = aggs.get(*slot).cloned().unwrap_or(Value::Null);
                if v.is_null() { fallback.clone() } else { v }
            }
            Self::Const(v) => v.clone(),
        }
    }
}

// ============================================================================
// AST -> `FastExpr` / `FastAccessPath` Compiler
// ============================================================================

#[derive(Debug, Clone)]
struct FastTableBinding {
    side: u8,
    schema_name: String,
    table_name: String,
    eff_alias: String,
    has_explicit_alias: bool,
    tbl: TableSchema,
}

#[derive(Debug, Clone, Copy)]
struct FastResolvedCol {
    src: FastColSrc,
    affinity: Option<TypeAffinity>,
    collation: Collation,
}

struct FastCompileScope<'a> {
    bindings: &'a [FastTableBinding],
    slot_cols: &'a [Option<FastResolvedCol>],
    functions: &'a FunctionRegistry,
    collations: &'a CollationRegistry,
}

fn default_value_for_column(col: &crate::schema::ColumnSchema, table_strict: bool) -> Value {
    match col.default_p4_for_table(table_strict) {
        P4::None => Value::Null,
        P4::Int64(i) => Value::Integer(i),
        P4::Real(r) => Value::real(r),
        P4::Str(s) => Value::Text(s),
        P4::Blob(b) => Value::Blob(b),
        _ => Value::Null,
    }
}

fn build_fast_table_desc(tbl: &TableSchema) -> Option<FastTableDesc> {
    if tbl.without_rowid {
        return None;
    }
    let mut cols = Vec::with_capacity(tbl.columns.len());
    for c in &tbl.columns {
        if c.generated.is_some() || c.custom_collation.is_some() {
            return None;
        }
        cols.push(FastColMeta {
            default_val: default_value_for_column(c, tbl.strict),
        });
    }
    let real_cols: Vec<u16> = tbl
        .columns
        .iter()
        .enumerate()
        .filter(|(ci, c)| {
            c.affinity == TypeAffinity::Real
                && tbl.rowid_alias_col != Some(*ci)
                && u16::try_from(*ci).is_ok()
        })
        .map(|(ci, _)| ci as u16)
        .collect();
    if tbl.columns.len() > usize::from(u16::MAX) && !real_cols.is_empty() {
        return None;
    }
    Some(FastTableDesc {
        root_pgno: tbl.root_pgno,
        columns: cols.into_boxed_slice(),
        needs_record_parse: false,
        max_record_col: 0,
        real_cols: real_cols.into_boxed_slice(),
    })
}

fn build_slot_col_map(
    sample_cols: &[ScopedColumn],
    bindings: &[FastTableBinding],
) -> Vec<Option<FastResolvedCol>> {
    let mut out = Vec::with_capacity(sample_cols.len());
    for sc in sample_cols {
        if sc.custom_collation.is_some() || sc.is_vtab || sc.hidden_vtab {
            out.push(None);
            continue;
        }
        let Some(ref t_alias) = sc.table_alias else {
            out.push(None);
            continue;
        };
        let Some(b) = bindings.iter().find(|b| {
            b.eff_alias.eq_ignore_ascii_case(t_alias)
                && sc
                    .schema_name
                    .as_deref()
                    .is_none_or(|s| b.schema_name.eq_ignore_ascii_case(s))
        }) else {
            out.push(None);
            continue;
        };
        if sc.hidden_rowid {
            out.push(Some(FastResolvedCol {
                src: FastColSrc::Rowid { side: b.side },
                affinity: Some(TypeAffinity::Integer),
                collation: Collation::Binary,
            }));
        } else if let Some((ci, col_def)) = b
            .tbl
            .columns
            .iter()
            .enumerate()
            .find(|(_, c)| c.name.eq_ignore_ascii_case(&sc.name))
        {
            if col_def.custom_collation.is_some()
                || col_def.generated.is_some()
                || ci > u16::MAX as usize
            {
                out.push(None);
            } else {
                let is_ipk = b.tbl.rowid_alias_col == Some(ci);
                out.push(Some(FastResolvedCol {
                    src: FastColSrc::RecordCol {
                        side: b.side,
                        col_idx: ci as u16,
                        is_ipk,
                    },
                    affinity: Some(col_def.affinity),
                    collation: col_def.collation,
                }));
            }
        } else {
            out.push(None);
        }
    }
    out
}

impl FastCompileScope<'_> {
    fn resolve_col_ref(&self, cref: &ColumnRef) -> Option<FastResolvedCol> {
        if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA) {
            let idx = cref.column.parse::<usize>().ok()?;
            return self.slot_cols.get(idx).copied().flatten();
        }
        let mut found: Option<FastResolvedCol> = None;
        for b in self.bindings {
            if let Some(ref sq) = cref.schema {
                // In SQLite, `schema.alias.col` is invalid when `alias` is an explicit table alias
                // unless `alias` matches the underlying table name (`resolveIdentExpr`).
                if !b.schema_name.eq_ignore_ascii_case(sq) {
                    continue;
                }
                if let Some(ref tq) = cref.table {
                    if b.has_explicit_alias {
                        if !b.eff_alias.eq_ignore_ascii_case(tq)
                            || !b.table_name.eq_ignore_ascii_case(tq)
                        {
                            return None;
                        }
                    } else if !b.table_name.eq_ignore_ascii_case(tq) {
                        continue;
                    }
                }
            } else if let Some(ref tq) = cref.table
                && !b.eff_alias.eq_ignore_ascii_case(tq)
            {
                continue;
            }

            // Explicit column match takes precedence over `rowid` / `_rowid_` / `oid`.
            if let Some((ci, cdef)) = b
                .tbl
                .columns
                .iter()
                .enumerate()
                .find(|(_, c)| c.name.eq_ignore_ascii_case(&cref.column))
            {
                if cdef.custom_collation.is_some()
                    || cdef.generated.is_some()
                    || ci > u16::MAX as usize
                {
                    return None;
                }
                if found.is_some() {
                    return None;
                }
                let is_ipk = b.tbl.rowid_alias_col == Some(ci);
                found = Some(FastResolvedCol {
                    src: FastColSrc::RecordCol {
                        side: b.side,
                        col_idx: ci as u16,
                        is_ipk,
                    },
                    affinity: Some(cdef.affinity),
                    collation: cdef.collation,
                });
                continue;
            }

            if !b.tbl.without_rowid
                && (cref.column.eq_ignore_ascii_case("rowid")
                    || cref.column.eq_ignore_ascii_case("_rowid_")
                    || cref.column.eq_ignore_ascii_case("oid"))
            {
                if found.is_some() {
                    return None;
                }
                found = Some(FastResolvedCol {
                    src: FastColSrc::Rowid { side: b.side },
                    affinity: Some(TypeAffinity::Integer),
                    collation: Collation::Binary,
                });
            }
        }
        found
    }

    fn builtin_not_overridden(&self, name_lower: &str, argc: usize) -> bool {
        self.functions.find_scalar(name_lower, argc).is_none()
            && self.functions.find_aggregate(name_lower, argc).is_none()
    }

    fn resolve_builtin_collation(&self, name: &str) -> Option<Collation> {
        if self.collations.get_custom(name).is_some() {
            return None;
        }
        if name.eq_ignore_ascii_case("BINARY") {
            Some(Collation::Binary)
        } else if name.eq_ignore_ascii_case("NOCASE") {
            Some(Collation::NoCase)
        } else if name.eq_ignore_ascii_case("RTRIM") {
            Some(Collation::RTrim)
        } else {
            None
        }
    }

    fn expr_meta(&self, expr: &Expr) -> (Option<TypeAffinity>, Option<Collation>) {
        match expr {
            Expr::Column(cref) => {
                if let Some(rc) = self.resolve_col_ref(cref) {
                    (rc.affinity, Some(rc.collation))
                } else {
                    (None, None)
                }
            }
            Expr::Collate {
                expr: inner,
                collation,
            } => {
                let (aff, _) = self.expr_meta(inner);
                (aff, self.resolve_builtin_collation(collation))
            }
            Expr::Cast { type_name, .. } => {
                (Some(TypeAffinity::from_declared_type(type_name)), None)
            }
            _ => (None, None),
        }
    }
}

fn eval_const_literal_expr(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Null => Some(Value::Null),
        Expr::Bool(b) => Some(Value::Integer(i64::from(*b))),
        Expr::Integer(i) => Some(Value::Integer(*i)),
        Expr::Float(f) => Some(Value::real(*f)),
        Expr::String(s) => Some(Value::Text(s.clone())),
        Expr::Blob(b) => Some(Value::Blob(b.clone())),
        Expr::Unary {
            op: UnaryOp::Plus,
            expr: inner,
        } => match eval_const_literal_expr(inner)? {
            v @ (Value::Integer(_) | Value::Real(_)) => Some(v),
            _ => None,
        },
        Expr::Unary {
            op: UnaryOp::Negate,
            expr: inner,
        } => match inner.as_ref() {
            Expr::Integer(i) => i
                .checked_neg()
                .map(Value::Integer)
                .or_else(|| Some(Value::real(-(*i as f64)))),
            Expr::Float(f) => Some(Value::real(-*f)),
            _ => None,
        },
        _ => None,
    }
}

fn compile_scalar_rhs(expr: &Expr) -> Option<FastScalarRhs> {
    if let Some(v) = eval_const_literal_expr(expr) {
        return Some(FastScalarRhs::Const(v));
    }
    if let Expr::Bind(bp) = expr {
        let idx1 = match bp {
            BindParam::Anonymous(i) | BindParam::Numbered(i) | BindParam::Named(_, i) => *i,
        };
        if idx1 >= 1 {
            return Some(FastScalarRhs::Param(idx1 - 1));
        }
    }
    None
}

fn map_binary_cmp_op(op: BinaryOp) -> Option<FastCmpOp> {
    match op {
        BinaryOp::Eq => Some(FastCmpOp::Eq),
        BinaryOp::Ne => Some(FastCmpOp::Ne),
        BinaryOp::Lt => Some(FastCmpOp::Lt),
        BinaryOp::Le => Some(FastCmpOp::Le),
        BinaryOp::Gt => Some(FastCmpOp::Gt),
        BinaryOp::Ge => Some(FastCmpOp::Ge),
        BinaryOp::Is => Some(FastCmpOp::Is),
        BinaryOp::IsNot => Some(FastCmpOp::IsNot),
        _ => None,
    }
}

fn map_binary_arith_op(op: BinaryOp) -> Option<FastArithOp> {
    match op {
        BinaryOp::Add => Some(FastArithOp::Add),
        BinaryOp::Subtract => Some(FastArithOp::Sub),
        BinaryOp::Multiply => Some(FastArithOp::Mul),
        BinaryOp::Divide => Some(FastArithOp::Div),
        BinaryOp::Remainder => Some(FastArithOp::Rem),
        _ => None,
    }
}

fn flatten_concat_ast<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::Concat,
        right,
    } = expr
    {
        flatten_concat_ast(left, out);
        flatten_concat_ast(right, out);
    } else {
        out.push(expr);
    }
}

fn eval_const_char_fn(args: &[Expr], scope: &FastCompileScope<'_>) -> Option<String> {
    if !scope.builtin_not_overridden("char", args.len()) {
        return None;
    }
    let mut s = String::with_capacity(args.len());
    for a in args {
        let Value::Integer(code) = eval_const_literal_expr(a)? else {
            return None;
        };
        let cp = u32::try_from(code).ok()?;
        let ch = char::from_u32(cp)?;
        s.push(ch);
    }
    Some(s)
}

fn compile_concat_chain(expr: &Expr, scope: &FastCompileScope<'_>) -> Option<FastExpr> {
    let mut raw_nodes = Vec::with_capacity(8);
    flatten_concat_ast(expr, &mut raw_nodes);
    let mut parts: Vec<FastConcatPart> = Vec::with_capacity(raw_nodes.len());
    let mut pending_lit = String::new();
    let mut est_cap: usize = 0;

    let flush_lit = |pending: &mut String, parts: &mut Vec<FastConcatPart>, est_cap: &mut usize| {
        if !pending.is_empty() {
            *est_cap += pending.len();
            let s = std::mem::take(pending);
            parts.push(FastConcatPart::Literal(s.into_boxed_str()));
        }
    };

    for node in raw_nodes {
        if let Expr::String(s) = node {
            pending_lit.push_str(s);
            continue;
        }
        if let Expr::FunctionCall {
            name,
            distinct: false,
            args: FunctionArgs::Exprs(args),
            filter: None,
            over: None,
        } = node
        {
            if name.eq_ignore_ascii_case("char")
                && let Some(ch_s) = eval_const_char_fn(args, scope)
            {
                pending_lit.push_str(&ch_s);
                continue;
            }
            if (name.eq_ignore_ascii_case("lower") || name.eq_ignore_ascii_case("upper"))
                && args.len() == 1
                && scope.builtin_not_overridden(&name.to_ascii_lowercase(), 1)
                && let Expr::FunctionCall {
                    name: inner_name,
                    distinct: false,
                    args: FunctionArgs::Exprs(inner_args),
                    filter: None,
                    over: None,
                } = &args[0]
                && inner_name.eq_ignore_ascii_case("hex")
                && inner_args.len() == 1
                && scope.builtin_not_overridden("hex", 1)
                && let Expr::Column(cref) = &inner_args[0]
                && let Some(rc) = scope.resolve_col_ref(cref)
            {
                flush_lit(&mut pending_lit, &mut parts, &mut est_cap);
                est_cap += 64;
                if name.eq_ignore_ascii_case("lower") {
                    parts.push(FastConcatPart::LowerHexCol(rc.src));
                } else {
                    parts.push(FastConcatPart::UpperHexCol(rc.src));
                }
                continue;
            }
            if name.eq_ignore_ascii_case("hex")
                && args.len() == 1
                && scope.builtin_not_overridden("hex", 1)
                && let Expr::Column(cref) = &args[0]
                && let Some(rc) = scope.resolve_col_ref(cref)
            {
                flush_lit(&mut pending_lit, &mut parts, &mut est_cap);
                est_cap += 64;
                parts.push(FastConcatPart::UpperHexCol(rc.src));
                continue;
            }
        }
        if let Expr::Cast {
            expr: inner,
            type_name,
        } = node
            && TypeAffinity::from_declared_type(type_name) == TypeAffinity::Text
            && let Expr::Column(cref) = inner.as_ref()
            && let Some(rc) = scope.resolve_col_ref(cref)
        {
            flush_lit(&mut pending_lit, &mut parts, &mut est_cap);
            est_cap += 16;
            parts.push(FastConcatPart::CastTextCol(rc.src));
            continue;
        }
        if let Expr::Column(cref) = node
            && let Some(rc) = scope.resolve_col_ref(cref)
        {
            flush_lit(&mut pending_lit, &mut parts, &mut est_cap);
            est_cap += 24;
            parts.push(FastConcatPart::Col(rc.src));
            continue;
        }

        flush_lit(&mut pending_lit, &mut parts, &mut est_cap);
        est_cap += 24;
        parts.push(FastConcatPart::Expr(compile_fast_expr(node, scope)?));
    }
    flush_lit(&mut pending_lit, &mut parts, &mut est_cap);

    Some(FastExpr::ConcatChain {
        parts: parts.into_boxed_slice(),
        est_cap: est_cap.max(32),
    })
}

fn try_extract_json_arrow_col(
    expr: &Expr,
    scope: &FastCompileScope<'_>,
) -> Option<(FastColSrc, CompiledJsonArrowPath, bool)> {
    if let Expr::Binary {
        left,
        op: op @ (BinaryOp::JsonExtract | BinaryOp::JsonExtractText),
        right,
    } = expr
        && let Expr::Column(cref) = left.as_ref()
        && let Expr::String(path_str) = right.as_ref()
    {
        let as_text = *op == BinaryOp::JsonExtractText;
        let fn_name = if as_text { "->>" } else { "->" };
        if !scope.builtin_not_overridden(fn_name, 2) {
            return None;
        }
        let rc = scope.resolve_col_ref(cref)?;
        let compiled = CompiledJsonArrowPath::compile(path_str);
        return Some((rc.src, compiled, as_text));
    }
    None
}

fn compile_fast_expr(expr: &Expr, scope: &FastCompileScope<'_>) -> Option<FastExpr> {
    if let Some(v) = eval_const_literal_expr(expr) {
        return Some(FastExpr::Const(v));
    }
    match expr {
        Expr::Bind(bp) => {
            let idx1 = match bp {
                BindParam::Anonymous(i) | BindParam::Numbered(i) | BindParam::Named(_, i) => *i,
            };
            if idx1 >= 1 {
                Some(FastExpr::Param(idx1 - 1))
            } else {
                None
            }
        }
        Expr::Column(cref) => Some(FastExpr::Col(scope.resolve_col_ref(cref)?.src)),
        Expr::Collate {
            expr: inner,
            collation,
        } => {
            let _ = scope.resolve_builtin_collation(collation)?;
            compile_fast_expr(inner, scope)
        }
        Expr::Unary { op, expr: inner } => match op {
            UnaryOp::Plus => {
                // Note: `+col` strips column affinity in SQLite; keep `UnaryPlus` only when inner
                // is already numeric/general expression; comparisons check AST directly via `expr_meta`.
                Some(FastExpr::UnaryPlus(Box::new(compile_fast_expr(
                    inner, scope,
                )?)))
            }
            UnaryOp::Negate => Some(FastExpr::Negate(Box::new(compile_fast_expr(inner, scope)?))),
            UnaryOp::Not => Some(FastExpr::Not(Box::new(compile_fast_expr(inner, scope)?))),
            UnaryOp::BitNot => None,
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Some(FastExpr::IsNull {
            inner: Box::new(compile_fast_expr(inner, scope)?),
            negated: *negated,
        }),
        Expr::Between {
            expr: target,
            low,
            high,
            negated,
        } => {
            if let Expr::Column(cref) = target.as_ref()
                && let Some(rc) = scope.resolve_col_ref(cref)
                && let (Some(lo_rhs), Some(hi_rhs)) =
                    (compile_scalar_rhs(low), compile_scalar_rhs(high))
            {
                let (_, lo_coll) = scope.expr_meta(low);
                let (_, hi_coll) = scope.expr_meta(high);
                if lo_coll.is_none() && hi_coll.is_none() {
                    let aff = sqlite_compare_affinity(rc.affinity, None);
                    return Some(FastExpr::ColBetweenRhs {
                        col: rc.src,
                        low: lo_rhs,
                        high: hi_rhs,
                        negated: *negated,
                        affinity: aff,
                        collation: rc.collation,
                    });
                }
            }
            None
        }
        Expr::Like {
            expr: target,
            pattern,
            escape,
            op: LikeOp::Like,
            negated,
        } => {
            let argc = if escape.is_some() { 3 } else { 2 };
            if !scope.builtin_not_overridden("like", argc) {
                return None;
            }
            let Expr::Column(cref) = target.as_ref() else {
                return None;
            };
            let rc = scope.resolve_col_ref(cref)?;
            let Expr::String(pat_str) = pattern.as_ref() else {
                return None;
            };
            let esc_char = match escape {
                None => None,
                Some(esc_e) => {
                    let ev = eval_const_literal_expr(esc_e)?;
                    if ev.is_null() {
                        return Some(FastExpr::Const(Value::Null));
                    }
                    Some(parse_escape_char(&ev.to_text_lossy()).ok()?)
                }
            };
            let compiled = CompiledLikePattern::compile(pat_str, esc_char, true);
            Some(FastExpr::ColLike {
                col: rc.src,
                compiled,
                negated: *negated,
            })
        }
        Expr::Cast {
            expr: inner,
            type_name,
        } => Some(FastExpr::Cast {
            inner: Box::new(compile_fast_expr(inner, scope)?),
            type_name: type_name.clone().into_boxed_str(),
        }),
        Expr::Binary { left, op, right } => {
            if *op == BinaryOp::Concat {
                return compile_concat_chain(expr, scope);
            }
            if matches!(op, BinaryOp::JsonExtract | BinaryOp::JsonExtractText) {
                let (col, path, as_text) = try_extract_json_arrow_col(expr, scope)?;
                return Some(FastExpr::JsonArrow { col, path, as_text });
            }
            if *op == BinaryOp::And {
                return Some(FastExpr::And(
                    Box::new(compile_fast_expr(left, scope)?),
                    Box::new(compile_fast_expr(right, scope)?),
                ));
            }
            if *op == BinaryOp::Or {
                return Some(FastExpr::Or(
                    Box::new(compile_fast_expr(left, scope)?),
                    Box::new(compile_fast_expr(right, scope)?),
                ));
            }
            if let Some(aop) = map_binary_arith_op(*op) {
                if let (Expr::Column(lc), Expr::Column(rc)) = (left.as_ref(), right.as_ref()) {
                    let l_res = scope.resolve_col_ref(lc)?;
                    let r_res = scope.resolve_col_ref(rc)?;
                    return Some(FastExpr::ArithCols {
                        left: l_res.src,
                        op: aop,
                        right: r_res.src,
                    });
                }
                return Some(FastExpr::Arith {
                    left: Box::new(compile_fast_expr(left, scope)?),
                    op: aop,
                    right: Box::new(compile_fast_expr(right, scope)?),
                });
            }
            if let Some(cop) = map_binary_cmp_op(*op) {
                let (l_aff, l_coll) = scope.expr_meta(left);
                let (r_aff, r_coll) = scope.expr_meta(right);
                // If either side had an unsupported custom collation inside `Expr::Collate`, bail out:
                if matches!(left.as_ref(), Expr::Collate { .. }) && l_coll.is_none() {
                    return None;
                }
                if matches!(right.as_ref(), Expr::Collate { .. }) && r_coll.is_none() {
                    return None;
                }
                let eff_aff = sqlite_compare_affinity(l_aff, r_aff);
                // Explicit `COLLATE` overrides implicit column collation (`exprCompareCollSeq`).
                let eff_coll = if matches!(left.as_ref(), Expr::Collate { .. }) {
                    l_coll?
                } else if matches!(right.as_ref(), Expr::Collate { .. }) {
                    r_coll?
                } else {
                    l_coll.or(r_coll).unwrap_or(Collation::Binary)
                };

                if let Expr::Column(lc) = left.as_ref()
                    && let Some(l_rc) = scope.resolve_col_ref(lc)
                    && let Some(rhs) = compile_scalar_rhs(right)
                {
                    return Some(FastExpr::ColCmpRhs {
                        col: l_rc.src,
                        op: cop,
                        rhs,
                        affinity: eff_aff,
                        collation: eff_coll,
                    });
                }
                if let Expr::Column(rc_ast) = right.as_ref()
                    && let Some(r_rc) = scope.resolve_col_ref(rc_ast)
                    && let Some(lhs) = compile_scalar_rhs(left)
                {
                    return Some(FastExpr::ColCmpRhs {
                        col: r_rc.src,
                        op: cop.flip(),
                        rhs: lhs,
                        affinity: eff_aff,
                        collation: eff_coll,
                    });
                }
                if eff_aff == TypeAffinity::Blob
                    && eff_coll == Collation::Binary
                    && let Some((col, path, as_text)) = try_extract_json_arrow_col(left, scope)
                    && let Some(rhs) = compile_scalar_rhs(right)
                {
                    return Some(FastExpr::JsonArrowCmpRhs {
                        col,
                        path,
                        as_text,
                        op: cop,
                        rhs,
                    });
                }
                if eff_aff == TypeAffinity::Blob
                    && eff_coll == Collation::Binary
                    && let Some((col, path, as_text)) = try_extract_json_arrow_col(right, scope)
                    && let Some(lhs) = compile_scalar_rhs(left)
                {
                    return Some(FastExpr::JsonArrowCmpRhs {
                        col,
                        path,
                        as_text,
                        op: cop.flip(),
                        rhs: lhs,
                    });
                }

                return Some(FastExpr::Compare {
                    left: Box::new(compile_fast_expr(left, scope)?),
                    op: cop,
                    right: Box::new(compile_fast_expr(right, scope)?),
                    affinity: eff_aff,
                    collation: eff_coll,
                });
            }
            None
        }
        Expr::FunctionCall {
            name,
            distinct: false,
            args: FunctionArgs::Exprs(args),
            filter: None,
            over: None,
        } => {
            let lower_name = name.to_ascii_lowercase();
            if !scope.builtin_not_overridden(&lower_name, args.len()) {
                return None;
            }
            match lower_name.as_str() {
                "char" => Some(FastExpr::Const(Value::Text(eval_const_char_fn(
                    args, scope,
                )?))),
                "coalesce" | "ifnull" if args.len() >= 2 => {
                    let mut compiled_args = Vec::with_capacity(args.len());
                    for a in args {
                        compiled_args.push(compile_fast_expr(a, scope)?);
                    }
                    Some(FastExpr::Coalesce(compiled_args.into_boxed_slice()))
                }
                "hex" if args.len() == 1 => {
                    Some(FastExpr::Hex(Box::new(compile_fast_expr(&args[0], scope)?)))
                }
                "lower" if args.len() == 1 => {
                    if let Expr::FunctionCall {
                        name: in_name,
                        distinct: false,
                        args: FunctionArgs::Exprs(in_args),
                        filter: None,
                        over: None,
                    } = &args[0]
                        && in_name.eq_ignore_ascii_case("hex")
                        && in_args.len() == 1
                        && scope.builtin_not_overridden("hex", 1)
                    {
                        return Some(FastExpr::LowerHex(Box::new(compile_fast_expr(
                            &in_args[0],
                            scope,
                        )?)));
                    }
                    Some(FastExpr::LowerAscii(Box::new(compile_fast_expr(
                        &args[0], scope,
                    )?)))
                }
                "upper" if args.len() == 1 => {
                    if let Expr::FunctionCall {
                        name: in_name,
                        distinct: false,
                        args: FunctionArgs::Exprs(in_args),
                        filter: None,
                        over: None,
                    } = &args[0]
                        && in_name.eq_ignore_ascii_case("hex")
                        && in_args.len() == 1
                        && scope.builtin_not_overridden("hex", 1)
                    {
                        return Some(FastExpr::UpperHex(Box::new(compile_fast_expr(
                            &in_args[0],
                            scope,
                        )?)));
                    }
                    Some(FastExpr::UpperAscii(Box::new(compile_fast_expr(
                        &args[0], scope,
                    )?)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

// ============================================================================
// Single-Table Raw Access Paths (`Table` & `CoveringIndex`)
// ============================================================================

#[derive(Debug, Clone)]
struct FastRangeBound {
    rhs: FastScalarRhs,
    inclusive: bool,
}

#[derive(Debug, Clone)]
struct FastCoveringIndex {
    root_pgno: Pgno,
    orders: Box<[KeySortOrder]>,
    /// For each index key column `0..n_idx_cols`, `Some(tbl_col_idx)` or `None` (if IPK).
    idx_to_tbl_col: Box<[Option<u16>]>,
    /// For each index key column: whether the table column has REAL affinity (an integer found
    /// in the index entry is then a real, see [`FastTableDesc::real_cols`]).
    idx_col_is_real: Box<[bool]>,
    /// Whether any entry of `idx_col_is_real` is set.
    has_real_col: bool,
    /// Number of index record fields to parse per cell (`0` if neither cols nor rowid needed).
    parse_prefix_len: usize,
    needs_rowid: bool,
}

#[derive(Debug, Clone)]
enum FastSingleAccess {
    TableFullScan,
    TableRowidSeek {
        key: FastScalarRhs,
    },
    TableRowidRange {
        lower: Option<FastRangeBound>,
        upper: Option<FastRangeBound>,
    },
    CoveringIndexFullScan {
        cov: FastCoveringIndex,
    },
    CoveringIndexRangeScan {
        cov: FastCoveringIndex,
        prefix_keys: Box<[FastScalarRhs]>,
        prefix_affinities: Box<[TypeAffinity]>,
        range_aff: TypeAffinity,
        lower: Option<FastRangeBound>,
        upper: Option<FastRangeBound>,
    },
    /// Non-covering full index scan: visits table rows in index order by looking each index
    /// entry's trailing rowid (record field `rid_field_pos`) up in the table B+Tree.
    IndexOrderedTableLookup {
        index_root: Pgno,
        rid_field_pos: usize,
    },
}

#[inline]
fn apply_index_seek_affinity(aff: TypeAffinity, raw: Value) -> Value {
    match aff {
        TypeAffinity::Blob => raw,
        TypeAffinity::Text => TypeAffinity::Text.apply(raw),
        TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real => {
            if matches!(raw, Value::Text(_)) {
                TypeAffinity::Numeric.apply(raw)
            } else {
                raw
            }
        }
    }
}

#[inline]
fn index_has_custom_collation(index: &IndexSchema) -> bool {
    index
        .columns
        .iter()
        .any(|ic| ic.sort_order.custom_collation.is_some())
}

fn build_covering_index_desc(
    tbl: &TableSchema,
    index: &IndexSchema,
    td: &FastTableDesc,
    needs_rowid: bool,
) -> Option<FastCoveringIndex> {
    if tbl.without_rowid || index_has_custom_collation(index) {
        return None;
    }
    let n_idx = index.columns.len();
    let mut idx_to_tbl = Vec::with_capacity(n_idx);
    for ic in &index.columns {
        if ic.expr.is_some() || ic.sort_order.custom_collation.is_some() {
            return None;
        }
        let ci = ic.col_idx?;
        if ci > u16::MAX as usize {
            return None;
        }
        if tbl.rowid_alias_col == Some(ci) || !td.needs_record_parse || ci > td.max_record_col {
            idx_to_tbl.push(None);
        } else {
            idx_to_tbl.push(Some(ci as u16));
        }
    }

    let parse_prefix_len = if needs_rowid {
        n_idx + 1
    } else if !td.needs_record_parse {
        0
    } else {
        let mut max_i = 0usize;
        for (i, mapped) in idx_to_tbl.iter().enumerate() {
            if let Some(tc) = *mapped
                && (tc as usize) <= td.max_record_col
            {
                max_i = i + 1;
            }
        }
        max_i
    };

    let idx_col_is_real: Vec<bool> = index
        .columns
        .iter()
        .map(|ic| {
            ic.col_idx.is_some_and(|ci| {
                tbl.rowid_alias_col != Some(ci)
                    && tbl
                        .columns
                        .get(ci)
                        .is_some_and(|c| c.affinity == TypeAffinity::Real)
            })
        })
        .collect();
    Some(FastCoveringIndex {
        root_pgno: index.root_pgno,
        orders: index.key_sort_orders().into_boxed_slice(),
        idx_to_tbl_col: idx_to_tbl.into_boxed_slice(),
        has_real_col: idx_col_is_real.contains(&true),
        idx_col_is_real: idx_col_is_real.into_boxed_slice(),
        parse_prefix_len,
        needs_rowid,
    })
}

fn check_index_covers_all_exprs(
    tbl: &TableSchema,
    index: &IndexSchema,
    used_cols: &[bool],
) -> bool {
    if tbl.without_rowid || index_has_custom_collation(index) {
        return false;
    }
    for (col_i, &used) in used_cols.iter().enumerate() {
        if !used || tbl.rowid_alias_col == Some(col_i) {
            continue;
        }
        if !index
            .columns
            .iter()
            .any(|ic| ic.expr.is_none() && ic.col_idx == Some(col_i))
        {
            return false;
        }
    }
    true
}

#[inline]
fn decode_covering_cell<'a>(
    cov: &FastCoveringIndex,
    cell: &'a [u8],
    idx_scratch: &mut [RawField<'a>],
    tbl_cols: &mut [RawField<'a>],
) -> Result<i64> {
    if cov.parse_prefix_len == 0 {
        return Ok(0);
    }
    let parsed = parse_raw_record_prefix(cell, &mut idx_scratch[..cov.parse_prefix_len])?;
    for (i, mapped) in cov
        .idx_to_tbl_col
        .iter()
        .enumerate()
        .take(cov.parse_prefix_len.min(cov.idx_to_tbl_col.len()))
    {
        if let Some(tc) = *mapped
            && (tc as usize) < tbl_cols.len()
        {
            tbl_cols[tc as usize] = if i < parsed {
                match idx_scratch[i] {
                    RawField::Int(iv) if cov.has_real_col && cov.idx_col_is_real[i] => {
                        RawField::Float(iv as f64)
                    }
                    other => other,
                }
            } else {
                RawField::Null
            };
        }
    }
    if cov.needs_rowid {
        let rid_slot = cov.idx_to_tbl_col.len();
        if rid_slot < parsed
            && let RawField::Int(rid) = idx_scratch[rid_slot]
        {
            return Ok(rid);
        }
    }
    Ok(0)
}

/// Decodes a covering-index `cell` into `col_buf_len` table column slots (on the stack when
/// they fit) and hands `(rowid, columns)` to `f`.
#[inline(always)]
fn with_decoded_covering_cell<'a, R>(
    cov: &FastCoveringIndex,
    col_buf_len: usize,
    cell: &'a [u8],
    f: impl FnOnce(i64, &[RawField<'a>]) -> Result<R>,
) -> Result<R> {
    if cov.parse_prefix_len <= 16 && col_buf_len <= 16 {
        let mut idx_local: [RawField<'a>; 16] = [RawField::Null; 16];
        let mut tbl_local: [RawField<'a>; 16] = [RawField::Null; 16];
        let rid = decode_covering_cell(
            cov,
            cell,
            &mut idx_local[..cov.parse_prefix_len],
            &mut tbl_local[..col_buf_len],
        )?;
        f(rid, &tbl_local[..col_buf_len])
    } else {
        let mut idx_heap = vec![RawField::Null; cov.parse_prefix_len];
        let mut tbl_heap = vec![RawField::Null; col_buf_len];
        let rid = decode_covering_cell(cov, cell, &mut idx_heap, &mut tbl_heap)?;
        f(rid, &tbl_heap)
    }
}

/// Resolves the seek keys of a [`FastSingleAccess::CoveringIndexRangeScan`] against `params`
/// (applying the index column affinities) and visits the raw index cells of the range.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn seek_covering_index_range<F>(
    bt: &mut Btree,
    cov: &FastCoveringIndex,
    prefix_keys: &[FastScalarRhs],
    prefix_affinities: &[TypeAffinity],
    range_aff: TypeAffinity,
    lower: Option<&FastRangeBound>,
    upper: Option<&FastRangeBound>,
    params: &[Value],
    visit_cell: F,
) -> Result<()>
where
    F: FnMut(&[u8]) -> Result<ControlFlow<()>>,
{
    let mut prefix_vals: Vec<Value> = Vec::with_capacity(prefix_keys.len());
    for (k, &aff) in prefix_keys.iter().zip(prefix_affinities.iter()) {
        let coerced = apply_index_seek_affinity(aff, k.resolve(params).clone());
        if coerced.is_null() {
            return Ok(());
        }
        prefix_vals.push(coerced);
    }
    let lo_val: Option<(Value, bool)> = if let Some(lb) = lower {
        let coerced = apply_index_seek_affinity(range_aff, lb.rhs.resolve(params).clone());
        if coerced.is_null() {
            return Ok(());
        }
        Some((coerced, lb.inclusive))
    } else {
        None
    };
    let hi_val: Option<(Value, bool)> = if let Some(ub) = upper {
        let coerced = apply_index_seek_affinity(range_aff, ub.rhs.resolve(params).clone());
        if coerced.is_null() {
            return Ok(());
        }
        Some((coerced, ub.inclusive))
    } else {
        None
    };
    let lo_ref = lo_val.as_ref().map(|(v, inc)| (v, *inc));
    let hi_ref = hi_val.as_ref().map(|(v, inc)| (v, *inc));
    bt.seek_index_range_raw_cells(
        cov.root_pgno,
        &prefix_vals,
        lo_ref,
        hi_ref,
        &cov.orders,
        false,
        visit_cell,
    )
}

fn scan_single_table_rows<F>(
    bt: &mut Btree,
    td: &FastTableDesc,
    access: &FastSingleAccess,
    params: &[Value],
    mut visit: F,
) -> Result<()>
where
    F: FnMut(i64, &[RawField<'_>]) -> Result<ControlFlow<()>>,
{
    let col_buf_len = if td.needs_record_parse {
        td.max_record_col + 1
    } else {
        0
    };
    let mut tbl_cols: Vec<RawField<'_>> = vec![RawField::Null; col_buf_len];

    match access {
        FastSingleAccess::TableFullScan => {
            bt.scan_table_range_raw_cells(td.root_pgno, None, None, |rowid, payload| {
                // Short-lived borrow scoped strictly to this closure invocation:
                let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                if col_buf_len <= 16 {
                    td.fill_from_table_payload(payload, &mut local_cols[..col_buf_len])?;
                    visit(rowid, &local_cols[..col_buf_len])
                } else {
                    let mut heap_cols = vec![RawField::Null; col_buf_len];
                    td.fill_from_table_payload(payload, &mut heap_cols)?;
                    visit(rowid, &heap_cols)
                }
            })
        }
        FastSingleAccess::TableRowidSeek { key } => {
            let kv = key.resolve(params).clone();
            let Some(rid) = eval_rowid_target_int(kv) else {
                return Ok(());
            };
            let _ = bt.with_table_rowid_raw_cell(td.root_pgno, rid, |payload| {
                let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                if col_buf_len <= 16 {
                    td.fill_from_table_payload(payload, &mut local_cols[..col_buf_len])?;
                    let _ = visit(rid, &local_cols[..col_buf_len])?;
                } else {
                    let mut heap_cols = vec![RawField::Null; col_buf_len];
                    td.fill_from_table_payload(payload, &mut heap_cols)?;
                    let _ = visit(rid, &heap_cols)?;
                }
                Ok(())
            })?;
            Ok(())
        }
        FastSingleAccess::TableRowidRange { lower, upper } => {
            let min_rid = if let Some(lb) = lower {
                let Some(b) =
                    eval_rowid_range_bound(lb.rhs.resolve(params).clone(), lb.inclusive, true)
                else {
                    return Ok(());
                };
                b
            } else {
                None
            };
            let max_rid = if let Some(ub) = upper {
                let Some(b) =
                    eval_rowid_range_bound(ub.rhs.resolve(params).clone(), ub.inclusive, false)
                else {
                    return Ok(());
                };
                b
            } else {
                None
            };
            bt.scan_table_range_raw_cells(td.root_pgno, min_rid, max_rid, |rowid, payload| {
                let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                if col_buf_len <= 16 {
                    td.fill_from_table_payload(payload, &mut local_cols[..col_buf_len])?;
                    visit(rowid, &local_cols[..col_buf_len])
                } else {
                    let mut heap_cols = vec![RawField::Null; col_buf_len];
                    td.fill_from_table_payload(payload, &mut heap_cols)?;
                    visit(rowid, &heap_cols)
                }
            })
        }
        FastSingleAccess::CoveringIndexFullScan { cov } => {
            let _ = &mut tbl_cols;
            bt.scan_index_raw_cells(cov.root_pgno, false, |cell| {
                let mut idx_local: [RawField<'_>; 16] = [RawField::Null; 16];
                let mut tbl_local: [RawField<'_>; 16] = [RawField::Null; 16];
                if cov.parse_prefix_len <= 16 && col_buf_len <= 16 {
                    let rid = decode_covering_cell(
                        cov,
                        cell,
                        &mut idx_local[..cov.parse_prefix_len],
                        &mut tbl_local[..col_buf_len],
                    )?;
                    visit(rid, &tbl_local[..col_buf_len])
                } else {
                    let mut idx_heap = vec![RawField::Null; cov.parse_prefix_len];
                    let mut tbl_heap = vec![RawField::Null; col_buf_len];
                    let rid = decode_covering_cell(cov, cell, &mut idx_heap, &mut tbl_heap)?;
                    visit(rid, &tbl_heap)
                }
            })
        }
        FastSingleAccess::CoveringIndexRangeScan {
            cov,
            prefix_keys,
            prefix_affinities,
            range_aff,
            lower,
            upper,
        } => seek_covering_index_range(
            bt,
            cov,
            prefix_keys,
            prefix_affinities,
            *range_aff,
            lower.as_ref(),
            upper.as_ref(),
            params,
            |cell| {
                let mut idx_local: [RawField<'_>; 16] = [RawField::Null; 16];
                let mut tbl_local: [RawField<'_>; 16] = [RawField::Null; 16];
                if cov.parse_prefix_len <= 16 && col_buf_len <= 16 {
                    let rid = decode_covering_cell(
                        cov,
                        cell,
                        &mut idx_local[..cov.parse_prefix_len],
                        &mut tbl_local[..col_buf_len],
                    )?;
                    visit(rid, &tbl_local[..col_buf_len])
                } else {
                    let mut idx_heap = vec![RawField::Null; cov.parse_prefix_len];
                    let mut tbl_heap = vec![RawField::Null; col_buf_len];
                    let rid = decode_covering_cell(cov, cell, &mut idx_heap, &mut tbl_heap)?;
                    visit(rid, &tbl_heap)
                }
            },
        ),
        FastSingleAccess::IndexOrderedTableLookup {
            index_root,
            rid_field_pos,
        } => scan_index_ordered_table_rows(bt, td, *index_root, *rid_field_pos, &mut visit),
    }
}

/// Type-erased per-row callback of [`scan_single_table_rows`]: `(rowid, table columns)`.
type RowVisitor<'v> = dyn FnMut(i64, &[RawField<'_>]) -> Result<ControlFlow<()>> + 'v;

/// Non-covering full index scan: collects the rowids in index order, then visits each table row.
/// Kept out of line (and behind `dyn`) so the hot scan arms above keep their codegen.
#[inline(never)]
fn scan_index_ordered_table_rows(
    bt: &mut Btree,
    td: &FastTableDesc,
    index_root: Pgno,
    rid_pos: usize,
    visit: &mut RowVisitor<'_>,
) -> Result<()> {
    let col_buf_len = if td.needs_record_parse {
        td.max_record_col + 1
    } else {
        0
    };
    let mut rowids: Vec<i64> = Vec::new();
    bt.scan_index_raw_cells(index_root, false, |cell| {
        let mut idx_local: [RawField<'_>; 16] = [RawField::Null; 16];
        let mut idx_heap: Vec<RawField<'_>>;
        let fields: &mut [RawField<'_>] = if rid_pos < 16 {
            &mut idx_local[..rid_pos + 1]
        } else {
            idx_heap = vec![RawField::Null; rid_pos + 1];
            &mut idx_heap
        };
        let n = parse_raw_record_prefix(cell, fields)?;
        if rid_pos < n
            && let RawField::Int(rid) = fields[rid_pos]
        {
            rowids.push(rid);
        }
        Ok(ControlFlow::Continue(()))
    })?;
    for rid in rowids {
        let flow = bt.with_table_rowid_raw_cell(td.root_pgno, rid, |payload| {
            let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
            if col_buf_len <= 16 {
                td.fill_from_table_payload(payload, &mut local_cols[..col_buf_len])?;
                visit(rid, &local_cols[..col_buf_len])
            } else {
                let mut heap_cols = vec![RawField::Null; col_buf_len];
                td.fill_from_table_payload(payload, &mut heap_cols)?;
                visit(rid, &heap_cols)
            }
        })?;
        if matches!(flow, Some(ControlFlow::Break(()))) {
            break;
        }
    }
    Ok(())
}

// ============================================================================
// Aggregate & Group-By Key Helpers
// ============================================================================

fn compile_agg_call(expr: &Expr, scope: &FastCompileScope<'_>) -> Option<FastAggSpec> {
    let Expr::FunctionCall {
        name,
        distinct: false,
        args,
        filter: None,
        over: None,
    } = expr
    else {
        return None;
    };
    let lower = name.to_ascii_lowercase();
    match (lower.as_str(), args) {
        ("count", FunctionArgs::Star) => {
            if !scope.builtin_not_overridden("count", 0) {
                return None;
            }
            Some(FastAggSpec::CountStar)
        }
        ("count", FunctionArgs::Exprs(es)) if es.len() == 1 => {
            if !scope.builtin_not_overridden("count", 1) || expr_contains_aggregate(&es[0]) {
                return None;
            }
            Some(FastAggSpec::Count(compile_fast_expr(&es[0], scope)?))
        }
        ("sum", FunctionArgs::Exprs(es)) if es.len() == 1 => {
            if !scope.builtin_not_overridden("sum", 1) || expr_contains_aggregate(&es[0]) {
                return None;
            }
            Some(FastAggSpec::Sum(compile_fast_expr(&es[0], scope)?))
        }
        ("total", FunctionArgs::Exprs(es)) if es.len() == 1 => {
            if !scope.builtin_not_overridden("total", 1) || expr_contains_aggregate(&es[0]) {
                return None;
            }
            Some(FastAggSpec::Total(compile_fast_expr(&es[0], scope)?))
        }
        ("avg", FunctionArgs::Exprs(es)) if es.len() == 1 => {
            if !scope.builtin_not_overridden("avg", 1) || expr_contains_aggregate(&es[0]) {
                return None;
            }
            Some(FastAggSpec::Avg(compile_fast_expr(&es[0], scope)?))
        }
        ("min" | "max", FunctionArgs::Exprs(es)) if es.len() == 1 => {
            if !scope.builtin_not_overridden(&lower, 1) || expr_contains_aggregate(&es[0]) {
                return None;
            }
            let (_, coll_opt) = scope.expr_meta(&es[0]);
            if matches!(&es[0], Expr::Collate { .. }) && coll_opt.is_none() {
                return None;
            }
            let coll = coll_opt.unwrap_or(Collation::Binary);
            let fe = compile_fast_expr(&es[0], scope)?;
            if lower == "min" {
                Some(FastAggSpec::Min(fe, coll))
            } else {
                Some(FastAggSpec::Max(fe, coll))
            }
        }
        ("json_group_array", FunctionArgs::Exprs(es)) if es.len() == 1 => {
            if !scope.builtin_not_overridden("json_group_array", 1)
                || expr_contains_aggregate(&es[0])
            {
                return None;
            }
            Some(FastAggSpec::JsonGroupArray(compile_fast_expr(
                &es[0], scope,
            )?))
        }
        _ => None,
    }
}

fn compile_group_proj_expr(
    expr: &Expr,
    group_by_cols: &[FastResolvedCol],
    scope: &FastCompileScope<'_>,
    aggs: &mut Vec<FastAggSpec>,
) -> Option<FastGroupProjExpr> {
    if let Some(v) = eval_const_literal_expr(expr) {
        return Some(FastGroupProjExpr::Const(v));
    }
    if let Expr::Column(cref) = expr {
        let rc = scope.resolve_col_ref(cref)?;
        let pos = group_by_cols.iter().position(|g| g.src == rc.src)?;
        return Some(FastGroupProjExpr::GroupKey(pos));
    }
    if let Some(spec) = compile_agg_call(expr, scope) {
        let slot = aggs.len();
        aggs.push(spec);
        return Some(FastGroupProjExpr::AggSlot(slot));
    }
    if let Expr::FunctionCall {
        name,
        distinct: false,
        args: FunctionArgs::Exprs(es),
        filter: None,
        over: None,
    } = expr
        && (name.eq_ignore_ascii_case("coalesce") || name.eq_ignore_ascii_case("ifnull"))
        && es.len() == 2
        && scope.builtin_not_overridden(&name.to_ascii_lowercase(), 2)
        && let Some(spec) = compile_agg_call(&es[0], scope)
        && let Some(fallback) = eval_const_literal_expr(&es[1])
    {
        let slot = aggs.len();
        aggs.push(spec);
        return Some(FastGroupProjExpr::CoalesceAggConst { slot, fallback });
    }
    None
}

// ============================================================================
// Plan 1: Ungrouped Streaming Aggregate (`W04`, `W09`, `W14` Q1, `COUNT(*)`)
// ============================================================================

#[derive(Debug, Clone)]
struct UngroupedAggPlan {
    table: FastTableDesc,
    access: FastSingleAccess,
    where_pred: Option<FastExpr>,
    aggs: Box<[FastAggSpec]>,
    projections: Box<[FastGroupProjExpr]>,
    /// Integer row kernel for covering-index accesses (`None` when the plan shape has none).
    int_kernel: Option<CovIntKernelShape>,
}

impl UngroupedAggPlan {
    fn execute(&self, bt: &mut Btree, params: &[Value], enc: TextEncoding) -> Result<Vec<Value>> {
        let mut states: Vec<FastAggState> = self
            .aggs
            .iter()
            .map(|s| FastAggState::from_spec(s, enc))
            .collect();
        if let Some(kernel) = self.int_kernel.as_ref().and_then(|k| k.bind(params)) {
            self.scan_covering_with_kernel(bt, params, enc, &kernel, &mut states)?;
        } else {
            scan_single_table_rows(bt, &self.table, &self.access, params, |rowid, cols| {
                let ctx = FastRowCtx::single(rowid, cols, params, enc);
                if let Some(ref pred) = self.where_pred
                    && !pred.eval_where_true(&ctx)?
                {
                    return Ok(ControlFlow::Continue(()));
                }
                for (st, spec) in states.iter_mut().zip(self.aggs.iter()) {
                    st.step(spec, &ctx)?;
                }
                Ok(ControlFlow::Continue(()))
            })?;
        }

        let mut agg_vals = Vec::with_capacity(self.aggs.len());
        for (st, spec) in states.iter().zip(self.aggs.iter()) {
            agg_vals.push(st.finalize(spec)?);
        }
        let mut out = Vec::with_capacity(self.projections.len());
        for p in self.projections.iter() {
            out.push(p.eval(&[], &agg_vals));
        }
        Ok(out)
    }

    /// Covering-index scan with the integer row kernel: cells the kernel decodes are filtered
    /// and aggregated as plain integers, every other cell takes the general row code of
    /// [`UngroupedAggPlan::execute`].
    fn scan_covering_with_kernel(
        &self,
        bt: &mut Btree,
        params: &[Value],
        enc: TextEncoding,
        kernel: &CovIntKernel<'_>,
        states: &mut [FastAggState],
    ) -> Result<()> {
        let col_buf_len = self.table.col_buf_len();
        let (FastSingleAccess::CoveringIndexFullScan { cov }
        | FastSingleAccess::CoveringIndexRangeScan { cov, .. }) = &self.access
        else {
            return Err(SqliteError::new(
                ErrorCode::Internal,
                "integer kernel bound to a non-covering access",
            ));
        };
        let visit_cell = |cell: &[u8]| -> Result<ControlFlow<()>> {
            let mut row: IntRow = [0; INT_RECORD_MAX_SLOTS];
            if kernel.decode(cell, &mut row) {
                note_int_kernel_row(true);
                if kernel.passes(&row) {
                    kernel.step(states, &row, enc);
                }
                return Ok(ControlFlow::Continue(()));
            }
            note_int_kernel_row(false);
            with_decoded_covering_cell(cov, col_buf_len, cell, |rowid, cols| {
                let ctx = FastRowCtx::single(rowid, cols, params, enc);
                if let Some(ref pred) = self.where_pred
                    && !pred.eval_where_true(&ctx)?
                {
                    return Ok(ControlFlow::Continue(()));
                }
                for (st, spec) in states.iter_mut().zip(self.aggs.iter()) {
                    st.step(spec, &ctx)?;
                }
                Ok(ControlFlow::Continue(()))
            })
        };
        match &self.access {
            FastSingleAccess::CoveringIndexRangeScan {
                cov,
                prefix_keys,
                prefix_affinities,
                range_aff,
                lower,
                upper,
            } => seek_covering_index_range(
                bt,
                cov,
                prefix_keys,
                prefix_affinities,
                *range_aff,
                lower.as_ref(),
                upper.as_ref(),
                params,
                visit_cell,
            ),
            _ => bt.scan_index_raw_cells(cov.root_pgno, false, visit_cell),
        }
    }
}

// ============================================================================
// Plan 2: Ordered Covering-Index Streaming `GROUP BY` (`W05`)
// ============================================================================

#[derive(Debug, Clone)]
struct OrderedCoveringGroupByPlan {
    table: FastTableDesc,
    cov: FastCoveringIndex,
    reverse: bool,
    group_col: FastColSrc,
    group_collation: Collation,
    where_pred: Option<FastExpr>,
    aggs: Box<[FastAggSpec]>,
    projections: Box<[FastGroupProjExpr]>,
    /// Integer row kernel (`None` when the plan shape has none).
    int_kernel: Option<CovIntKernelShape>,
}

impl OrderedCoveringGroupByPlan {
    fn execute(
        &self,
        bt: &mut Btree,
        params: &[Value],
        enc: TextEncoding,
    ) -> Result<Vec<Vec<Value>>> {
        let col_buf_len = if self.table.needs_record_parse {
            self.table.max_record_col + 1
        } else {
            0
        };
        let mut states: Vec<FastAggState> = self
            .aggs
            .iter()
            .map(|s| FastAggState::from_spec(s, enc))
            .collect();
        let mut cur_group_key: Option<Value> = None;
        let mut out_rows: Vec<Vec<Value>> = Vec::with_capacity(64);

        let flush_group = |key: Value,
                           states: &mut [FastAggState],
                           out_rows: &mut Vec<Vec<Value>>|
         -> Result<()> {
            let mut agg_vals = Vec::with_capacity(self.aggs.len());
            for (st, spec) in states.iter().zip(self.aggs.iter()) {
                agg_vals.push(st.finalize(spec)?);
            }
            let key_arr = [key];
            let mut row = Vec::with_capacity(self.projections.len());
            for p in self.projections.iter() {
                row.push(p.eval(&key_arr, &agg_vals));
            }
            out_rows.push(row);
            for (st, spec) in states.iter_mut().zip(self.aggs.iter()) {
                st.reset(spec, enc);
            }
            Ok(())
        };

        let kernel = self.int_kernel.as_ref().and_then(|k| k.bind(params));

        bt.scan_index_raw_cells(self.cov.root_pgno, self.reverse, |cell| {
            // Integer rows: same filter / group-break / aggregate steps as the general code
            // below, on plain integers.
            if let Some(ref kernel) = kernel {
                let mut row: IntRow = [0; INT_RECORD_MAX_SLOTS];
                if kernel.decode(cell, &mut row) {
                    note_int_kernel_row(true);
                    if !kernel.passes(&row) {
                        return Ok(ControlFlow::Continue(()));
                    }
                    let gk = kernel.group_key(&row);
                    let same_group = match cur_group_key.as_ref() {
                        None | Some(Value::Null) => false,
                        Some(Value::Integer(prev)) => *prev == gk,
                        Some(prev) => {
                            let spec = key_order_for_collation(self.group_collation);
                            RawField::Int(gk)
                                .compare_sql_to_nonnull_value(prev, &spec, enc)
                                .unwrap_or(Ordering::Equal)
                                == Ordering::Equal
                        }
                    };
                    if !same_group {
                        if let Some(prev_key) = cur_group_key.take() {
                            flush_group(prev_key, &mut states, &mut out_rows)?;
                        }
                        cur_group_key = Some(Value::Integer(gk));
                    }
                    kernel.step(&mut states, &row, enc);
                    return Ok(ControlFlow::Continue(()));
                }
                note_int_kernel_row(false);
            }
            let mut idx_local: [RawField<'_>; 16] = [RawField::Null; 16];
            let mut tbl_local: [RawField<'_>; 16] = [RawField::Null; 16];
            let mut idx_heap: Vec<RawField<'_>>;
            let mut tbl_heap: Vec<RawField<'_>>;
            let (rid, tbl_slice): (i64, &[RawField<'_>]) =
                if self.cov.parse_prefix_len <= 16 && col_buf_len <= 16 {
                    let r = decode_covering_cell(
                        &self.cov,
                        cell,
                        &mut idx_local[..self.cov.parse_prefix_len],
                        &mut tbl_local[..col_buf_len],
                    )?;
                    (r, &tbl_local[..col_buf_len])
                } else {
                    idx_heap = vec![RawField::Null; self.cov.parse_prefix_len];
                    tbl_heap = vec![RawField::Null; col_buf_len];
                    let r = decode_covering_cell(&self.cov, cell, &mut idx_heap, &mut tbl_heap)?;
                    (r, &tbl_heap)
                };
            let ctx = FastRowCtx::single(rid, tbl_slice, params, enc);
            if let Some(ref pred) = self.where_pred
                && !pred.eval_where_true(&ctx)?
            {
                return Ok(ControlFlow::Continue(()));
            }
            let raw_gk = ctx.get_col(self.group_col);
            let same_group = match cur_group_key.as_ref() {
                None => false,
                Some(prev) => match (raw_gk, prev) {
                    (RawField::Null, Value::Null) => true,
                    (RawField::Null, _) | (_, Value::Null) => false,
                    (rf, pv) => {
                        let spec = key_order_for_collation(self.group_collation);
                        rf.compare_sql_to_nonnull_value(pv, &spec, enc)
                            .unwrap_or(Ordering::Equal)
                            == Ordering::Equal
                    }
                },
            };
            if !same_group {
                if let Some(prev_key) = cur_group_key.take() {
                    flush_group(prev_key, &mut states, &mut out_rows)?;
                }
                cur_group_key = Some(raw_gk.to_owned_value(enc));
            }
            for (st, spec) in states.iter_mut().zip(self.aggs.iter()) {
                st.step(spec, &ctx)?;
            }
            Ok(ControlFlow::Continue(()))
        })?;

        if let Some(last_key) = cur_group_key.take() {
            flush_group(last_key, &mut states, &mut out_rows)?;
        }
        Ok(out_rows)
    }
}

// ============================================================================
// Plan 3: Selective Table-Leaf Dense/Hash `GROUP BY` (`W10` Q1, `W14` Q3)
// ============================================================================

#[derive(Debug, Clone)]
struct GroupSortSpec {
    proj_or_key: FastGroupProjExpr,
    desc: bool,
    nulls_last: bool,
    collation: Collation,
}

#[derive(Debug, Clone)]
struct HashGroupByPlan {
    table: FastTableDesc,
    access: FastSingleAccess,
    where_pred: Option<FastExpr>,
    group_col: FastColSrc,
    group_collation: Collation,
    /// The groups come out in descending key order when there is no ORDER BY: C walks an
    /// index that leads with the group column in DESC order instead of sorting.
    groups_desc: bool,
    aggs: Box<[FastAggSpec]>,
    projections: Box<[FastGroupProjExpr]>,
    order_by: Box<[GroupSortSpec]>,
}

struct HashGroupBucketKey {
    val: Value,
    collation: Collation,
    enc: TextEncoding,
}

impl PartialEq for HashGroupBucketKey {
    fn eq(&self, other: &Self) -> bool {
        self.val
            .cmp_sqlite_with_encoding(&other.val, self.collation, self.enc)
            == Ordering::Equal
    }
}
impl Eq for HashGroupBucketKey {}

impl std::hash::Hash for HashGroupBucketKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match &self.val {
            Value::Null => state.write_u64(0),
            Value::Integer(i) => {
                state.write_u64(1);
                state.write_i64(*i);
            }
            Value::Real(r) => {
                if *r >= (i64::MIN as f64) && *r < (i64::MAX as f64) {
                    let i = *r as i64;
                    if (i as f64) == *r {
                        state.write_u64(1);
                        state.write_i64(i);
                        return;
                    }
                }
                state.write_u64(2);
                let bits = if *r == 0.0 { 0u64 } else { r.to_bits() };
                state.write_u64(bits);
            }
            Value::Text(s) => {
                state.write_u64(3);
                match self.collation {
                    Collation::Binary => state.write(s.as_bytes()),
                    Collation::RTrim => state.write(trim_ascii_spaces_end(s.as_bytes())),
                    Collation::NoCase => {
                        for &b in s.as_bytes() {
                            state.write_u64(u64::from(b.to_ascii_lowercase()));
                        }
                    }
                }
            }
            Value::Blob(b) => {
                state.write_u64(4);
                state.write(b);
            }
        }
    }
}

impl HashGroupByPlan {
    fn execute(
        &self,
        bt: &mut Btree,
        params: &[Value],
        enc: TextEncoding,
    ) -> Result<Vec<Vec<Value>>> {
        let mut dense_slots: [u32; 256] = [u32::MAX; 256];
        let mut map: HashMap<HashGroupBucketKey, u32, FastBuildHasher> = HashMap::default();
        let mut groups: Vec<(Value, Vec<FastAggState>)> = Vec::with_capacity(32);

        scan_single_table_rows(bt, &self.table, &self.access, params, |rowid, cols| {
            let ctx = FastRowCtx::single(rowid, cols, params, enc);
            if let Some(ref pred) = self.where_pred
                && !pred.eval_where_true(&ctx)?
            {
                return Ok(ControlFlow::Continue(()));
            }
            let gk_raw = ctx.get_col(self.group_col);
            // Small integral keys take the dense table; an integral REAL (`1.0`) must land in the
            // same group as the equal INTEGER (`1`), exactly like the hashed path below.
            let dense_key: Option<usize> = match gk_raw {
                RawField::Int(iv) if (0..256).contains(&iv) => Some(iv as usize),
                RawField::Float(fv) if (0.0..256.0).contains(&fv) && fv.fract() == 0.0 => {
                    Some(fv as usize)
                }
                _ => None,
            };
            let grp_idx: usize = if let Some(dk) = dense_key {
                let slot = &mut dense_slots[dk];
                if *slot == u32::MAX {
                    let idx = groups.len();
                    let init_states = self
                        .aggs
                        .iter()
                        .map(|s| FastAggState::from_spec(s, enc))
                        .collect();
                    groups.push((gk_raw.to_owned_value(enc), init_states));
                    *slot = idx as u32;
                    idx
                } else {
                    *slot as usize
                }
            } else {
                let owned = gk_raw.to_owned_value(enc);
                let bkey = HashGroupBucketKey {
                    val: owned.clone(),
                    collation: self.group_collation,
                    enc,
                };
                if let Some(&idx) = map.get(&bkey) {
                    idx as usize
                } else {
                    let idx = groups.len();
                    let init_states = self
                        .aggs
                        .iter()
                        .map(|s| FastAggState::from_spec(s, enc))
                        .collect();
                    groups.push((owned, init_states));
                    map.insert(bkey, idx as u32);
                    idx
                }
            };

            let states = &mut groups[grp_idx].1;
            for (st, spec) in states.iter_mut().zip(self.aggs.iter()) {
                st.step(spec, &ctx)?;
            }
            Ok(ControlFlow::Continue(()))
        })?;

        if groups.is_empty() {
            return Ok(Vec::new());
        }

        let mut finalized: Vec<(Value, Vec<Value>)> = Vec::with_capacity(groups.len());
        for (gk, states) in groups {
            let mut agg_vals = Vec::with_capacity(self.aggs.len());
            for (st, spec) in states.iter().zip(self.aggs.iter()) {
                agg_vals.push(st.finalize(spec)?);
            }
            finalized.push((gk, agg_vals));
        }

        if self.order_by.is_empty() && finalized.len() > 1 {
            // C groups through a sorter (or an index walk), so without ORDER BY the groups
            // come out in ascending key order; the hash table above collected them in
            // first-seen order. The sort is stable: the group keeps its first-seen key value.
            let coll = self.group_collation;
            finalized.sort_by(|(k_a, _), (k_b, _)| k_a.cmp_sqlite(k_b, coll));
            if self.groups_desc {
                finalized.reverse();
            }
        }
        if !self.order_by.is_empty() && finalized.len() > 1 {
            fallible_sort_by(&mut finalized, |(k_a, a_a), (k_b, a_b)| {
                let ka_arr = [k_a.clone()];
                let kb_arr = [k_b.clone()];
                for ob in self.order_by.iter() {
                    let va = ob.proj_or_key.eval(&ka_arr, a_a);
                    let vb = ob.proj_or_key.eval(&kb_arr, a_b);
                    let ord = compare_order_by_values(
                        &va,
                        &vb,
                        ob.desc,
                        ob.nulls_last,
                        ob.collation,
                        enc,
                    );
                    if ord != Ordering::Equal {
                        return Ok(ord);
                    }
                }
                Ok(Ordering::Equal)
            })?;
        }

        let mut out_rows = Vec::with_capacity(finalized.len());
        for (gk, agg_vals) in finalized {
            let key_arr = [gk];
            let mut row = Vec::with_capacity(self.projections.len());
            for p in self.projections.iter() {
                row.push(p.eval(&key_arr, &agg_vals));
            }
            out_rows.push(row);
        }
        Ok(out_rows)
    }
}

#[inline]
fn compare_order_by_values(
    a: &Value,
    b: &Value,
    desc: bool,
    nulls_last: bool,
    collation: Collation,
    enc: TextEncoding,
) -> Ordering {
    match (a.is_null(), b.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => {
            if nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, true) => {
            if nulls_last {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, false) => {
            let base = a.cmp_sqlite_with_encoding(b, collation, enc);
            if desc { base.reverse() } else { base }
        }
    }
}

// ============================================================================
// Plan 4: Ordered Index `LIMIT` Select & Outer `json_group_array` (`W10` Q2, `W14` Q2)
// ============================================================================

#[derive(Debug, Clone)]
enum OrderedIndexMode {
    /// Index order matches `ORDER BY` directly (`W10` Q2: `WHERE bucket = ?1 ... ORDER BY id LIMIT 20`).
    ExactStream {
        reverse: bool,
        outer_json_group_array: bool,
    },
    /// `WHERE <eq_prefix on idx> ORDER BY sort_col DESC, rowid_or_ipk ASC LIMIT K`
    /// where `idx` has `(eq_prefix..., sort_col ASC)` (`W14` Q2).
    ReverseTieBreakRowidAsc {
        sort_idx_pos: usize,
        sort_collation: Collation,
    },
}

#[derive(Debug, Clone)]
struct OrderedIndexSelectPlan {
    table: FastTableDesc,
    index_root: Pgno,
    index_orders: Box<[KeySortOrder]>,
    n_index_cols: usize,
    prefix_keys: Box<[FastScalarRhs]>,
    prefix_affinities: Box<[TypeAffinity]>,
    extra_where: Option<FastExpr>,
    projections: Box<[FastExpr]>,
    limit: usize,
    mode: OrderedIndexMode,
}

impl OrderedIndexSelectPlan {
    fn execute(
        &self,
        bt: &mut Btree,
        params: &[Value],
        enc: TextEncoding,
    ) -> Result<FastReadExecOut> {
        let mut prefix_vals: Vec<Value> = Vec::with_capacity(self.prefix_keys.len());
        for (k, &aff) in self.prefix_keys.iter().zip(self.prefix_affinities.iter()) {
            let coerced = apply_index_seek_affinity(aff, k.resolve(params).clone());
            if coerced.is_null() {
                return match self.mode {
                    OrderedIndexMode::ExactStream {
                        outer_json_group_array: true,
                        ..
                    } => Ok(FastReadExecOut::SingleRow(vec![Value::Text(
                        "[]".to_owned(),
                    )])),
                    _ => Ok(FastReadExecOut::Empty),
                };
            }
            prefix_vals.push(coerced);
        }

        if self.limit == 0 {
            return match self.mode {
                OrderedIndexMode::ExactStream {
                    outer_json_group_array: true,
                    ..
                } => Ok(FastReadExecOut::SingleRow(vec![Value::Text(
                    "[]".to_owned(),
                )])),
                _ => Ok(FastReadExecOut::Empty),
            };
        }

        let col_buf_len = if self.table.needs_record_parse {
            self.table.max_record_col + 1
        } else {
            0
        };
        let rid_field_pos = self.n_index_cols;

        match &self.mode {
            OrderedIndexMode::ExactStream {
                reverse,
                outer_json_group_array,
            } => {
                let stop_on_idx = self.extra_where.is_none();
                let mut candidate_rowids: Vec<i64> = Vec::with_capacity(self.limit.min(256));
                bt.seek_index_range_raw_cells(
                    self.index_root,
                    &prefix_vals,
                    None,
                    None,
                    &self.index_orders,
                    *reverse,
                    |cell| {
                        let mut idx_fields: [RawField<'_>; 16] = [RawField::Null; 16];
                        if rid_field_pos < 16 {
                            let n = parse_raw_record_prefix(
                                cell,
                                &mut idx_fields[..rid_field_pos + 1],
                            )?;
                            if rid_field_pos < n
                                && let RawField::Int(rid) = idx_fields[rid_field_pos]
                            {
                                candidate_rowids.push(rid);
                            }
                        } else {
                            let mut heap_f = vec![RawField::Null; rid_field_pos + 1];
                            let n = parse_raw_record_prefix(cell, &mut heap_f)?;
                            if rid_field_pos < n
                                && let RawField::Int(rid) = heap_f[rid_field_pos]
                            {
                                candidate_rowids.push(rid);
                            }
                        }
                        if stop_on_idx && candidate_rowids.len() >= self.limit {
                            Ok(ControlFlow::Break(()))
                        } else {
                            Ok(ControlFlow::Continue(()))
                        }
                    },
                )?;

                if *outer_json_group_array {
                    let mut json_acc =
                        AggregateAccumulator::new(AggregateKind::JsonGroupArray, false)
                            .with_encoding(enc);
                    let mut matched = 0usize;
                    for rid in candidate_rowids {
                        let step_opt =
                            bt.with_table_rowid_raw_cell(self.table.root_pgno, rid, |payload| {
                                let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                                let mut heap_cols: Vec<RawField<'_>>;
                                let cols_slice: &[RawField<'_>] = if col_buf_len <= 16 {
                                    self.table.fill_from_table_payload(
                                        payload,
                                        &mut local_cols[..col_buf_len],
                                    )?;
                                    &local_cols[..col_buf_len]
                                } else {
                                    heap_cols = vec![RawField::Null; col_buf_len];
                                    self.table
                                        .fill_from_table_payload(payload, &mut heap_cols)?;
                                    &heap_cols
                                };
                                let ctx = FastRowCtx::single(rid, cols_slice, params, enc);
                                if let Some(ref pred) = self.extra_where
                                    && !pred.eval_where_true(&ctx)?
                                {
                                    return Ok(None);
                                }
                                Ok(Some(self.projections[0].eval_runtime_value(&ctx)?))
                            })?;
                        if let Some(Some(rv)) = step_opt {
                            json_acc.step_rt(&[rv])?;
                            matched += 1;
                            if matched >= self.limit {
                                break;
                            }
                        }
                    }
                    return Ok(FastReadExecOut::SingleRow(vec![json_acc.finalize()?]));
                }

                let mut rows: Vec<Vec<Value>> =
                    Vec::with_capacity(self.limit.min(candidate_rowids.len()));
                for rid in candidate_rowids {
                    let row_opt =
                        bt.with_table_rowid_raw_cell(self.table.root_pgno, rid, |payload| {
                            let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                            let mut heap_cols: Vec<RawField<'_>>;
                            let cols_slice: &[RawField<'_>] = if col_buf_len <= 16 {
                                self.table.fill_from_table_payload(
                                    payload,
                                    &mut local_cols[..col_buf_len],
                                )?;
                                &local_cols[..col_buf_len]
                            } else {
                                heap_cols = vec![RawField::Null; col_buf_len];
                                self.table
                                    .fill_from_table_payload(payload, &mut heap_cols)?;
                                &heap_cols
                            };
                            let ctx = FastRowCtx::single(rid, cols_slice, params, enc);
                            if let Some(ref pred) = self.extra_where
                                && !pred.eval_where_true(&ctx)?
                            {
                                return Ok(None);
                            }
                            let mut vals = Vec::with_capacity(self.projections.len());
                            for p in self.projections.iter() {
                                vals.push(p.eval_value(&ctx)?);
                            }
                            Ok(Some(vals))
                        })?;
                    if let Some(Some(vals)) = row_opt {
                        rows.push(vals);
                        if rows.len() >= self.limit {
                            break;
                        }
                    }
                }
                Ok(FastReadExecOut::from_rows(rows))
            }
            OrderedIndexMode::ReverseTieBreakRowidAsc {
                sort_idx_pos,
                sort_collation,
            } => {
                // Reverse scan of `(eq_prefix..., sort_col ASC, rowid ASC)` yields `sort_col DESC, rowid DESC`.
                // Collect until `>= limit` AND `cur_sort_val < items[limit - 1].0` so ties on the K-th
                // `sort_col` value get exact `rowid ASC` ordering!
                let mut candidates: Vec<(Value, i64)> = Vec::with_capacity(self.limit + 4);
                bt.seek_index_range_raw_cells(
                    self.index_root,
                    &prefix_vals,
                    None,
                    None,
                    &self.index_orders,
                    true,
                    |cell| {
                        let mut idx_fields: [RawField<'_>; 16] = [RawField::Null; 16];
                        let n =
                            parse_raw_record_prefix(cell, &mut idx_fields[..rid_field_pos + 1])?;
                        if rid_field_pos >= n {
                            return Ok(ControlFlow::Continue(()));
                        }
                        let RawField::Int(rid) = idx_fields[rid_field_pos] else {
                            return Ok(ControlFlow::Continue(()));
                        };
                        let sv_raw = idx_fields[*sort_idx_pos];
                        if candidates.len() >= self.limit {
                            let kth = &candidates[self.limit - 1].0;
                            let strictly_smaller = match (sv_raw, kth) {
                                (RawField::Null, Value::Null) => false,
                                (RawField::Null, _) => true,
                                (_, Value::Null) => false,
                                (rf, kv) => {
                                    let spec = key_order_for_collation(*sort_collation);
                                    rf.compare_sql_to_nonnull_value(kv, &spec, enc)
                                        .unwrap_or(Ordering::Equal)
                                        == Ordering::Less
                                }
                            };
                            if strictly_smaller {
                                return Ok(ControlFlow::Break(()));
                            }
                        }
                        candidates.push((sv_raw.to_owned_value(enc), rid));
                        Ok(ControlFlow::Continue(()))
                    },
                )?;

                if candidates.is_empty() {
                    return Ok(FastReadExecOut::Empty);
                }

                candidates.sort_by(|(va, ra), (vb, rb)| {
                    let ord = compare_order_by_values(va, vb, true, true, *sort_collation, enc);
                    if ord != Ordering::Equal {
                        ord
                    } else {
                        ra.cmp(rb)
                    }
                });
                candidates.truncate(self.limit);

                let mut rows: Vec<Vec<Value>> = Vec::with_capacity(candidates.len());
                for (_, rid) in candidates {
                    let row_opt =
                        bt.with_table_rowid_raw_cell(self.table.root_pgno, rid, |payload| {
                            let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                            let mut heap_cols: Vec<RawField<'_>>;
                            let cols_slice: &[RawField<'_>] = if col_buf_len <= 16 {
                                self.table.fill_from_table_payload(
                                    payload,
                                    &mut local_cols[..col_buf_len],
                                )?;
                                &local_cols[..col_buf_len]
                            } else {
                                heap_cols = vec![RawField::Null; col_buf_len];
                                self.table
                                    .fill_from_table_payload(payload, &mut heap_cols)?;
                                &heap_cols
                            };
                            let ctx = FastRowCtx::single(rid, cols_slice, params, enc);
                            let mut vals = Vec::with_capacity(self.projections.len());
                            for p in self.projections.iter() {
                                vals.push(p.eval_value(&ctx)?);
                            }
                            Ok(vals)
                        })?;
                    if let Some(vals) = row_opt {
                        rows.push(vals);
                    }
                }
                Ok(FastReadExecOut::from_rows(rows))
            }
        }
    }
}

// ============================================================================
// Plan 5: Streaming Hash Equi-Join + `GROUP BY` (`W06`)
// ============================================================================

#[derive(Debug, Clone)]
struct EquiJoinGroupByPlan {
    /// Side `0` in `FastRowCtx` = build table (filtered small side, e.g. `t2` in `W06`).
    build_table: FastTableDesc,
    build_access: FastSingleAccess,
    build_where: Option<FastExpr>,
    build_join_key: FastColSrc,
    /// Columns of `build_table` (besides `rowid`) carried in each hash-table entry `(col_idx, ...)`.
    build_kept_cols: Box<[u16]>,
    /// Side `1` in `FastRowCtx` = probe table (streamed once, e.g. `t1` in `W06`).
    probe_table: FastTableDesc,
    probe_access: FastSingleAccess,
    probe_where: Option<FastExpr>,
    probe_join_key: FastColSrc,
    post_join_where: Option<FastExpr>,
    group_col: FastColSrc,
    group_collation: Collation,
    /// The groups come out in descending key order when there is no ORDER BY: C walks an
    /// index that leads with the group column in DESC order instead of sorting.
    groups_desc: bool,
    aggs: Box<[FastAggSpec]>,
    projections: Box<[FastGroupProjExpr]>,
    order_by: Box<[GroupSortSpec]>,
}

#[derive(Clone)]
struct BuildSideRow {
    rowid: i64,
    kept_vals: SmallBuildVals,
}

#[derive(Clone)]
enum SmallBuildVals {
    Inline([Value; 3]),
    Heap(Box<[Value]>),
}

impl SmallBuildVals {
    #[inline]
    fn as_slice(&self, len: usize) -> &[Value] {
        match self {
            Self::Inline(arr) => &arr[..len],
            Self::Heap(b) => b,
        }
    }
}

#[inline]
fn value_as_raw_field(v: &Value) -> RawField<'_> {
    match v {
        Value::Null => RawField::Null,
        Value::Integer(i) => RawField::Int(*i),
        Value::Real(r) => RawField::Float(*r),
        Value::Text(s) => RawField::Text(s.as_bytes()),
        Value::Blob(b) => RawField::Blob(b.as_slice()),
    }
}

impl EquiJoinGroupByPlan {
    fn execute(
        &self,
        bt: &mut Btree,
        params: &[Value],
        enc: TextEncoding,
    ) -> Result<Vec<Vec<Value>>> {
        // 1. Scan filtered build table into compact `ArenaMultiMap`:
        let mut build_map: ArenaMultiMap<BuildSideRow> = ArenaMultiMap::with_capacity(512, 12);
        let n_kept = self.build_kept_cols.len();

        scan_single_table_rows(
            bt,
            &self.build_table,
            &self.build_access,
            params,
            |b_rid, b_cols| {
                let b_ctx = FastRowCtx::single(b_rid, b_cols, params, enc);
                if let Some(ref pred) = self.build_where
                    && !pred.eval_where_true(&b_ctx)?
                {
                    return Ok(ControlFlow::Continue(()));
                }
                let jk = b_ctx.get_col(self.build_join_key);
                if matches!(jk, RawField::Null) {
                    return Ok(ControlFlow::Continue(()));
                }

                let kept_vals = if n_kept <= 3 {
                    let mut arr = [const { Value::Null }; 3];
                    for (slot, &ci) in self.build_kept_cols.iter().enumerate() {
                        arr[slot] = b_cols
                            .get(ci as usize)
                            .copied()
                            .unwrap_or(RawField::Null)
                            .to_owned_value(enc);
                    }
                    SmallBuildVals::Inline(arr)
                } else {
                    let mut v = Vec::with_capacity(n_kept);
                    for &ci in self.build_kept_cols.iter() {
                        v.push(
                            b_cols
                                .get(ci as usize)
                                .copied()
                                .unwrap_or(RawField::Null)
                                .to_owned_value(enc),
                        );
                    }
                    SmallBuildVals::Heap(v.into_boxed_slice())
                };
                let entry = BuildSideRow {
                    rowid: b_rid,
                    kept_vals,
                };
                match jk {
                    RawField::Text(b) if enc == TextEncoding::Utf8 => {
                        build_map.insert(b, false, entry);
                    }
                    RawField::Text(b) => {
                        let s = enc.decode_bytes(b);
                        build_map.insert(s.as_bytes(), false, entry);
                    }
                    RawField::Blob(b) => build_map.insert(b, true, entry),
                    RawField::Int(i) => {
                        let mut ibuf = [0u8; 24];
                        let s = format_i64_into_stack(i, &mut ibuf);
                        build_map.insert(s, false, entry);
                    }
                    RawField::Float(f) => {
                        let s = format_sqlite_real(f);
                        build_map.insert(s.as_bytes(), false, entry);
                    }
                    RawField::Null => {}
                }
                Ok(ControlFlow::Continue(()))
            },
        )?;

        if build_map.used_buckets == 0 {
            return Ok(Vec::new());
        }

        // 2. Stream probe table once, probing `build_map` and accumulating directly into groups:
        let mut dense_slots: [u32; 256] = [u32::MAX; 256];
        let mut group_map: HashMap<HashGroupBucketKey, u32, FastBuildHasher> = HashMap::default();
        let mut groups: Vec<(Value, Vec<FastAggState>)> = Vec::with_capacity(32);
        let b_col_len = if self.build_table.needs_record_parse {
            self.build_table.max_record_col + 1
        } else {
            0
        };

        scan_single_table_rows(
            bt,
            &self.probe_table,
            &self.probe_access,
            params,
            |p_rid, p_cols| {
                if let Some(ref p_pred) = self.probe_where {
                    let p_only = FastRowCtx {
                        rowid0: 0,
                        cols0: &[],
                        rowid1: p_rid,
                        cols1: p_cols,
                        params,
                        enc,
                    };
                    if !p_pred.eval_where_true(&p_only)? {
                        return Ok(ControlFlow::Continue(()));
                    }
                }

                let p_jk = match self.probe_join_key {
                    FastColSrc::Rowid { .. } => RawField::Int(p_rid),
                    FastColSrc::RecordCol { is_ipk: true, .. } => RawField::Int(p_rid),
                    FastColSrc::RecordCol { col_idx, .. } => p_cols
                        .get(col_idx as usize)
                        .copied()
                        .unwrap_or(RawField::Null),
                };
                if matches!(p_jk, RawField::Null) {
                    return Ok(ControlFlow::Continue(()));
                }

                let mut ibuf = [0u8; 24];
                let decoded_tmp: String;
                let (key_bytes, is_blob): (&[u8], bool) = match p_jk {
                    RawField::Text(b) if enc == TextEncoding::Utf8 => (b, false),
                    RawField::Text(b) => {
                        decoded_tmp = enc.decode_bytes(b);
                        (decoded_tmp.as_bytes(), false)
                    }
                    RawField::Blob(b) => (b, true),
                    RawField::Int(i) => (format_i64_into_stack(i, &mut ibuf), false),
                    RawField::Float(f) => {
                        decoded_tmp = format_sqlite_real(f);
                        (decoded_tmp.as_bytes(), false)
                    }
                    RawField::Null => unreachable!(),
                };

                let mut cur_entry = build_map.find_head(key_bytes, is_blob);
                while cur_entry != u32::MAX {
                    let (ref brow, next_idx) = build_map.entries[cur_entry as usize];
                    cur_entry = next_idx;

                    let mut b_raw_local: [RawField<'_>; 16] = [RawField::Null; 16];
                    let mut b_raw_heap: Vec<RawField<'_>>;
                    let b_slice: &[RawField<'_>] = if b_col_len <= 16 {
                        let kv = brow.kept_vals.as_slice(n_kept);
                        for (slot, &ci) in self.build_kept_cols.iter().enumerate() {
                            b_raw_local[ci as usize] = value_as_raw_field(&kv[slot]);
                        }
                        &b_raw_local[..b_col_len]
                    } else {
                        b_raw_heap = vec![RawField::Null; b_col_len];
                        let kv = brow.kept_vals.as_slice(n_kept);
                        for (slot, &ci) in self.build_kept_cols.iter().enumerate() {
                            b_raw_heap[ci as usize] = value_as_raw_field(&kv[slot]);
                        }
                        &b_raw_heap
                    };

                    let j_ctx = FastRowCtx {
                        rowid0: brow.rowid,
                        cols0: b_slice,
                        rowid1: p_rid,
                        cols1: p_cols,
                        params,
                        enc,
                    };
                    if let Some(ref post_pred) = self.post_join_where
                        && !post_pred.eval_where_true(&j_ctx)?
                    {
                        continue;
                    }

                    let gk_raw = j_ctx.get_col(self.group_col);
                    let grp_idx: usize = if let RawField::Int(iv) = gk_raw
                        && (0..256).contains(&iv)
                    {
                        let slot = &mut dense_slots[iv as usize];
                        if *slot == u32::MAX {
                            let idx = groups.len();
                            let init_states = self
                                .aggs
                                .iter()
                                .map(|s| FastAggState::from_spec(s, enc))
                                .collect();
                            groups.push((Value::Integer(iv), init_states));
                            *slot = idx as u32;
                            idx
                        } else {
                            *slot as usize
                        }
                    } else {
                        let owned = gk_raw.to_owned_value(enc);
                        let bkey = HashGroupBucketKey {
                            val: owned.clone(),
                            collation: self.group_collation,
                            enc,
                        };
                        if let Some(&idx) = group_map.get(&bkey) {
                            idx as usize
                        } else {
                            let idx = groups.len();
                            let init_states = self
                                .aggs
                                .iter()
                                .map(|s| FastAggState::from_spec(s, enc))
                                .collect();
                            groups.push((owned, init_states));
                            group_map.insert(bkey, idx as u32);
                            idx
                        }
                    };

                    let states = &mut groups[grp_idx].1;
                    for (st, spec) in states.iter_mut().zip(self.aggs.iter()) {
                        st.step(spec, &j_ctx)?;
                    }
                }
                Ok(ControlFlow::Continue(()))
            },
        )?;

        if groups.is_empty() {
            return Ok(Vec::new());
        }

        let mut finalized: Vec<(Value, Vec<Value>)> = Vec::with_capacity(groups.len());
        for (gk, states) in groups {
            let mut agg_vals = Vec::with_capacity(self.aggs.len());
            for (st, spec) in states.iter().zip(self.aggs.iter()) {
                agg_vals.push(st.finalize(spec)?);
            }
            finalized.push((gk, agg_vals));
        }

        if self.order_by.is_empty() && finalized.len() > 1 {
            // C groups through a sorter (or an index walk), so without ORDER BY the groups
            // come out in ascending key order; the hash table above collected them in
            // first-seen order. The sort is stable: the group keeps its first-seen key value.
            let coll = self.group_collation;
            finalized.sort_by(|(k_a, _), (k_b, _)| k_a.cmp_sqlite(k_b, coll));
            if self.groups_desc {
                finalized.reverse();
            }
        }
        if !self.order_by.is_empty() && finalized.len() > 1 {
            fallible_sort_by(&mut finalized, |(k_a, a_a), (k_b, a_b)| {
                let ka_arr = [k_a.clone()];
                let kb_arr = [k_b.clone()];
                for ob in self.order_by.iter() {
                    let va = ob.proj_or_key.eval(&ka_arr, a_a);
                    let vb = ob.proj_or_key.eval(&kb_arr, a_b);
                    let ord = compare_order_by_values(
                        &va,
                        &vb,
                        ob.desc,
                        ob.nulls_last,
                        ob.collation,
                        enc,
                    );
                    if ord != Ordering::Equal {
                        return Ok(ord);
                    }
                }
                Ok(Ordering::Equal)
            })?;
        }

        let mut out_rows = Vec::with_capacity(finalized.len());
        for (gk, agg_vals) in finalized {
            let key_arr = [gk];
            let mut row = Vec::with_capacity(self.projections.len());
            for p in self.projections.iter() {
                row.push(p.eval(&key_arr, &agg_vals));
            }
            out_rows.push(row);
        }
        Ok(out_rows)
    }
}

// ============================================================================
// Plan 6: Streaming Equi-Join + Compact Arena Sort (`uuid-query/join-*`)
// ============================================================================

impl FastArenaByteSet {
    /// Looks up `field` (under `TEXT` affinity + `BINARY` collation) and returns `(off, len_and_kind)`
    /// into `self.arena` so the exact stored `parent` bytes/kind are preserved.
    #[inline]
    fn find_slot(&self, bytes: &[u8], is_blob: bool) -> Option<(u32, u32)> {
        if self.len == 0 || bytes.len() >= (BLOB_TAG_BIT as usize) {
            return None;
        }
        let h = hash_bytes_fast(bytes);
        let lk = (bytes.len() as u32) | if is_blob { BLOB_TAG_BIT } else { 0 };
        let mut idx = (h as usize) & self.mask;
        loop {
            let slot = self.slots[idx];
            if slot.hash == 0 {
                return None;
            }
            if slot.hash == h && slot.len_and_kind == lk {
                let s_off = slot.off as usize;
                let s_len = (lk & !BLOB_TAG_BIT) as usize;
                if &self.arena[s_off..s_off + s_len] == bytes {
                    return Some((slot.off, slot.len_and_kind));
                }
            }
            idx = (idx + 1) & self.mask;
        }
    }

    #[inline]
    fn lookup_text_affinity_slot(
        &self,
        field: RawField<'_>,
        enc: TextEncoding,
    ) -> Option<(u32, u32)> {
        match field {
            RawField::Null => None,
            RawField::Text(b) => {
                if enc == TextEncoding::Utf8 {
                    self.find_slot(b, false)
                } else {
                    let s = enc.decode_bytes(b);
                    self.find_slot(s.as_bytes(), false)
                }
            }
            RawField::Blob(b) => self.find_slot(b, true),
            RawField::Int(i) => {
                let mut buf = [0u8; 24];
                let slice = format_i64_into_stack(i, &mut buf);
                self.find_slot(slice, false)
            }
            RawField::Float(f) => {
                let s = format_sqlite_real(f);
                self.find_slot(s.as_bytes(), false)
            }
        }
    }

    #[inline]
    fn slot_as_raw_field(&self, off: u32, lk: u32) -> RawField<'_> {
        let s_off = off as usize;
        let s_len = (lk & !BLOB_TAG_BIT) as usize;
        let bytes = &self.arena[s_off..s_off + s_len];
        if (lk & BLOB_TAG_BIT) != 0 {
            RawField::Blob(bytes)
        } else {
            RawField::Text(bytes)
        }
    }
}

impl FastExpr {
    /// Appends a `ConcatChain` (or general `Text` expression) directly into `arena`.
    /// Returns `Ok(Some((off, len)))` on non-null text, or `Ok(None)` if SQL `NULL`.
    fn eval_into_text_arena(
        &self,
        ctx: &FastRowCtx<'_>,
        arena: &mut String,
    ) -> Result<Option<(u32, u32)>> {
        let start = arena.len();
        let Ok(start_u32) = u32::try_from(start) else {
            let v = self.eval_value(ctx)?;
            if v.is_null() {
                return Ok(None);
            }
            return Err(SqliteError::new(
                ErrorCode::TooBig,
                "string or blob too big",
            ));
        };

        if let Self::ConcatChain { parts, .. } = self {
            for part in parts.iter() {
                match part {
                    FastConcatPart::Literal(lit) => arena.push_str(lit),
                    FastConcatPart::Col(c) => {
                        if !push_raw_field_text(arena, ctx.get_col(*c), ctx.enc, false) {
                            arena.truncate(start);
                            return Ok(None);
                        }
                    }
                    FastConcatPart::CastTextCol(c) => {
                        if !push_raw_field_text(arena, ctx.get_col(*c), ctx.enc, true) {
                            arena.truncate(start);
                            return Ok(None);
                        }
                    }
                    FastConcatPart::LowerHexCol(c) => {
                        if !push_raw_field_hex(arena, ctx.get_col(*c), ctx.enc, HEX_LOWER) {
                            arena.truncate(start);
                            return Ok(None);
                        }
                    }
                    FastConcatPart::UpperHexCol(c) => {
                        if !push_raw_field_hex(arena, ctx.get_col(*c), ctx.enc, HEX_UPPER) {
                            arena.truncate(start);
                            return Ok(None);
                        }
                    }
                    FastConcatPart::Expr(e) => {
                        let v = e.eval_value(ctx)?;
                        if v.is_null() {
                            arena.truncate(start);
                            return Ok(None);
                        }
                        match v {
                            Value::Text(s) => arena.push_str(&s),
                            Value::Integer(i) => {
                                let mut buf = [0u8; 24];
                                if let Ok(s) =
                                    std::str::from_utf8(format_i64_into_stack(i, &mut buf))
                                {
                                    arena.push_str(s);
                                }
                            }
                            Value::Real(r) => arena.push_str(&format_sqlite_real(r)),
                            Value::Blob(b) => arena.push_str(&String::from_utf8_lossy(&b)),
                            Value::Null => {
                                arena.truncate(start);
                                return Ok(None);
                            }
                        }
                    }
                }
            }
            let byte_len = arena.len() - start;
            let Ok(len_u32) = u32::try_from(byte_len) else {
                arena.truncate(start);
                return Err(SqliteError::new(
                    ErrorCode::TooBig,
                    "string or blob too big",
                ));
            };
            return Ok(Some((start_u32, len_u32)));
        }

        let v = self.eval_value(ctx)?;
        if v.is_null() {
            return Ok(None);
        }
        arena.push_str(&v.to_text_lossy());
        let byte_len = arena.len() - start;
        let len_u32 = u32::try_from(byte_len)
            .map_err(|_| SqliteError::new(ErrorCode::TooBig, "string or blob too big"))?;
        Ok(Some((start_u32, len_u32)))
    }
}

#[derive(Debug, Clone)]
enum ParentKeyScan {
    /// Scan unique/PK index on `parent` where field `0` is `parent.<join_col>`.
    UniqueIndex { root_pgno: Pgno },
    /// Scan `parent` table leaves (`NOT INDEXED`).
    TableScan { col_idx: u16 },
}

#[derive(Debug, Clone)]
enum DetailScanMode {
    /// Scan `detail` table leaves sequentially (`NOT INDEXED` or default).
    TableScan,
    /// Scan forced/chosen index on `detail` (`INDEXED BY detail_parent_time`):
    /// if `join_col_in_idx` is `Some(pos)`, filter against `parent_set` directly on index leaves,
    /// sort matched `rowid`s ascending, and merge-read `detail` table leaves sequentially!
    IndexDriven {
        index_root: Pgno,
        n_index_cols: usize,
        join_col_in_idx: Option<usize>,
    },
}

#[derive(Debug, Clone)]
struct JoinOrderTerm {
    expr: FastExpr,
    desc: bool,
    nulls_last: bool,
    collation: Collation,
}

#[derive(Debug, Clone)]
struct EquiJoinProjectPlan {
    /// Side `0`: `detail` outer/probe table.
    detail_table: FastTableDesc,
    detail_scan: DetailScanMode,
    detail_join_col: FastColSrc,
    /// Side `1`: `parent` lookup table (equi-joined on `TEXT` `BINARY` `UNIQUE NOT NULL` key,
    /// with ONLY `parent.<join_col>` referenced from `parent`).
    parent_root_pgno: Pgno,
    parent_scan: ParentKeyScan,
    parent_join_col_idx: u16,
    where_pred: Option<FastExpr>,
    projections: Box<[FastExpr]>,
    order_by: Box<[JoinOrderTerm]>,
    /// Fast path when `projections.len() == 1` (`ConcatChain`) and `order_by` is a single
    /// `NOT NULL TEXT BINARY` column on `detail` (`uuid-query/join-index` & `join-noindex`).
    fast_single_text_sort_col: Option<(FastColSrc, bool /* desc */)>,
}

/// Output of executing a [`FastReadPlan`].
pub enum FastReadExecOut {
    Empty,
    SingleRow(Vec<Value>),
    Rows(Vec<Vec<Value>>),
    /// Ultra-compact arena representation of N single-column `Text` (or `Null` when `len == u32::MAX`) rows.
    SingleTextArena {
        arena: String,
        spans: Vec<(u32, u32)>,
    },
}

impl FastReadExecOut {
    #[inline]
    fn from_rows(mut rows: Vec<Vec<Value>>) -> Self {
        match rows.len() {
            0 => Self::Empty,
            1 => Self::SingleRow(rows.pop().unwrap()),
            _ => Self::Rows(rows),
        }
    }

    pub fn into_materialized_rows(self) -> Vec<Vec<Value>> {
        match self {
            Self::Empty => Vec::new(),
            Self::SingleRow(r) => vec![r],
            Self::Rows(rs) => rs,
            Self::SingleTextArena { arena, spans } => spans
                .into_iter()
                .map(|(off, len)| {
                    if len == u32::MAX {
                        vec![Value::Null]
                    } else {
                        let s = &arena[off as usize..(off + len) as usize];
                        vec![Value::Text(s.to_owned())]
                    }
                })
                .collect(),
        }
    }
}

impl EquiJoinProjectPlan {
    fn execute(
        &self,
        bt: &mut Btree,
        params: &[Value],
        enc: TextEncoding,
    ) -> Result<FastReadExecOut> {
        // 1. Build `FastArenaByteSet` of unique `parent` keys (2 `Vec` allocations total):
        let mut parent_set = FastArenaByteSet::with_capacity(3_072, 36);
        match self.parent_scan {
            ParentKeyScan::UniqueIndex { root_pgno } => {
                bt.scan_index_raw_cells(root_pgno, false, |cell| {
                    let mut f0 = [RawField::Null; 1];
                    let n = parse_raw_record_prefix(cell, &mut f0)?;
                    if n >= 1 {
                        parent_set.insert_text_affinity_field(f0[0], enc);
                    }
                    Ok(ControlFlow::Continue(()))
                })?;
            }
            ParentKeyScan::TableScan { col_idx } => {
                let prefix_len = (col_idx as usize) + 1;
                bt.scan_table_range_raw_cells(
                    self.parent_root_pgno,
                    None,
                    None,
                    |_rid, payload| {
                        let mut local: [RawField<'_>; 16] = [RawField::Null; 16];
                        if prefix_len <= 16 {
                            let n = parse_raw_record_prefix(payload, &mut local[..prefix_len])?;
                            if (col_idx as usize) < n {
                                parent_set.insert_text_affinity_field(local[col_idx as usize], enc);
                            }
                        } else {
                            let mut heap = vec![RawField::Null; prefix_len];
                            let n = parse_raw_record_prefix(payload, &mut heap)?;
                            if (col_idx as usize) < n {
                                parent_set.insert_text_affinity_field(heap[col_idx as usize], enc);
                            }
                        }
                        Ok(ControlFlow::Continue(()))
                    },
                )?;
            }
        }

        if parent_set.len == 0 {
            return Ok(FastReadExecOut::Empty);
        }

        let d_col_len = if self.detail_table.needs_record_parse {
            self.detail_table.max_record_col + 1
        } else {
            0
        };
        let p_col_len = (self.parent_join_col_idx as usize) + 1;

        // Fast arena path for single `ConcatChain` + single `TEXT BINARY` `ORDER BY` (`uuid-query/join-*`):
        if let Some((sort_col, sort_desc)) = self.fast_single_text_sort_col
            && enc == TextEncoding::Utf8
        {
            let mut sort_key_arena: Vec<u8> = Vec::with_capacity(12_288 * 36);
            let mut text_arena = String::with_capacity(12_288 * 184);
            // `(key_off, key_len, text_off, text_len)` - 16 bytes per row!
            let mut items: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(12_288);

            let mut process_detail_row = |d_rid: i64,
                                          d_cols: &[RawField<'_>],
                                          pre_matched: Option<(u32, u32)>|
             -> Result<()> {
                let (p_off, p_lk) = match pre_matched {
                    Some(pair) => pair,
                    None => {
                        let d_only = FastRowCtx::single(d_rid, d_cols, params, enc);
                        let d_jk = d_only.get_col(self.detail_join_col);
                        let Some(pair) = parent_set.lookup_text_affinity_slot(d_jk, enc) else {
                            return Ok(());
                        };
                        pair
                    }
                };

                let p_field = parent_set.slot_as_raw_field(p_off, p_lk);
                let mut p_local: [RawField<'_>; 16] = [RawField::Null; 16];
                let mut p_heap: Vec<RawField<'_>>;
                let p_slice: &[RawField<'_>] = if p_col_len <= 16 {
                    p_local[self.parent_join_col_idx as usize] = p_field;
                    &p_local[..p_col_len]
                } else {
                    p_heap = vec![RawField::Null; p_col_len];
                    p_heap[self.parent_join_col_idx as usize] = p_field;
                    &p_heap
                };

                let ctx = FastRowCtx {
                    rowid0: d_rid,
                    cols0: d_cols,
                    rowid1: 0,
                    cols1: p_slice,
                    params,
                    enc,
                };
                if let Some(ref pred) = self.where_pred
                    && !pred.eval_where_true(&ctx)?
                {
                    return Ok(());
                }

                let sk_field = ctx.get_col(sort_col);
                let k_off = u32::try_from(sort_key_arena.len())
                    .map_err(|_| SqliteError::new(ErrorCode::TooBig, "sort arena overflow"))?;
                let mut num_buf = [0u8; 24];
                let real_tmp: String;
                let (tag, sk_bytes): (u8, &[u8]) = match sk_field {
                    RawField::Null => (0u8, &[]),
                    RawField::Text(b) => (1u8, b),
                    RawField::Blob(b) => (2u8, b),
                    RawField::Int(i) => (1u8, format_i64_into_stack(i, &mut num_buf)),
                    RawField::Float(f) => {
                        real_tmp = format_sqlite_real(f);
                        (1u8, real_tmp.as_bytes())
                    }
                };
                let k_len = u32::try_from(1 + sk_bytes.len())
                    .map_err(|_| SqliteError::new(ErrorCode::TooBig, "sort key too big"))?;
                sort_key_arena.push(tag);
                sort_key_arena.extend_from_slice(sk_bytes);

                let (t_off, t_len) = self.projections[0]
                    .eval_into_text_arena(&ctx, &mut text_arena)?
                    .unwrap_or((0, u32::MAX));
                items.push((k_off, k_len, t_off, t_len));
                Ok(())
            };

            match self.detail_scan {
                DetailScanMode::TableScan => {
                    bt.scan_table_range_raw_cells(
                        self.detail_table.root_pgno,
                        None,
                        None,
                        |d_rid, payload| {
                            let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                            if d_col_len <= 16 {
                                self.detail_table.fill_from_table_payload(
                                    payload,
                                    &mut local_cols[..d_col_len],
                                )?;
                                process_detail_row(d_rid, &local_cols[..d_col_len], None)?;
                            } else {
                                let mut heap_cols = vec![RawField::Null; d_col_len];
                                self.detail_table
                                    .fill_from_table_payload(payload, &mut heap_cols)?;
                                process_detail_row(d_rid, &heap_cols, None)?;
                            }
                            Ok(ControlFlow::Continue(()))
                        },
                    )?;
                }
                DetailScanMode::IndexDriven {
                    index_root,
                    n_index_cols,
                    join_col_in_idx,
                } => {
                    // Collect `(rowid, Option<(p_off, p_lk)>)` from the index, with a 1-entry
                    // consecutive `parent_id` cache so adjacent rows sharing `parent_id` skip hashing!
                    let mut matched_rids: Vec<(i64, Option<(u32, u32)>)> =
                        Vec::with_capacity(12_288);
                    let mut cached_key = [0u8; 64];
                    let mut cached_len: usize = usize::MAX;
                    let mut cached_res: Option<(u32, u32)> = None;

                    bt.scan_index_raw_cells(index_root, false, |cell| {
                        let mut idx_f: [RawField<'_>; 16] = [RawField::Null; 16];
                        let need = n_index_cols + 1;
                        let n = if need <= 16 {
                            parse_raw_record_prefix(cell, &mut idx_f[..need])?
                        } else {
                            return Err(SqliteError::new(
                                ErrorCode::Internal,
                                "index arity exceeds fast bound",
                            ));
                        };
                        if n_index_cols >= n {
                            return Ok(ControlFlow::Continue(()));
                        }
                        let RawField::Int(rid) = idx_f[n_index_cols] else {
                            return Ok(ControlFlow::Continue(()));
                        };
                        if let Some(jpos) = join_col_in_idx {
                            let jf = idx_f[jpos];
                            let hit = if let RawField::Text(jb) = jf
                                && jb.len() <= 64
                            {
                                if cached_len == jb.len() && &cached_key[..cached_len] == jb {
                                    cached_res
                                } else {
                                    let r = parent_set.find_slot(jb, false);
                                    cached_key[..jb.len()].copy_from_slice(jb);
                                    cached_len = jb.len();
                                    cached_res = r;
                                    r
                                }
                            } else {
                                parent_set.lookup_text_affinity_slot(jf, enc)
                            };
                            if let Some(pair) = hit {
                                matched_rids.push((rid, Some(pair)));
                            }
                        } else {
                            matched_rids.push((rid, None));
                        }
                        Ok(ControlFlow::Continue(()))
                    })?;

                    if matched_rids.is_empty() {
                        return Ok(FastReadExecOut::Empty);
                    }

                    // Sort matched rowids ascending and scan `detail` table leaves sequentially!
                    matched_rids.sort_unstable_by_key(|&(rid, _)| rid);
                    let first_rid = matched_rids[0].0;
                    let last_rid = matched_rids[matched_rids.len() - 1].0;
                    let mut cursor = 0usize;

                    bt.scan_table_range_raw_cells(
                        self.detail_table.root_pgno,
                        Some((first_rid, true)),
                        Some((last_rid, true)),
                        |d_rid, payload| {
                            while cursor < matched_rids.len() && matched_rids[cursor].0 < d_rid {
                                cursor += 1;
                            }
                            if cursor >= matched_rids.len() {
                                return Ok(ControlFlow::Break(()));
                            }
                            if matched_rids[cursor].0 == d_rid {
                                let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                                let mut heap_cols: Vec<RawField<'_>>;
                                let cols_slice: &[RawField<'_>] = if d_col_len <= 16 {
                                    self.detail_table.fill_from_table_payload(
                                        payload,
                                        &mut local_cols[..d_col_len],
                                    )?;
                                    &local_cols[..d_col_len]
                                } else {
                                    heap_cols = vec![RawField::Null; d_col_len];
                                    self.detail_table
                                        .fill_from_table_payload(payload, &mut heap_cols)?;
                                    &heap_cols
                                };
                                while cursor < matched_rids.len() && matched_rids[cursor].0 == d_rid
                                {
                                    let pre = matched_rids[cursor].1;
                                    cursor += 1;
                                    process_detail_row(d_rid, cols_slice, pre)?;
                                }
                            }
                            Ok(ControlFlow::Continue(()))
                        },
                    )?;
                }
            }

            // Free `parent_set` before sorting!
            drop(parent_set);

            if items.is_empty() {
                return Ok(FastReadExecOut::Empty);
            }

            items.sort_by(|&(ka_off, ka_len, _, _), &(kb_off, kb_len, _, _)| {
                let sa = &sort_key_arena[ka_off as usize..(ka_off + ka_len) as usize];
                let sb = &sort_key_arena[kb_off as usize..(kb_off + kb_len) as usize];
                if sort_desc { sb.cmp(sa) } else { sa.cmp(sb) }
            });
            // Free `sort_key_arena` immediately after sorting!
            drop(sort_key_arena);

            if items.len() == 1 {
                let (_, _, t_off, t_len) = items[0];
                if t_len == u32::MAX {
                    return Ok(FastReadExecOut::SingleRow(vec![Value::Null]));
                }
                let s = text_arena[t_off as usize..(t_off + t_len) as usize].to_owned();
                return Ok(FastReadExecOut::SingleRow(vec![Value::Text(s)]));
            }

            let spans: Vec<(u32, u32)> = items
                .into_iter()
                .map(|(_, _, t_off, t_len)| (t_off, t_len))
                .collect();
            return Ok(FastReadExecOut::SingleTextArena {
                arena: text_arena,
                spans,
            });
        }

        // General multi-column / general `ORDER BY` path (`JOIN` / `NOINDEX` in `uuid_family.py`):
        let mut keyed_rows: Vec<(Vec<Value>, Vec<Value>)> = Vec::with_capacity(1_024);

        let mut process_general_row =
            |d_rid: i64, d_cols: &[RawField<'_>], pre_matched: Option<(u32, u32)>| -> Result<()> {
                let (p_off, p_lk) = match pre_matched {
                    Some(pair) => pair,
                    None => {
                        let d_only = FastRowCtx::single(d_rid, d_cols, params, enc);
                        let d_jk = d_only.get_col(self.detail_join_col);
                        let Some(pair) = parent_set.lookup_text_affinity_slot(d_jk, enc) else {
                            return Ok(());
                        };
                        pair
                    }
                };
                let p_field = parent_set.slot_as_raw_field(p_off, p_lk);
                let mut p_local: [RawField<'_>; 16] = [RawField::Null; 16];
                let mut p_heap: Vec<RawField<'_>>;
                let p_slice: &[RawField<'_>] = if p_col_len <= 16 {
                    p_local[self.parent_join_col_idx as usize] = p_field;
                    &p_local[..p_col_len]
                } else {
                    p_heap = vec![RawField::Null; p_col_len];
                    p_heap[self.parent_join_col_idx as usize] = p_field;
                    &p_heap
                };

                let ctx = FastRowCtx {
                    rowid0: d_rid,
                    cols0: d_cols,
                    rowid1: 0,
                    cols1: p_slice,
                    params,
                    enc,
                };
                if let Some(ref pred) = self.where_pred
                    && !pred.eval_where_true(&ctx)?
                {
                    return Ok(());
                }

                let mut sort_vals = Vec::with_capacity(self.order_by.len());
                for ob in self.order_by.iter() {
                    sort_vals.push(ob.expr.eval_value(&ctx)?);
                }
                let mut proj_vals = Vec::with_capacity(self.projections.len());
                for p in self.projections.iter() {
                    proj_vals.push(p.eval_value(&ctx)?);
                }
                keyed_rows.push((sort_vals, proj_vals));
                Ok(())
            };

        match self.detail_scan {
            DetailScanMode::TableScan => {
                bt.scan_table_range_raw_cells(
                    self.detail_table.root_pgno,
                    None,
                    None,
                    |d_rid, payload| {
                        let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                        if d_col_len <= 16 {
                            self.detail_table
                                .fill_from_table_payload(payload, &mut local_cols[..d_col_len])?;
                            process_general_row(d_rid, &local_cols[..d_col_len], None)?;
                        } else {
                            let mut heap_cols = vec![RawField::Null; d_col_len];
                            self.detail_table
                                .fill_from_table_payload(payload, &mut heap_cols)?;
                            process_general_row(d_rid, &heap_cols, None)?;
                        }
                        Ok(ControlFlow::Continue(()))
                    },
                )?;
            }
            DetailScanMode::IndexDriven {
                index_root,
                n_index_cols,
                join_col_in_idx,
            } => {
                let mut matched_rids: Vec<(i64, Option<(u32, u32)>)> = Vec::with_capacity(1_024);
                bt.scan_index_raw_cells(index_root, false, |cell| {
                    let mut idx_f: [RawField<'_>; 16] = [RawField::Null; 16];
                    let need = n_index_cols + 1;
                    if need > 16 {
                        return Err(SqliteError::new(
                            ErrorCode::Internal,
                            "index arity exceeds fast bound",
                        ));
                    }
                    let n = parse_raw_record_prefix(cell, &mut idx_f[..need])?;
                    if n_index_cols >= n {
                        return Ok(ControlFlow::Continue(()));
                    }
                    let RawField::Int(rid) = idx_f[n_index_cols] else {
                        return Ok(ControlFlow::Continue(()));
                    };
                    if let Some(jpos) = join_col_in_idx {
                        if let Some(pair) = parent_set.lookup_text_affinity_slot(idx_f[jpos], enc) {
                            matched_rids.push((rid, Some(pair)));
                        }
                    } else {
                        matched_rids.push((rid, None));
                    }
                    Ok(ControlFlow::Continue(()))
                })?;

                if matched_rids.is_empty() {
                    return Ok(FastReadExecOut::Empty);
                }
                matched_rids.sort_unstable_by_key(|&(rid, _)| rid);
                let first_rid = matched_rids[0].0;
                let last_rid = matched_rids[matched_rids.len() - 1].0;
                let mut cursor = 0usize;
                bt.scan_table_range_raw_cells(
                    self.detail_table.root_pgno,
                    Some((first_rid, true)),
                    Some((last_rid, true)),
                    |d_rid, payload| {
                        while cursor < matched_rids.len() && matched_rids[cursor].0 < d_rid {
                            cursor += 1;
                        }
                        if cursor >= matched_rids.len() {
                            return Ok(ControlFlow::Break(()));
                        }
                        if matched_rids[cursor].0 == d_rid {
                            let mut local_cols: [RawField<'_>; 16] = [RawField::Null; 16];
                            let mut heap_cols: Vec<RawField<'_>>;
                            let cols_slice: &[RawField<'_>] = if d_col_len <= 16 {
                                self.detail_table.fill_from_table_payload(
                                    payload,
                                    &mut local_cols[..d_col_len],
                                )?;
                                &local_cols[..d_col_len]
                            } else {
                                heap_cols = vec![RawField::Null; d_col_len];
                                self.detail_table
                                    .fill_from_table_payload(payload, &mut heap_cols)?;
                                &heap_cols
                            };
                            while cursor < matched_rids.len() && matched_rids[cursor].0 == d_rid {
                                let pre = matched_rids[cursor].1;
                                cursor += 1;
                                process_general_row(d_rid, cols_slice, pre)?;
                            }
                        }
                        Ok(ControlFlow::Continue(()))
                    },
                )?;
            }
        }

        drop(parent_set);

        if keyed_rows.is_empty() {
            return Ok(FastReadExecOut::Empty);
        }

        if !self.order_by.is_empty() && keyed_rows.len() > 1 {
            keyed_rows.sort_by(|(ka, _), (kb, _)| {
                for (i, ob) in self.order_by.iter().enumerate() {
                    let ord = compare_order_by_values(
                        &ka[i],
                        &kb[i],
                        ob.desc,
                        ob.nulls_last,
                        ob.collation,
                        enc,
                    );
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                Ordering::Equal
            });
        }

        let rows: Vec<Vec<Value>> = keyed_rows.into_iter().map(|(_, vals)| vals).collect();
        Ok(FastReadExecOut::from_rows(rows))
    }
}

// ============================================================================
// Top-Level `FastReadPlan` & Planner Entry Points
// ============================================================================

#[derive(Debug, Clone)]
enum FastReadKind {
    UngroupedAgg(UngroupedAggPlan),
    OrderedCoveringGroupBy(OrderedCoveringGroupByPlan),
    HashGroupBy(HashGroupByPlan),
    OrderedIndexSelect(OrderedIndexSelectPlan),
    EquiJoinGroupBy(Box<EquiJoinGroupByPlan>),
    EquiJoinProject(EquiJoinProjectPlan),
}

#[derive(Debug, Clone)]
pub struct FastReadPlan {
    pub db_id: DatabaseId,
    pub column_names: Vec<String>,
    pub lease_roots: Box<[Pgno]>,
    kind: FastReadKind,
}

impl FastReadPlan {
    pub fn execute(
        &self,
        bt: &mut Btree,
        params: &[Value],
        enc: TextEncoding,
    ) -> Result<FastReadExecOut> {
        match &self.kind {
            FastReadKind::UngroupedAgg(p) => {
                Ok(FastReadExecOut::SingleRow(p.execute(bt, params, enc)?))
            }
            FastReadKind::OrderedCoveringGroupBy(p) => {
                Ok(FastReadExecOut::from_rows(p.execute(bt, params, enc)?))
            }
            FastReadKind::HashGroupBy(p) => {
                Ok(FastReadExecOut::from_rows(p.execute(bt, params, enc)?))
            }
            FastReadKind::OrderedIndexSelect(p) => p.execute(bt, params, enc),
            FastReadKind::EquiJoinGroupBy(p) => {
                Ok(FastReadExecOut::from_rows(p.execute(bt, params, enc)?))
            }
            FastReadKind::EquiJoinProject(p) => p.execute(bt, params, enc),
        }
    }
}

#[cfg(test)]
impl FastReadPlan {
    /// Whether the plan carries an integer row kernel (test instrumentation).
    pub(crate) fn has_int_kernel(&self) -> bool {
        match &self.kind {
            FastReadKind::UngroupedAgg(p) => p.int_kernel.is_some(),
            FastReadKind::OrderedCoveringGroupBy(p) => p.int_kernel.is_some(),
            _ => false,
        }
    }

    /// The same plan with its integer row kernel removed, so that every row takes the general
    /// row code (test instrumentation).
    pub(crate) fn without_int_kernel(&self) -> Self {
        let mut plan = self.clone();
        match &mut plan.kind {
            FastReadKind::UngroupedAgg(p) => p.int_kernel = None,
            FastReadKind::OrderedCoveringGroupBy(p) => p.int_kernel = None,
            _ => {}
        }
        plan
    }
}

fn validate_and_resolve_table_tos(
    tos: &TableOrSubquery,
    selection: Option<&Expr>,
    db_set: &DatabaseSet,
    side: u8,
) -> Option<(DatabaseId, Schema, FastTableBinding, Option<IndexHint>)> {
    let TableOrSubquery::Table {
        name,
        table_args: None,
        alias,
        indexed_by,
    } = tos
    else {
        return None;
    };
    if db_set.resolve_catalog_table(name).ok()?.is_some() {
        return None;
    }
    let (db_id, ResolvedRelation::Table(tbl)) = db_set.find_relation_in_set(name).ok()?? else {
        return None;
    };
    if tbl.without_rowid
        || tbl
            .name
            .as_bytes()
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case(b"sqlite_"))
    {
        return None;
    }
    let slot = db_set.slot(db_id)?;
    let schema_name = slot.name.clone();
    let slot_schema = slot.schema.clone();
    let eff_alias = alias.clone().unwrap_or_else(|| name.name.clone());

    if let Some(IndexHint::IndexedBy(idx_name)) = indexed_by {
        let idx = slot_schema
            .indexes_for_table(&tbl.name)
            .into_iter()
            .find(|i| !i.is_autoindex && i.name.eq_ignore_ascii_case(idx_name))?;
        if let Some(ref pw) = idx.where_clause {
            let usable = selection.is_some_and(|wh| {
                expr_implies_partial_where(wh, pw, &tbl, &schema_name, alias.as_deref())
            });
            if !usable {
                return None;
            }
        }
    }

    Some((
        db_id,
        slot_schema,
        FastTableBinding {
            side,
            schema_name,
            table_name: tbl.name.clone(),
            eff_alias,
            has_explicit_alias: alias.is_some(),
            tbl: *tbl,
        },
        indexed_by.clone(),
    ))
}

fn collect_col_flags_from_desc(td: &FastTableDesc, total_cols: usize) -> Vec<bool> {
    let mut used = vec![false; total_cols];
    if td.needs_record_parse {
        for flag in used
            .iter_mut()
            .take((td.max_record_col + 1).min(total_cols))
        {
            *flag = true;
        }
    }
    used
}

fn mark_col_src_exact(src: FastColSrc, exact_cols: &mut [bool], needs_rowid: &mut bool) {
    match src {
        FastColSrc::Rowid { .. } | FastColSrc::RecordCol { is_ipk: true, .. } => {
            *needs_rowid = true
        }
        FastColSrc::RecordCol {
            col_idx,
            is_ipk: false,
            ..
        } => {
            if let Some(slot) = exact_cols.get_mut(col_idx as usize) {
                *slot = true;
            }
        }
    }
}

fn collect_expr_exact_usage(expr: &FastExpr, exact_cols: &mut [bool], needs_rowid: &mut bool) {
    match expr {
        FastExpr::Const(_) | FastExpr::Param(_) => {}
        FastExpr::Col(c)
        | FastExpr::ColCmpRhs { col: c, .. }
        | FastExpr::ColBetweenRhs { col: c, .. }
        | FastExpr::JsonArrowCmpRhs { col: c, .. }
        | FastExpr::ColLike { col: c, .. }
        | FastExpr::JsonArrow { col: c, .. } => mark_col_src_exact(*c, exact_cols, needs_rowid),
        FastExpr::ArithCols { left, right, .. } => {
            mark_col_src_exact(*left, exact_cols, needs_rowid);
            mark_col_src_exact(*right, exact_cols, needs_rowid);
        }
        FastExpr::UnaryPlus(e)
        | FastExpr::Negate(e)
        | FastExpr::Not(e)
        | FastExpr::IsNull { inner: e, .. }
        | FastExpr::Cast { inner: e, .. }
        | FastExpr::Hex(e)
        | FastExpr::LowerHex(e)
        | FastExpr::UpperHex(e)
        | FastExpr::LowerAscii(e)
        | FastExpr::UpperAscii(e) => collect_expr_exact_usage(e, exact_cols, needs_rowid),
        FastExpr::Arith { left, right, .. }
        | FastExpr::Compare { left, right, .. }
        | FastExpr::And(left, right)
        | FastExpr::Or(left, right) => {
            collect_expr_exact_usage(left, exact_cols, needs_rowid);
            collect_expr_exact_usage(right, exact_cols, needs_rowid);
        }
        FastExpr::Coalesce(es) => {
            for e in es.iter() {
                collect_expr_exact_usage(e, exact_cols, needs_rowid);
            }
        }
        FastExpr::ConcatChain { parts, .. } => {
            for p in parts.iter() {
                match p {
                    FastConcatPart::Literal(_) => {}
                    FastConcatPart::Col(c)
                    | FastConcatPart::CastTextCol(c)
                    | FastConcatPart::LowerHexCol(c)
                    | FastConcatPart::UpperHexCol(c) => {
                        mark_col_src_exact(*c, exact_cols, needs_rowid);
                    }
                    FastConcatPart::Expr(e) => collect_expr_exact_usage(e, exact_cols, needs_rowid),
                }
            }
        }
    }
}

fn collect_agg_exact_usage(spec: &FastAggSpec, exact_cols: &mut [bool], needs_rowid: &mut bool) {
    match spec {
        FastAggSpec::CountStar => {}
        FastAggSpec::Count(e)
        | FastAggSpec::Sum(e)
        | FastAggSpec::Total(e)
        | FastAggSpec::Avg(e)
        | FastAggSpec::Min(e, _)
        | FastAggSpec::Max(e, _)
        | FastAggSpec::JsonGroupArray(e) => collect_expr_exact_usage(e, exact_cols, needs_rowid),
    }
}

fn compile_fast_single_access(
    ap: &AccessPlan,
    tbl: &TableSchema,
    td: &FastTableDesc,
    exact_cols: &[bool],
    needs_rowid: bool,
) -> Option<FastSingleAccess> {
    match ap {
        AccessPlan::FullScan => Some(FastSingleAccess::TableFullScan),
        AccessPlan::RowidSeek { key_expr } => Some(FastSingleAccess::TableRowidSeek {
            key: compile_scalar_rhs(key_expr)?,
        }),
        AccessPlan::RowidRange { lower, upper } => {
            let lo = match lower {
                None => None,
                Some(lb) => Some(FastRangeBound {
                    rhs: compile_scalar_rhs(&lb.expr)?,
                    inclusive: lb.inclusive,
                }),
            };
            let hi = match upper {
                None => None,
                Some(ub) => Some(FastRangeBound {
                    rhs: compile_scalar_rhs(&ub.expr)?,
                    inclusive: ub.inclusive,
                }),
            };
            Some(FastSingleAccess::TableRowidRange {
                lower: lo,
                upper: hi,
            })
        }
        AccessPlan::IndexFullScan {
            index,
            is_covering: true,
        } => {
            if !check_index_covers_all_exprs(tbl, index, exact_cols) {
                return None;
            }
            let cov = build_covering_index_desc(tbl, index, td, needs_rowid)?;
            Some(FastSingleAccess::CoveringIndexFullScan { cov })
        }
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
            is_covering: true,
        } => {
            if !check_index_covers_all_exprs(tbl, index, exact_cols) {
                return None;
            }
            let cov = build_covering_index_desc(tbl, index, td, needs_rowid)?;
            let mut p_keys = Vec::with_capacity(prefix_exprs.len());
            let mut p_affs = Vec::with_capacity(prefix_exprs.len());
            for (i, pe) in prefix_exprs.iter().enumerate() {
                p_keys.push(compile_scalar_rhs(pe)?);
                let aff = index.columns[i]
                    .col_idx
                    .map_or(TypeAffinity::Blob, |ci| tbl.columns[ci].affinity);
                p_affs.push(aff);
            }
            Some(FastSingleAccess::CoveringIndexRangeScan {
                cov,
                prefix_keys: p_keys.into_boxed_slice(),
                prefix_affinities: p_affs.into_boxed_slice(),
                range_aff: TypeAffinity::Blob,
                lower: None,
                upper: None,
            })
        }
        AccessPlan::IndexRangeScan {
            index,
            prefix_exprs,
            prefix_in_lists,
            range_col_idx,
            lower,
            upper,
            is_covering: true,
            skip_scan_cols: 0,
        } if prefix_in_lists.is_empty() => {
            if let Some(rc) = *range_col_idx
                && rc != prefix_exprs.len()
            {
                return None;
            }
            if !check_index_covers_all_exprs(tbl, index, exact_cols) {
                return None;
            }
            let cov = build_covering_index_desc(tbl, index, td, needs_rowid)?;
            let mut p_keys = Vec::with_capacity(prefix_exprs.len());
            let mut p_affs = Vec::with_capacity(prefix_exprs.len());
            for (i, pe) in prefix_exprs.iter().enumerate() {
                p_keys.push(compile_scalar_rhs(pe)?);
                let aff = index.columns[i]
                    .col_idx
                    .map_or(TypeAffinity::Blob, |ci| tbl.columns[ci].affinity);
                p_affs.push(aff);
            }
            let range_aff = range_col_idx
                .and_then(|rc| index.columns.get(rc))
                .and_then(|ic| ic.col_idx)
                .map_or(TypeAffinity::Blob, |ci| tbl.columns[ci].affinity);
            let lo = match lower {
                None => None,
                Some(lb) => Some(FastRangeBound {
                    rhs: compile_scalar_rhs(&lb.expr)?,
                    inclusive: lb.inclusive,
                }),
            };
            let hi = match upper {
                None => None,
                Some(ub) => Some(FastRangeBound {
                    rhs: compile_scalar_rhs(&ub.expr)?,
                    inclusive: ub.inclusive,
                }),
            };
            Some(FastSingleAccess::CoveringIndexRangeScan {
                cov,
                prefix_keys: p_keys.into_boxed_slice(),
                prefix_affinities: p_affs.into_boxed_slice(),
                range_aff,
                lower: lo,
                upper: hi,
            })
        }
        _ => None,
    }
}

/// [`compile_fast_single_access`] for single-table aggregate plans, which can additionally run a
/// non-covering full index scan (the planner picks one to feed `GROUP BY`/`ORDER BY` in index
/// order) as an index-ordered rowid lookup: rows reach the aggregates in exactly the order the
/// generic executor materializes them.
fn compile_fast_agg_access(
    ap: &AccessPlan,
    tbl: &TableSchema,
    td: &FastTableDesc,
    exact_cols: &[bool],
    needs_rowid: bool,
) -> Option<FastSingleAccess> {
    if let AccessPlan::IndexFullScan {
        index,
        is_covering: false,
    } = ap
    {
        if tbl.without_rowid || index.root_pgno == tbl.root_pgno {
            return None;
        }
        return Some(FastSingleAccess::IndexOrderedTableLookup {
            index_root: index.root_pgno,
            rid_field_pos: index.columns.len(),
        });
    }
    compile_fast_single_access(ap, tbl, td, exact_cols, needs_rowid)
}

#[allow(clippy::too_many_arguments)]
fn compile_group_sort_specs(
    order_by: &[OrderByTerm],
    expanded_proj: &[(Expr, String, Option<String>)],
    sample_cols: &[ScopedColumn],
    db_set: &DatabaseSet,
    group_cols: &[FastResolvedCol],
    projections: &[FastGroupProjExpr],
    scope: &FastCompileScope<'_>,
    aggs: &mut Vec<FastAggSpec>,
) -> Option<Box<[GroupSortSpec]>> {
    let mut specs = Vec::with_capacity(order_by.len());
    for ob in order_by {
        let nulls_last = ob.nulls_last.unwrap_or(ob.desc);
        if let Expr::Integer(ord) = ob.expr
            && ord >= 1
            && (ord as usize) <= projections.len()
        {
            let idx = (ord as usize) - 1;
            let (_, coll_opt) = scope.expr_meta(&expanded_proj[idx].0);
            if matches!(&expanded_proj[idx].0, Expr::Collate { .. }) && coll_opt.is_none() {
                return None;
            }
            specs.push(GroupSortSpec {
                proj_or_key: projections[idx].clone(),
                desc: ob.desc,
                nulls_last,
                collation: coll_opt.unwrap_or(Collation::Binary),
            });
            continue;
        }
        let sub_e = subst_select_aliases_pub(&ob.expr, expanded_proj, sample_cols, db_set);
        let (_, coll_opt) = scope.expr_meta(&sub_e);
        if matches!(&sub_e, Expr::Collate { .. }) && coll_opt.is_none() {
            return None;
        }
        let core_e = match &sub_e {
            Expr::Collate { expr: inner, .. } => inner.as_ref(),
            other => other,
        };
        let gp = compile_group_proj_expr(core_e, group_cols, scope, aggs)?;
        specs.push(GroupSortSpec {
            proj_or_key: gp,
            desc: ob.desc,
            nulls_last,
            collation: coll_opt.unwrap_or(Collation::Binary),
        });
    }
    Some(specs.into_boxed_slice())
}

#[allow(clippy::too_many_arguments)]
fn try_compile_single_table_core(
    distinct: bool,
    projection: &[ResultColumn],
    tos: &TableOrSubquery,
    selection: Option<&Expr>,
    group_by: &[Expr],
    having: Option<&Expr>,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    outer_json_group_array_col: Option<&str>,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Option<FastReadPlan> {
    if distinct || having.is_some() {
        return None;
    }
    let (db_id, slot_schema, binding, indexed_by) =
        validate_and_resolve_table_tos(tos, selection, db_set, 0)?;
    let sample_cols =
        build_table_scoped_columns_pub(&binding.tbl, &binding.schema_name, &binding.eff_alias);
    let (col_names, expanded_proj) = expand_projection(projection, &sample_cols).ok()?;
    let bindings_arr = [binding];
    let tbl_alias_opt = bindings_arr[0]
        .has_explicit_alias
        .then_some(bindings_arr[0].eff_alias.as_str());
    let slot_cols = build_slot_col_map(&sample_cols, &bindings_arr);
    let scope = FastCompileScope {
        bindings: &bindings_arr,
        slot_cols: &slot_cols,
        functions,
        collations,
    };

    let sub_where =
        selection.map(|w| subst_select_aliases_pub(w, &expanded_proj, &sample_cols, db_set));
    if sub_where.as_ref().is_some_and(expr_contains_aggregate) {
        return None;
    }
    let compiled_where = match sub_where.as_ref() {
        None => None,
        Some(w) => Some(compile_fast_expr(w, &scope)?),
    };

    let tbl = &bindings_arr[0].tbl;
    let lease_roots: Box<[Pgno]> = Box::new([tbl.root_pgno]);

    // Case 1: Ungrouped Aggregate (`W04`, `W09`, `W14` Q1, `SELECT count(*) FROM bench`)
    let any_proj_agg = expanded_proj
        .iter()
        .any(|(e, _, _)| expr_contains_aggregate(e));
    if outer_json_group_array_col.is_none()
        && group_by.is_empty()
        && any_proj_agg
        && order_by.is_empty()
        && limit.is_none()
    {
        let mut aggs = Vec::with_capacity(expanded_proj.len());
        let mut projs = Vec::with_capacity(expanded_proj.len());
        for (pe, _, _) in &expanded_proj {
            projs.push(compile_group_proj_expr(pe, &[], &scope, &mut aggs)?);
        }
        if aggs.is_empty() {
            return None;
        }

        let mut td = build_fast_table_desc(tbl)?;
        let mut tds = [td];
        let mut exact_cols = vec![false; tbl.columns.len()];
        let mut needs_rowid = false;
        if let Some(ref w) = compiled_where {
            w.mark_usage(&mut tds);
            collect_expr_exact_usage(w, &mut exact_cols, &mut needs_rowid);
        }
        for a in &aggs {
            a.mark_usage(&mut tds);
            collect_agg_exact_usage(a, &mut exact_cols, &mut needs_rowid);
        }
        td = tds.into_iter().next().unwrap();

        let is_count_star_only = compiled_where.is_none()
            && aggs.len() == 1
            && matches!(aggs[0], FastAggSpec::CountStar);
        let qctx = QueryPlanContext {
            projection: Some(projection),
            group_by: &[],
            having: None,
            order_by: &[],
            indexed_by: indexed_by.as_ref(),
            is_count_star_only,
        };
        let ap = choose_access_plan_with_context(
            tbl,
            &bindings_arr[0].schema_name,
            tbl_alias_opt,
            sub_where.as_ref(),
            &slot_schema,
            qctx,
        );
        let access = compile_fast_agg_access(&ap, tbl, &td, &exact_cols, needs_rowid)?;
        let int_kernel = match &access {
            // The integer kernel computes on the stored integers; an integer stored for a
            // REAL-affinity column is a real to SQL, so such an index takes the general code.
            FastSingleAccess::CoveringIndexFullScan { cov }
            | FastSingleAccess::CoveringIndexRangeScan { cov, .. }
                if !cov.has_real_col =>
            {
                CovIntKernelShape::build(
                    cov,
                    td.col_buf_len(),
                    compiled_where.as_ref(),
                    None,
                    &aggs,
                )
            }
            _ => None,
        };

        return Some(FastReadPlan {
            db_id,
            column_names: col_names,
            lease_roots,
            kind: FastReadKind::UngroupedAgg(UngroupedAggPlan {
                table: td,
                access,
                where_pred: compiled_where,
                aggs: aggs.into_boxed_slice(),
                projections: projs.into_boxed_slice(),
                int_kernel,
            }),
        });
    }

    // Case 2: Single-Column `GROUP BY` (`W05`, `W10` Q1, `W14` Q3)
    if outer_json_group_array_col.is_none() && group_by.len() == 1 && limit.is_none() {
        let gb_sub = subst_select_aliases_pub(&group_by[0], &expanded_proj, &sample_cols, db_set);
        let (gb_rc, gb_coll) = match &gb_sub {
            Expr::Column(cref) => {
                let rc = scope.resolve_col_ref(cref)?;
                (rc, rc.collation)
            }
            Expr::Collate {
                expr: inner,
                collation,
            } => {
                let Expr::Column(cref) = inner.as_ref() else {
                    return None;
                };
                let rc = scope.resolve_col_ref(cref)?;
                let c = scope.resolve_builtin_collation(collation)?;
                (FastResolvedCol { collation: c, ..rc }, c)
            }
            _ => return None,
        };

        let group_cols = [gb_rc];
        let mut aggs = Vec::with_capacity(expanded_proj.len());
        let mut projs = Vec::with_capacity(expanded_proj.len());
        for (pe, _, _) in &expanded_proj {
            projs.push(compile_group_proj_expr(pe, &group_cols, &scope, &mut aggs)?);
        }
        let sort_specs = compile_group_sort_specs(
            order_by,
            &expanded_proj,
            &sample_cols,
            db_set,
            &group_cols,
            &projs,
            &scope,
            &mut aggs,
        )?;

        let mut td = build_fast_table_desc(tbl)?;
        let mut tds = [td];
        let mut exact_cols = vec![false; tbl.columns.len()];
        let mut needs_rowid = false;
        gb_rc.src.mark_usage(&mut tds);
        mark_col_src_exact(gb_rc.src, &mut exact_cols, &mut needs_rowid);
        if let Some(ref w) = compiled_where {
            w.mark_usage(&mut tds);
            collect_expr_exact_usage(w, &mut exact_cols, &mut needs_rowid);
        }
        for a in &aggs {
            a.mark_usage(&mut tds);
            collect_agg_exact_usage(a, &mut exact_cols, &mut needs_rowid);
        }
        td = tds.into_iter().next().unwrap();

        // Subcase 2a: Ordered Covering-Index `GROUP BY` (`W05`)
        // Check if an allowed covering index has `columns[0]` == `group_col` with matching collation,
        // and `order_by` is either empty or `[group_col ASC/DESC]` with default `nulls_last`.
        let order_matches_group = match sort_specs.as_ref() {
            [] => Some(None),
            [only]
                if matches!(only.proj_or_key, FastGroupProjExpr::GroupKey(0))
                    && only.collation == gb_coll
                    && only.nulls_last == only.desc =>
            {
                Some(Some(only.desc))
            }
            _ => None,
        };

        if let Some(req_desc_opt) = order_matches_group
            && !matches!(indexed_by, Some(IndexHint::NotIndexed))
            && let FastColSrc::RecordCol {
                col_idx: gb_col_idx,
                is_ipk: false,
                ..
            } = gb_rc.src
        {
            for idx in slot_schema.indexes_for_table(&tbl.name) {
                if let Some(IndexHint::IndexedBy(ref forced)) = indexed_by
                    && (idx.is_autoindex || !idx.name.eq_ignore_ascii_case(forced))
                {
                    continue;
                }
                if let Some(ref pw) = idx.where_clause {
                    let ok = sub_where.as_ref().is_some_and(|wh| {
                        expr_implies_partial_where(
                            wh,
                            pw,
                            tbl,
                            &bindings_arr[0].schema_name,
                            tbl_alias_opt,
                        )
                    });
                    if !ok {
                        continue;
                    }
                }
                if idx.columns.is_empty()
                    || idx.columns[0].expr.is_some()
                    || idx.columns[0].col_idx != Some(gb_col_idx as usize)
                    || idx.columns[0].sort_order.custom_collation.is_some()
                    || idx.columns[0].sort_order.collation != gb_coll
                {
                    continue;
                }
                let idx_desc = idx.columns[0].sort_order.desc;
                if idx.columns[0].sort_order.nulls_last != idx_desc {
                    continue;
                }
                if !check_index_covers_all_exprs(tbl, &idx, &exact_cols) {
                    continue;
                }
                let Some(cov) = build_covering_index_desc(tbl, &idx, &td, needs_rowid) else {
                    continue;
                };
                let reverse = match req_desc_opt {
                    None => false,
                    Some(want_desc) => want_desc != idx_desc,
                };
                let int_kernel = if cov.has_real_col {
                    None
                } else {
                    CovIntKernelShape::build(
                        &cov,
                        td.col_buf_len(),
                        compiled_where.as_ref(),
                        Some(gb_rc.src),
                        &aggs,
                    )
                };
                return Some(FastReadPlan {
                    db_id,
                    column_names: col_names,
                    lease_roots,
                    kind: FastReadKind::OrderedCoveringGroupBy(OrderedCoveringGroupByPlan {
                        table: td,
                        cov,
                        reverse,
                        group_col: gb_rc.src,
                        group_collation: gb_coll,
                        where_pred: compiled_where,
                        aggs: aggs.into_boxed_slice(),
                        projections: projs.into_boxed_slice(),
                        int_kernel,
                    }),
                });
            }
        }

        // Subcase 2b: Selective Table-Leaf / Covering Range `HashGroupBy` (`W10` Q1, `W14` Q3)
        let qctx = QueryPlanContext {
            projection: Some(projection),
            group_by,
            having: None,
            order_by,
            indexed_by: indexed_by.as_ref(),
            is_count_star_only: false,
        };
        let ap = choose_access_plan_with_context(
            tbl,
            &bindings_arr[0].schema_name,
            tbl_alias_opt,
            sub_where.as_ref(),
            &slot_schema,
            qctx,
        );
        if let AccessPlan::IndexFullScan {
            index,
            is_covering: false,
        } = &ap
        {
            // The hash plan emits groups in first-seen order. Walking the index is only
            // equivalent to the generic sorter-based grouping when that order is ascending
            // group-key order, i.e. the index leads with the group column itself.
            let leads_with_group_col = index.columns.first().is_some_and(|ic| {
                ic.expr.is_none()
                    && matches!(
                        gb_rc.src,
                        FastColSrc::RecordCol { col_idx, .. }
                            if ic.col_idx == Some(col_idx as usize)
                    )
                    && ic.sort_order.custom_collation.is_none()
                    && ic.sort_order.collation == gb_coll
                    && !ic.sort_order.desc
                    && !ic.sort_order.nulls_last
            });
            if !leads_with_group_col {
                return None;
            }
        }
        let access = compile_fast_agg_access(&ap, tbl, &td, &exact_cols, needs_rowid)?;
        // Without a WHERE clause C satisfies the GROUP BY by walking an index that leads
        // with the group column when there is one; if every such index is DESC the groups
        // come out in descending order.
        let groups_desc = sub_where.is_none() && {
            let mut any = false;
            let mut all_desc = true;
            for idx in slot_schema.indexes_for_table(&tbl.name) {
                if idx.where_clause.is_some() {
                    continue;
                }
                let Some(ic) = idx.columns.first() else {
                    continue;
                };
                if ic.expr.is_none()
                    && matches!(
                        gb_rc.src,
                        FastColSrc::RecordCol { col_idx, .. }
                            if ic.col_idx == Some(col_idx as usize)
                    )
                    && ic.sort_order.custom_collation.is_none()
                    && ic.sort_order.collation == gb_coll
                {
                    any = true;
                    all_desc &= ic.sort_order.desc;
                }
            }
            any && all_desc
        };
        return Some(FastReadPlan {
            db_id,
            column_names: col_names,
            lease_roots,
            kind: FastReadKind::HashGroupBy(HashGroupByPlan {
                table: td,
                access,
                where_pred: compiled_where,
                group_col: gb_rc.src,
                group_collation: gb_coll,
                groups_desc,
                aggs: aggs.into_boxed_slice(),
                projections: projs.into_boxed_slice(),
                order_by: sort_specs,
            }),
        });
    }

    // Case 3: Ordered Index `LIMIT` Select (`W10` Q2, `W14` Q2)
    if group_by.is_empty()
        && !any_proj_agg
        && !order_by.is_empty()
        && !matches!(indexed_by, Some(IndexHint::NotIndexed))
        && let Some(lc) = limit
        && lc.offset.is_none()
        && let Expr::Integer(lim_i64) = lc.limit
        && lim_i64 >= 0
    {
        let lim_usize = usize::try_from(lim_i64).ok()?;
        let out_col_names = match outer_json_group_array_col {
            Some(c) => {
                if expanded_proj.len() != 1 {
                    return None;
                }
                vec![c.to_owned()]
            }
            None => col_names,
        };

        let mut proj_exprs = Vec::with_capacity(expanded_proj.len());
        for (pe, _, _) in &expanded_proj {
            proj_exprs.push(compile_fast_expr(pe, &scope)?);
        }

        // Extract equality conjuncts on columns (`col = rhs`) from `sub_where`
        let mut conj_list = Vec::new();
        if let Some(ref w) = sub_where {
            collect_and_conjuncts_flat(w, &mut conj_list);
        }
        let mut col_eqs: Vec<(usize, FastScalarRhs)> = Vec::new();
        for c in &conj_list {
            if let Expr::Binary {
                left,
                op: BinaryOp::Eq,
                right,
            } = c
            {
                if let Expr::Column(cref) = left.as_ref()
                    && let Some(rc) = scope.resolve_col_ref(cref)
                    && let FastColSrc::RecordCol {
                        col_idx,
                        is_ipk: false,
                        ..
                    } = rc.src
                    && let Some(rhs) = compile_scalar_rhs(right)
                {
                    let eff_aff = sqlite_compare_affinity(rc.affinity, scope.expr_meta(right).0);
                    if matches!(eff_aff, TypeAffinity::Blob)
                        || eff_aff == tbl.columns[col_idx as usize].affinity
                        || (matches!(
                            tbl.columns[col_idx as usize].affinity,
                            TypeAffinity::Integer | TypeAffinity::Numeric | TypeAffinity::Real
                        ) && matches!(
                            eff_aff,
                            TypeAffinity::Integer | TypeAffinity::Numeric | TypeAffinity::Real
                        ))
                    {
                        col_eqs.push((col_idx as usize, rhs));
                    }
                } else if let Expr::Column(cref) = right.as_ref()
                    && let Some(rc) = scope.resolve_col_ref(cref)
                    && let FastColSrc::RecordCol {
                        col_idx,
                        is_ipk: false,
                        ..
                    } = rc.src
                    && let Some(lhs) = compile_scalar_rhs(left)
                {
                    let eff_aff = sqlite_compare_affinity(rc.affinity, scope.expr_meta(left).0);
                    if matches!(eff_aff, TypeAffinity::Blob)
                        || eff_aff == tbl.columns[col_idx as usize].affinity
                        || (matches!(
                            tbl.columns[col_idx as usize].affinity,
                            TypeAffinity::Integer | TypeAffinity::Numeric | TypeAffinity::Real
                        ) && matches!(
                            eff_aff,
                            TypeAffinity::Integer | TypeAffinity::Numeric | TypeAffinity::Real
                        ))
                    {
                        col_eqs.push((col_idx as usize, lhs));
                    }
                }
            }
        }

        // Resolve `order_by` columns:
        let mut ob_cols: Vec<(FastResolvedCol, bool, bool)> = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let sub_ob = subst_select_aliases_pub(&ob.expr, &expanded_proj, &sample_cols, db_set);
            let Expr::Column(cref) = &sub_ob else {
                return None;
            };
            let rc = scope.resolve_col_ref(cref)?;
            let nl = ob.nulls_last.unwrap_or(ob.desc);
            if nl != ob.desc {
                return None;
            }
            ob_cols.push((rc, ob.desc, nl));
        }

        let mut td = build_fast_table_desc(tbl)?;
        let mut tds = [td];
        if let Some(ref w) = compiled_where {
            w.mark_usage(&mut tds);
        }
        for p in &proj_exprs {
            p.mark_usage(&mut tds);
        }
        td = tds.into_iter().next().unwrap();

        for idx in slot_schema.indexes_for_table(&tbl.name) {
            if index_has_custom_collation(&idx) || idx.columns.len() > 15 {
                continue;
            }
            if let Some(IndexHint::IndexedBy(ref forced)) = indexed_by
                && (idx.is_autoindex || !idx.name.eq_ignore_ascii_case(forced))
            {
                continue;
            }
            if let Some(ref pw) = idx.where_clause {
                let ok = sub_where.as_ref().is_some_and(|wh| {
                    expr_implies_partial_where(
                        wh,
                        pw,
                        tbl,
                        &bindings_arr[0].schema_name,
                        tbl_alias_opt,
                    )
                });
                if !ok {
                    continue;
                }
            }

            // Match maximal equality prefix `0..n_eq`:
            let mut prefix_keys = Vec::new();
            let mut prefix_affs = Vec::new();
            for ic in &idx.columns {
                if ic.expr.is_some() {
                    break;
                }
                let Some(ci) = ic.col_idx else {
                    break;
                };
                if ic.sort_order.collation != tbl.columns[ci].collation {
                    break;
                }
                if let Some((_, rhs)) = col_eqs.iter().find(|(c, _)| *c == ci) {
                    prefix_keys.push(rhs.clone());
                    prefix_affs.push(tbl.columns[ci].affinity);
                } else {
                    break;
                }
            }
            if prefix_keys.is_empty() {
                continue;
            }
            let n_eq = prefix_keys.len();

            // Check Mode A (`ExactStream`): `ob_cols` len == 1 and matches either `idx.columns[n_eq]`
            // OR trailing `rowid`/`ipk` (`W10` Q2: `idx_jdocs_bucket(bucket, id)` where `id` is IPK!).
            if ob_cols.len() == 1 {
                let (ob_rc, ob_desc, _) = ob_cols[0];
                let exact_dir: Option<bool> = if n_eq < idx.columns.len() {
                    let ic = &idx.columns[n_eq];
                    if ic.expr.is_none()
                        && ic.sort_order.nulls_last == ic.sort_order.desc
                        && ic.sort_order.collation == ob_rc.collation
                    {
                        let col_matches = match ob_rc.src {
                            FastColSrc::Rowid { .. } => {
                                ic.col_idx.is_some_and(|ci| tbl.rowid_alias_col == Some(ci))
                            }
                            FastColSrc::RecordCol {
                                col_idx, is_ipk, ..
                            } => {
                                ic.col_idx == Some(col_idx as usize)
                                    || (is_ipk
                                        && ic
                                            .col_idx
                                            .is_some_and(|ci| tbl.rowid_alias_col == Some(ci)))
                            }
                        };
                        col_matches.then_some(ob_desc != ic.sort_order.desc)
                    } else {
                        None
                    }
                } else if matches!(
                    ob_rc.src,
                    FastColSrc::Rowid { .. } | FastColSrc::RecordCol { is_ipk: true, .. }
                ) {
                    Some(ob_desc)
                } else {
                    None
                };

                if let Some(reverse) = exact_dir {
                    return Some(FastReadPlan {
                        db_id,
                        column_names: out_col_names,
                        lease_roots,
                        kind: FastReadKind::OrderedIndexSelect(OrderedIndexSelectPlan {
                            table: td,
                            index_root: idx.root_pgno,
                            index_orders: idx.key_sort_orders().into_boxed_slice(),
                            n_index_cols: idx.columns.len(),
                            prefix_keys: prefix_keys.into_boxed_slice(),
                            prefix_affinities: prefix_affs.into_boxed_slice(),
                            extra_where: compiled_where,
                            projections: proj_exprs.into_boxed_slice(),
                            limit: lim_usize,
                            mode: OrderedIndexMode::ExactStream {
                                reverse,
                                outer_json_group_array: outer_json_group_array_col.is_some(),
                            },
                        }),
                    });
                }
            }

            // Check Mode B (`ReverseTieBreakRowidAsc`, `W14` Q2):
            // `WHERE <only eq_prefix conjuncts> ORDER BY sort_col DESC, rowid_or_ipk ASC LIMIT K`
            if outer_json_group_array_col.is_none()
                && conj_list.len() == n_eq
                && ob_cols.len() == 2
                && n_eq < idx.columns.len()
            {
                let (ob0_rc, ob0_desc, _) = ob_cols[0];
                let (ob1_rc, ob1_desc, _) = ob_cols[1];
                let ic = &idx.columns[n_eq];
                let ob1_is_rowid = matches!(
                    ob1_rc.src,
                    FastColSrc::Rowid { .. } | FastColSrc::RecordCol { is_ipk: true, .. }
                );
                if ob0_desc
                    && !ob1_desc
                    && ob1_is_rowid
                    && ic.expr.is_none()
                    && !ic.sort_order.desc
                    && !ic.sort_order.nulls_last
                    && ic.sort_order.collation == ob0_rc.collation
                    && let FastColSrc::RecordCol {
                        col_idx,
                        is_ipk: false,
                        ..
                    } = ob0_rc.src
                    && ic.col_idx == Some(col_idx as usize)
                {
                    return Some(FastReadPlan {
                        db_id,
                        column_names: out_col_names,
                        lease_roots,
                        kind: FastReadKind::OrderedIndexSelect(OrderedIndexSelectPlan {
                            table: td,
                            index_root: idx.root_pgno,
                            index_orders: idx.key_sort_orders().into_boxed_slice(),
                            n_index_cols: idx.columns.len(),
                            prefix_keys: prefix_keys.into_boxed_slice(),
                            prefix_affinities: prefix_affs.into_boxed_slice(),
                            extra_where: None,
                            projections: proj_exprs.into_boxed_slice(),
                            limit: lim_usize,
                            mode: OrderedIndexMode::ReverseTieBreakRowidAsc {
                                sort_idx_pos: n_eq,
                                sort_collation: ob0_rc.collation,
                            },
                        }),
                    });
                }
            }
        }
    }

    None
}

fn collect_and_conjuncts_flat<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        collect_and_conjuncts_flat(left, out);
        collect_and_conjuncts_flat(right, out);
    } else {
        out.push(expr);
    }
}

#[allow(clippy::too_many_arguments)]
fn try_compile_two_table_join_core(
    distinct: bool,
    projection: &[ResultColumn],
    fc: &FromClause,
    selection: Option<&Expr>,
    group_by: &[Expr],
    having: Option<&Expr>,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Option<FastReadPlan> {
    if distinct || having.is_some() || limit.is_some() || fc.joins.len() != 1 {
        return None;
    }
    let join = &fc.joins[0];
    if join.natural || join.join_type != JoinType::Inner {
        return None;
    }
    let Some(JoinConstraint::On(ref on_expr)) = join.constraint else {
        return None;
    };

    let (db_l, schema_l, mut bind_l, hint_l) =
        validate_and_resolve_table_tos(&fc.head, selection, db_set, 0)?;
    let (db_r, _schema_r, mut bind_r, hint_r) =
        validate_and_resolve_table_tos(&join.right, selection, db_set, 1)?;
    if db_l != db_r {
        return None;
    }
    if bind_l.eff_alias.eq_ignore_ascii_case(&bind_r.eff_alias) {
        return None;
    }

    // Build `sample_cols` in `FROM left JOIN right` order so `SELECT *` & alias substitution
    // preserve exact column order:
    let mut sample_cols =
        build_table_scoped_columns_pub(&bind_l.tbl, &bind_l.schema_name, &bind_l.eff_alias);
    sample_cols.extend(build_table_scoped_columns_pub(
        &bind_r.tbl,
        &bind_r.schema_name,
        &bind_r.eff_alias,
    ));
    let (col_names, expanded_proj) = expand_projection(projection, &sample_cols).ok()?;

    // Temporary `[bind_l(0), bind_r(1)]` scope to inspect `ON` + `WHERE` conjuncts:
    let init_bindings = [bind_l.clone(), bind_r.clone()];
    let init_slots = build_slot_col_map(&sample_cols, &init_bindings);
    let init_scope = FastCompileScope {
        bindings: &init_bindings,
        slot_cols: &init_slots,
        functions,
        collations,
    };

    let sub_where =
        selection.map(|w| subst_select_aliases_pub(w, &expanded_proj, &sample_cols, db_set));
    if sub_where.as_ref().is_some_and(expr_contains_aggregate) || expr_contains_aggregate(on_expr) {
        return None;
    }

    // Collect all `ON` + `WHERE` conjuncts (valid for `INNER JOIN`):
    let mut all_conjuncts: Vec<Expr> = Vec::new();
    let mut tmp_on = Vec::new();
    collect_and_conjuncts_flat(on_expr, &mut tmp_on);
    for c in tmp_on {
        all_conjuncts.push(c.clone());
    }
    if let Some(ref sw) = sub_where {
        let mut tmp_w = Vec::new();
        collect_and_conjuncts_flat(sw, &mut tmp_w);
        for c in tmp_w {
            all_conjuncts.push(c.clone());
        }
    }

    // Find an equi-join conjunct `col_0 = col_1` where both columns have `TEXT` affinity and `BINARY` collation:
    let mut join_pair: Option<(usize, FastResolvedCol, FastResolvedCol)> = None;
    for (ci, conj) in all_conjuncts.iter().enumerate() {
        if let Expr::Binary {
            left,
            op: BinaryOp::Eq,
            right,
        } = conj
            && let (Expr::Column(cl), Expr::Column(cr)) = (left.as_ref(), right.as_ref())
            && let (Some(rl), Some(rr)) = (
                init_scope.resolve_col_ref(cl),
                init_scope.resolve_col_ref(cr),
            )
        {
            let side_l = match rl.src {
                FastColSrc::Rowid { side } | FastColSrc::RecordCol { side, .. } => side,
            };
            let side_r = match rr.src {
                FastColSrc::Rowid { side } | FastColSrc::RecordCol { side, .. } => side,
            };
            if side_l != side_r
                && rl.affinity == Some(TypeAffinity::Text)
                && rr.affinity == Some(TypeAffinity::Text)
                && rl.collation == Collation::Binary
                && rr.collation == Collation::Binary
            {
                let (rc0, rc1) = if side_l == 0 { (rl, rr) } else { (rr, rl) };
                join_pair = Some((ci, rc0, rc1));
                break;
            }
        }
    }
    let (join_conj_idx, eq_col_l, eq_col_r) = join_pair?;

    let any_proj_agg = expanded_proj
        .iter()
        .any(|(e, _, _)| expr_contains_aggregate(e));
    let lease_roots: Box<[Pgno]> = if bind_l.tbl.root_pgno == bind_r.tbl.root_pgno {
        Box::new([bind_l.tbl.root_pgno])
    } else {
        Box::new([bind_l.tbl.root_pgno, bind_r.tbl.root_pgno])
    };

    // ========================================================================
    // Join Case A: Equi-Join + Single-Column `GROUP BY` (`W06`)
    // ========================================================================
    if group_by.len() == 1 {
        // Partition non-join conjuncts by referenced side (`only 0`, `only 1`, `both/none`):
        let mut conj_l_ast: Vec<Expr> = Vec::new();
        let mut conj_r_ast: Vec<Expr> = Vec::new();
        let mut conj_post_ast: Vec<Expr> = Vec::new();

        for (i, c) in all_conjuncts.iter().enumerate() {
            if i == join_conj_idx {
                continue;
            }
            let fe = compile_fast_expr(c, &init_scope)?;
            let mut tds = [
                build_fast_table_desc(&bind_l.tbl)?,
                build_fast_table_desc(&bind_r.tbl)?,
            ];
            let mut u0 = false;
            let mut u1 = false;
            expr_sides_used(&fe, &mut u0, &mut u1);
            let _ = &mut tds;
            match (u0, u1) {
                (true, false) => conj_l_ast.push(c.clone()),
                (false, true) => conj_r_ast.push(c.clone()),
                _ => conj_post_ast.push(c.clone()),
            }
        }

        let where_l = combine_and_owned(&conj_l_ast);
        let where_r = combine_and_owned(&conj_r_ast);
        let alias_l_opt = bind_l
            .has_explicit_alias
            .then_some(bind_l.eff_alias.as_str());
        let alias_r_opt = bind_r
            .has_explicit_alias
            .then_some(bind_r.eff_alias.as_str());

        let ap_l = choose_access_plan_with_context(
            &bind_l.tbl,
            &bind_l.schema_name,
            alias_l_opt,
            where_l.as_ref(),
            &schema_l,
            QueryPlanContext {
                indexed_by: hint_l.as_ref(),
                ..Default::default()
            },
        );
        let ap_r = choose_access_plan_with_context(
            &bind_r.tbl,
            &bind_r.schema_name,
            alias_r_opt,
            where_r.as_ref(),
            &schema_l,
            QueryPlanContext {
                indexed_by: hint_r.as_ref(),
                ..Default::default()
            },
        );

        // Choose smaller/filtered side as `build` (`side 0`) and larger/unfiltered side as `probe` (`side 1`):
        let l_is_selective = !matches!(
            ap_l,
            AccessPlan::FullScan | AccessPlan::IndexFullScan { .. }
        );
        let r_is_selective = !matches!(
            ap_r,
            AccessPlan::FullScan | AccessPlan::IndexFullScan { .. }
        );
        let swap_sides = if l_is_selective != r_is_selective {
            r_is_selective
        } else {
            bind_r.tbl.stat1.n_row_log_est < bind_l.tbl.stat1.n_row_log_est
        };

        let (b_bind, p_bind, b_ap, p_ap, b_where_ast, p_where_ast) = if !swap_sides {
            bind_l.side = 0;
            bind_r.side = 1;
            (bind_l, bind_r, ap_l, ap_r, where_l, where_r)
        } else {
            bind_r.side = 0;
            bind_l.side = 1;
            (bind_r, bind_l, ap_r, ap_l, where_r, where_l)
        };
        let post_where_ast = combine_and_owned(&conj_post_ast);

        let final_bindings = [b_bind, p_bind];
        let final_slots = build_slot_col_map(&sample_cols, &final_bindings);
        let scope = FastCompileScope {
            bindings: &final_bindings,
            slot_cols: &final_slots,
            functions,
            collations,
        };

        // Re-resolve join keys under `final_bindings`:
        let Expr::Binary {
            left: jl,
            right: jr,
            ..
        } = &all_conjuncts[join_conj_idx]
        else {
            unreachable!();
        };
        let (Expr::Column(jcl), Expr::Column(jcr)) = (jl.as_ref(), jr.as_ref()) else {
            unreachable!();
        };
        let jrl = scope.resolve_col_ref(jcl)?;
        let jrr = scope.resolve_col_ref(jcr)?;
        let (build_jk, probe_jk) = match jrl.src {
            FastColSrc::Rowid { side: 0 } | FastColSrc::RecordCol { side: 0, .. } => {
                (jrl.src, jrr.src)
            }
            _ => (jrr.src, jrl.src),
        };

        let gb_sub = subst_select_aliases_pub(&group_by[0], &expanded_proj, &sample_cols, db_set);
        let (gb_rc, gb_coll) = match &gb_sub {
            Expr::Column(cref) => {
                let rc = scope.resolve_col_ref(cref)?;
                (rc, rc.collation)
            }
            Expr::Collate {
                expr: inner,
                collation,
            } => {
                let Expr::Column(cref) = inner.as_ref() else {
                    return None;
                };
                let rc = scope.resolve_col_ref(cref)?;
                let c = scope.resolve_builtin_collation(collation)?;
                (FastResolvedCol { collation: c, ..rc }, c)
            }
            _ => return None,
        };

        let group_cols = [gb_rc];
        let mut aggs = Vec::with_capacity(expanded_proj.len());
        let mut projs = Vec::with_capacity(expanded_proj.len());
        for (pe, _, _) in &expanded_proj {
            projs.push(compile_group_proj_expr(pe, &group_cols, &scope, &mut aggs)?);
        }
        let sort_specs = compile_group_sort_specs(
            order_by,
            &expanded_proj,
            &sample_cols,
            db_set,
            &group_cols,
            &projs,
            &scope,
            &mut aggs,
        )?;

        let b_where = match b_where_ast.as_ref() {
            None => None,
            Some(w) => Some(compile_fast_expr(w, &scope)?),
        };
        let p_where = match p_where_ast.as_ref() {
            None => None,
            Some(w) => Some(compile_fast_expr(w, &scope)?),
        };
        let post_where = match post_where_ast.as_ref() {
            None => None,
            Some(w) => Some(compile_fast_expr(w, &scope)?),
        };

        let mut tds = [
            build_fast_table_desc(&final_bindings[0].tbl)?,
            build_fast_table_desc(&final_bindings[1].tbl)?,
        ];
        build_jk.mark_usage(&mut tds);
        probe_jk.mark_usage(&mut tds);
        gb_rc.src.mark_usage(&mut tds);
        if let Some(ref w) = b_where {
            w.mark_usage(&mut tds);
        }
        if let Some(ref w) = p_where {
            w.mark_usage(&mut tds);
        }
        if let Some(ref w) = post_where {
            w.mark_usage(&mut tds);
        }
        for a in &aggs {
            a.mark_usage(&mut tds);
        }

        // Collect which non-IPK record columns of `build` (`side 0`) are needed AFTER the build phase
        // (in `post_where`, `group_col`, and `aggs`):
        let mut kept_set: Vec<u16> = Vec::new();
        let mut mark_kept = |src: FastColSrc| {
            if let FastColSrc::RecordCol {
                side: 0,
                col_idx,
                is_ipk: false,
            } = src
                && !kept_set.contains(&col_idx)
            {
                kept_set.push(col_idx);
            }
        };
        mark_kept(gb_rc.src);
        if let Some(ref w) = post_where {
            collect_side0_cols_expr(w, &mut mark_kept);
        }
        for a in &aggs {
            match a {
                FastAggSpec::CountStar => {}
                FastAggSpec::Count(e)
                | FastAggSpec::Sum(e)
                | FastAggSpec::Total(e)
                | FastAggSpec::Avg(e)
                | FastAggSpec::Min(e, _)
                | FastAggSpec::Max(e, _)
                | FastAggSpec::JsonGroupArray(e) => collect_side0_cols_expr(e, &mut mark_kept),
            }
        }

        let b_exact = collect_col_flags_from_desc(&tds[0], final_bindings[0].tbl.columns.len());
        let p_exact = collect_col_flags_from_desc(&tds[1], final_bindings[1].tbl.columns.len());
        let b_access =
            compile_fast_single_access(&b_ap, &final_bindings[0].tbl, &tds[0], &b_exact, true)?;
        let p_access =
            compile_fast_single_access(&p_ap, &final_bindings[1].tbl, &tds[1], &p_exact, true)?;

        let mut td_iter = tds.into_iter();
        let build_td = td_iter.next().unwrap();
        let probe_td = td_iter.next().unwrap();

        return Some(FastReadPlan {
            db_id: db_l,
            column_names: col_names,
            lease_roots,
            kind: FastReadKind::EquiJoinGroupBy(Box::new(EquiJoinGroupByPlan {
                build_table: build_td,
                build_access: b_access,
                build_where: b_where,
                build_join_key: build_jk,
                build_kept_cols: kept_set.into_boxed_slice(),
                probe_table: probe_td,
                probe_access: p_access,
                probe_where: p_where,
                probe_join_key: probe_jk,
                post_join_where: post_where,
                group_col: gb_rc.src,
                group_collation: gb_coll,
                groups_desc: false,
                aggs: aggs.into_boxed_slice(),
                projections: projs.into_boxed_slice(),
                order_by: sort_specs,
            })),
        });
    }

    // ========================================================================
    // Join Case B: Equi-Join + Projection + `ORDER BY` (`uuid-query/join-*`)
    // ========================================================================
    if group_by.is_empty() && !any_proj_agg {
        // Try `(detail = left, parent = right)` then `(detail = right, parent = left)`:
        for parent_is_right in [true, false] {
            let (mut d_bind, mut p_bind, d_hint, p_hint, d_eq_orig, p_eq_orig) = if parent_is_right
            {
                (
                    bind_l.clone(),
                    bind_r.clone(),
                    hint_l.clone(),
                    hint_r.clone(),
                    eq_col_l,
                    eq_col_r,
                )
            } else {
                (
                    bind_r.clone(),
                    bind_l.clone(),
                    hint_r.clone(),
                    hint_l.clone(),
                    eq_col_r,
                    eq_col_l,
                )
            };
            let FastColSrc::RecordCol {
                col_idx: p_join_ci,
                is_ipk: false,
                ..
            } = p_eq_orig.src
            else {
                continue;
            };
            let FastColSrc::RecordCol {
                col_idx: d_join_ci,
                is_ipk: false,
                ..
            } = d_eq_orig.src
            else {
                continue;
            };

            // Verify `p` has a single-column `UNIQUE` non-partial `BINARY` index on `p_join_ci`:
            let Some(u_idx) = schema_l
                .indexes_for_table(&p_bind.tbl.name)
                .into_iter()
                .find(|idx| {
                    idx.unique
                        && idx.where_clause.is_none()
                        && idx.columns.len() == 1
                        && idx.columns[0].expr.is_none()
                        && idx.columns[0].col_idx == Some(p_join_ci as usize)
                        && idx.columns[0].sort_order.collation == Collation::Binary
                        && idx.columns[0].sort_order.custom_collation.is_none()
                })
            else {
                continue;
            };

            let parent_scan = match p_hint {
                None => ParentKeyScan::UniqueIndex {
                    root_pgno: u_idx.root_pgno,
                },
                Some(IndexHint::NotIndexed) => ParentKeyScan::TableScan { col_idx: p_join_ci },
                Some(IndexHint::IndexedBy(ref forced_p)) => {
                    if !u_idx.is_autoindex && u_idx.name.eq_ignore_ascii_case(forced_p) {
                        ParentKeyScan::UniqueIndex {
                            root_pgno: u_idx.root_pgno,
                        }
                    } else {
                        continue;
                    }
                }
            };

            d_bind.side = 0;
            p_bind.side = 1;
            let final_bindings = [d_bind, p_bind];
            let final_slots = build_slot_col_map(&sample_cols, &final_bindings);
            let scope = FastCompileScope {
                bindings: &final_bindings,
                slot_cols: &final_slots,
                functions,
                collations,
            };

            // Compile remaining `ON`/`WHERE` conjuncts:
            let mut rem_asts: Vec<Expr> = Vec::new();
            for (i, c) in all_conjuncts.iter().enumerate() {
                if i != join_conj_idx {
                    rem_asts.push(c.clone());
                }
            }
            let rem_where_ast = combine_and_owned(&rem_asts);
            let Some(rem_where) = (match rem_where_ast.as_ref() {
                None => Some(None),
                Some(w) => compile_fast_expr(w, &scope).map(Some),
            }) else {
                continue;
            };

            let mut proj_exprs = Vec::with_capacity(expanded_proj.len());
            let mut proj_ok = true;
            for (pe, _, _) in &expanded_proj {
                if let Some(fe) = compile_fast_expr(pe, &scope) {
                    proj_exprs.push(fe);
                } else {
                    proj_ok = false;
                    break;
                }
            }
            if !proj_ok {
                continue;
            }

            let mut order_terms = Vec::with_capacity(order_by.len());
            let mut ob_ok = true;
            for ob in order_by {
                let nulls_last = ob.nulls_last.unwrap_or(ob.desc);
                if let Expr::Integer(ord) = ob.expr
                    && ord >= 1
                    && (ord as usize) <= proj_exprs.len()
                {
                    let idx = (ord as usize) - 1;
                    let (_, coll_opt) = scope.expr_meta(&expanded_proj[idx].0);
                    if matches!(&expanded_proj[idx].0, Expr::Collate { .. }) && coll_opt.is_none() {
                        ob_ok = false;
                        break;
                    }
                    order_terms.push(JoinOrderTerm {
                        expr: proj_exprs[idx].clone(),
                        desc: ob.desc,
                        nulls_last,
                        collation: coll_opt.unwrap_or(Collation::Binary),
                    });
                    continue;
                }
                let sub_ob =
                    subst_select_aliases_pub(&ob.expr, &expanded_proj, &sample_cols, db_set);
                let (_, coll_opt) = scope.expr_meta(&sub_ob);
                if matches!(&sub_ob, Expr::Collate { .. }) && coll_opt.is_none() {
                    ob_ok = false;
                    break;
                }
                let Some(fe) = compile_fast_expr(&sub_ob, &scope) else {
                    ob_ok = false;
                    break;
                };
                order_terms.push(JoinOrderTerm {
                    expr: fe,
                    desc: ob.desc,
                    nulls_last,
                    collation: coll_opt.unwrap_or(Collation::Binary),
                });
            }
            if !ob_ok {
                continue;
            }

            // Ensure ONLY `RecordCol { side: 1, col_idx: p_join_ci, is_ipk: false }` is ever referenced on `parent` (`side 1`):
            let mut bad_p_col = false;
            let mut check_p = |src: FastColSrc| {
                if match src {
                    FastColSrc::Rowid { side: 1 } => true,
                    FastColSrc::RecordCol {
                        side: 1,
                        col_idx,
                        is_ipk,
                    } => is_ipk || col_idx != p_join_ci,
                    _ => false,
                } {
                    bad_p_col = true;
                }
            };
            if let Some(ref w) = rem_where {
                visit_all_col_srcs(w, &mut check_p);
            }
            for p in &proj_exprs {
                visit_all_col_srcs(p, &mut check_p);
            }
            for ob in &order_terms {
                visit_all_col_srcs(&ob.expr, &mut check_p);
            }
            if bad_p_col {
                continue;
            }

            // Determine `detail_scan` respecting `d_hint`:
            let detail_scan = match d_hint {
                Some(IndexHint::NotIndexed) => DetailScanMode::TableScan,
                None => {
                    // Only use full `TableScan` when `detail` has no selective indexable `WHERE` filter:
                    if rem_where.is_some() {
                        let d_alias = final_bindings[0]
                            .has_explicit_alias
                            .then_some(final_bindings[0].eff_alias.as_str());
                        let d_ap = choose_access_plan_with_context(
                            &final_bindings[0].tbl,
                            &final_bindings[0].schema_name,
                            d_alias,
                            rem_where_ast.as_ref(),
                            &schema_l,
                            QueryPlanContext::default(),
                        );
                        if !matches!(
                            d_ap,
                            AccessPlan::FullScan | AccessPlan::IndexFullScan { .. }
                        ) {
                            continue;
                        }
                    }
                    DetailScanMode::TableScan
                }
                Some(IndexHint::IndexedBy(ref forced_d)) => {
                    let Some(d_idx) = schema_l
                        .indexes_for_table(&final_bindings[0].tbl.name)
                        .into_iter()
                        .find(|i| !i.is_autoindex && i.name.eq_ignore_ascii_case(forced_d))
                    else {
                        continue;
                    };
                    if d_idx.columns.len() > 15 || index_has_custom_collation(&d_idx) {
                        continue;
                    }
                    let join_pos = d_idx
                        .columns
                        .iter()
                        .position(|ic| ic.expr.is_none() && ic.col_idx == Some(d_join_ci as usize));
                    DetailScanMode::IndexDriven {
                        index_root: d_idx.root_pgno,
                        n_index_cols: d_idx.columns.len(),
                        join_col_in_idx: join_pos,
                    }
                }
            };

            let d_join_src = FastColSrc::RecordCol {
                side: 0,
                col_idx: d_join_ci,
                is_ipk: false,
            };
            let Some(d_td_init) = build_fast_table_desc(&final_bindings[0].tbl) else {
                continue;
            };
            let Some(p_td_init) = build_fast_table_desc(&final_bindings[1].tbl) else {
                continue;
            };
            let mut tds = [d_td_init, p_td_init];
            d_join_src.mark_usage(&mut tds);
            if let Some(ref w) = rem_where {
                w.mark_usage(&mut tds);
            }
            for p in &proj_exprs {
                p.mark_usage(&mut tds);
            }
            for ob in &order_terms {
                ob.expr.mark_usage(&mut tds);
            }
            let detail_td = tds.into_iter().next().unwrap();

            let fast_single_text_sort_col = if proj_exprs.len() == 1
                && matches!(proj_exprs[0], FastExpr::ConcatChain { .. })
                && order_terms.len() == 1
                && order_terms[0].collation == Collation::Binary
                && order_terms[0].nulls_last == order_terms[0].desc
                && let FastExpr::Col(
                    sc @ FastColSrc::RecordCol {
                        side: 0,
                        col_idx: sort_ci,
                        is_ipk: false,
                    },
                ) = order_terms[0].expr
                && final_bindings[0].tbl.columns[sort_ci as usize]
                    .not_null
                    .is_some()
                && final_bindings[0].tbl.columns[sort_ci as usize].affinity == TypeAffinity::Text
            {
                Some((sc, order_terms[0].desc))
            } else {
                None
            };

            return Some(FastReadPlan {
                db_id: db_l,
                column_names: col_names,
                lease_roots,
                kind: FastReadKind::EquiJoinProject(EquiJoinProjectPlan {
                    detail_table: detail_td,
                    detail_scan,
                    detail_join_col: d_join_src,
                    parent_root_pgno: final_bindings[1].tbl.root_pgno,
                    parent_scan,
                    parent_join_col_idx: p_join_ci,
                    where_pred: rem_where,
                    projections: proj_exprs.into_boxed_slice(),
                    order_by: order_terms.into_boxed_slice(),
                    fast_single_text_sort_col,
                }),
            });
        }
    }

    None
}

fn combine_and_owned(exprs: &[Expr]) -> Option<Expr> {
    let mut acc: Option<Expr> = None;
    for e in exprs {
        acc = Some(match acc {
            None => e.clone(),
            Some(prev) => Expr::Binary {
                left: Box::new(prev),
                op: BinaryOp::And,
                right: Box::new(e.clone()),
            },
        });
    }
    acc
}

fn visit_all_col_srcs<F: FnMut(FastColSrc)>(expr: &FastExpr, f: &mut F) {
    match expr {
        FastExpr::Const(_) | FastExpr::Param(_) => {}
        FastExpr::Col(c)
        | FastExpr::ColCmpRhs { col: c, .. }
        | FastExpr::ColBetweenRhs { col: c, .. }
        | FastExpr::JsonArrowCmpRhs { col: c, .. }
        | FastExpr::ColLike { col: c, .. }
        | FastExpr::JsonArrow { col: c, .. } => f(*c),
        FastExpr::ArithCols { left, right, .. } => {
            f(*left);
            f(*right);
        }
        FastExpr::UnaryPlus(e)
        | FastExpr::Negate(e)
        | FastExpr::Not(e)
        | FastExpr::IsNull { inner: e, .. }
        | FastExpr::Cast { inner: e, .. }
        | FastExpr::Hex(e)
        | FastExpr::LowerHex(e)
        | FastExpr::UpperHex(e)
        | FastExpr::LowerAscii(e)
        | FastExpr::UpperAscii(e) => visit_all_col_srcs(e, f),
        FastExpr::Arith { left, right, .. }
        | FastExpr::Compare { left, right, .. }
        | FastExpr::And(left, right)
        | FastExpr::Or(left, right) => {
            visit_all_col_srcs(left, f);
            visit_all_col_srcs(right, f);
        }
        FastExpr::Coalesce(es) => {
            for e in es.iter() {
                visit_all_col_srcs(e, f);
            }
        }
        FastExpr::ConcatChain { parts, .. } => {
            for p in parts.iter() {
                match p {
                    FastConcatPart::Literal(_) => {}
                    FastConcatPart::Col(c)
                    | FastConcatPart::CastTextCol(c)
                    | FastConcatPart::LowerHexCol(c)
                    | FastConcatPart::UpperHexCol(c) => f(*c),
                    FastConcatPart::Expr(e) => visit_all_col_srcs(e, f),
                }
            }
        }
    }
}

fn expr_sides_used(expr: &FastExpr, u0: &mut bool, u1: &mut bool) {
    visit_all_col_srcs(expr, &mut |src| match src {
        FastColSrc::Rowid { side: 0 } | FastColSrc::RecordCol { side: 0, .. } => *u0 = true,
        _ => *u1 = true,
    });
}

fn collect_side0_cols_expr<F: FnMut(FastColSrc)>(expr: &FastExpr, f: &mut F) {
    visit_all_col_srcs(expr, f);
}

/// Attempts to pre-compile a top-level `SELECT` statement into a zero-allocation [`FastReadPlan`].
pub fn try_compile_fast_read_plan(
    stmt: &SelectStmt,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Option<FastReadPlan> {
    if !stmt.ctes.is_empty() || !stmt.body.compounds.is_empty() {
        return None;
    }
    let SelectCore::Select {
        distinct,
        ref projection,
        from: Some(ref fc),
        ref selection,
        ref group_by,
        ref having,
        ref windows,
    } = stmt.body.head
    else {
        return None;
    };
    if !windows.is_empty() || core_has_window(&stmt.body.head, &stmt.order_by) {
        return None;
    }

    // Check outer `SELECT json_group_array(col) FROM (<inner ordered LIMIT subquery>)` (`W10` Q2):
    if !distinct
        && fc.joins.is_empty()
        && selection.is_none()
        && group_by.is_empty()
        && having.is_none()
        && stmt.order_by.is_empty()
        && stmt.limit.is_none()
        && projection.len() == 1
        && let TableOrSubquery::Subquery {
            query: ref inner_q,
            alias: ref sub_alias,
        } = fc.head
        && inner_q.ctes.is_empty()
        && inner_q.body.compounds.is_empty()
        && let SelectCore::Select {
            distinct: in_dist,
            projection: ref in_proj,
            from: Some(ref in_fc),
            selection: ref in_sel,
            group_by: ref in_gb,
            having: ref in_hav,
            windows: ref in_win,
        } = inner_q.body.head
        && in_fc.joins.is_empty()
        && in_win.is_empty()
        && !core_has_window(&inner_q.body.head, &inner_q.order_by)
        && let ResultColumn::Expr {
            expr: ref out_e,
            alias: ref out_alias,
            span_text: ref out_span,
        } = projection[0]
        && let Expr::FunctionCall {
            name: ref fn_name,
            distinct: false,
            args: FunctionArgs::Exprs(ref out_args),
            filter: None,
            over: None,
        } = *out_e
        && fn_name.eq_ignore_ascii_case("json_group_array")
        && out_args.len() == 1
        && functions.find_scalar("json_group_array", 1).is_none()
        && functions.find_aggregate("json_group_array", 1).is_none()
        && let Expr::Column(ref out_cref) = out_args[0]
        && out_cref.schema.is_none()
        && out_cref.table.as_deref().is_none_or(|tq| {
            sub_alias
                .as_deref()
                .is_some_and(|sa| sa.eq_ignore_ascii_case(tq))
        })
    {
        // Verify `out_cref.column` matches the single inner projected column name:
        if let TableOrSubquery::Table {
            ref name,
            table_args: None,
            ref alias,
            ..
        } = in_fc.head
            && let Ok(Some((in_db, ResolvedRelation::Table(in_tbl)))) =
                db_set.find_relation_in_set(name)
            && let Some(in_slot) = db_set.slot(in_db)
        {
            let eff_a = alias.clone().unwrap_or_else(|| name.name.clone());
            let in_scols = build_table_scoped_columns_pub(&in_tbl, &in_slot.name, &eff_a);
            if let Ok((in_cnames, _)) = expand_projection(in_proj, &in_scols)
                && in_cnames.len() == 1
                && in_cnames[0].eq_ignore_ascii_case(&out_cref.column)
            {
                let out_col_lbl = out_alias
                    .clone()
                    .or_else(|| out_span.clone())
                    .unwrap_or_else(|| format!("{fn_name}({})", out_cref.column));
                if let Some(plan) = try_compile_single_table_core(
                    in_dist,
                    in_proj,
                    &in_fc.head,
                    in_sel.as_ref(),
                    in_gb,
                    in_hav.as_ref(),
                    &inner_q.order_by,
                    inner_q.limit.as_ref(),
                    Some(&out_col_lbl),
                    db_set,
                    functions,
                    collations,
                ) {
                    return Some(plan);
                }
            }
        }
    }

    if fc.joins.is_empty() {
        return try_compile_single_table_core(
            distinct,
            projection,
            &fc.head,
            selection.as_ref(),
            group_by,
            having.as_ref(),
            &stmt.order_by,
            stmt.limit.as_ref(),
            None,
            db_set,
            functions,
            collations,
        );
    }

    if fc.joins.len() == 1 {
        return try_compile_two_table_join_core(
            distinct,
            projection,
            fc,
            selection.as_ref(),
            group_by,
            having.as_ref(),
            &stmt.order_by,
            stmt.limit.as_ref(),
            db_set,
            functions,
            collations,
        );
    }

    None
}
