//! Integer-only row kernel for covering-index scans that feed aggregates.
//!
//! The general row path of the streaming plans decodes every index cell into `RawField`s, maps
//! them onto table columns, walks the `WHERE` expression tree and dispatches every aggregate
//! through `FastAggState::step`. For the very common shape "the columns the plan reads are all
//! INTEGERs and the `WHERE` is a conjunction of column/constant comparisons" that work reduces
//! to a handful of integer loads, range tests and additions.
//!
//! A [`CovIntKernelShape`] is derived from the plan once (it never looks at data), bound to the
//! statement parameters per execution ([`CovIntKernelShape::bind`]), and then tried row by row:
//! [`CovIntKernel::decode`] accepts a cell only when the general decoder would produce an
//! INTEGER for every column the plan reads. Any other cell — NULL / REAL / TEXT / BLOB values,
//! wide headers, short or damaged records — and any execution whose parameters are not
//! INTEGERs runs through the unchanged general row code, so results and errors are identical.

use std::cmp::Ordering;

use sqliters_types::{
    INT_RECORD_MAX_SLOTS, INT_RECORD_SKIP, TextEncoding, TypeAffinity, Value,
    decode_int_record_fields,
};

use super::{
    FastAggSpec, FastAggState, FastCmpOp, FastColSrc, FastCoveringIndex, FastExpr, FastScalarRhs,
};

/// Most `WHERE` conjuncts a kernel evaluates; larger predicates use the general path.
const MAX_TESTS: usize = 8;

/// Widest index-record prefix a kernel decodes (the general path's on-stack window).
const MAX_PREFIX_FIELDS: usize = 16;

/// One `WHERE` conjunct over an integer slot, before parameter binding.
#[derive(Debug, Clone)]
enum IntTestShape {
    Cmp {
        slot: u8,
        op: FastCmpOp,
        rhs: FastScalarRhs,
    },
    Between {
        slot: u8,
        low: FastScalarRhs,
        high: FastScalarRhs,
        negated: bool,
    },
}

/// One aggregate over integer slots; entry `i` drives `FastAggState` `i` of the plan.
#[derive(Debug, Clone, Copy)]
enum IntAggOp {
    /// `count(*)`, or `count(col)` of a decoded (hence non-NULL) column.
    Count,
    /// `sum(col)` / `total(col)` / `avg(col)`.
    Sum(u8),
    /// `min(col)` / `max(col)`.
    MinMax(u8),
}

/// Plan-time description of an integer kernel (see the module documentation).
#[derive(Debug, Clone)]
pub(super) struct CovIntKernelShape {
    /// For each of the first `parse_prefix_len` index record fields: the slot it is decoded
    /// into, or [`INT_RECORD_SKIP`].
    field_slots: Box<[u8]>,
    tests: Box<[IntTestShape]>,
    aggs: Box<[IntAggOp]>,
    /// Slot of the `GROUP BY` column (0 when the plan has none).
    group_slot: u8,
}

/// Assigns integer slots to the index record fields a plan reads.
struct SlotAlloc<'a> {
    cov: &'a FastCoveringIndex,
    col_buf_len: usize,
    field_slots: Vec<u8>,
    n_slots: u8,
}

impl SlotAlloc<'_> {
    /// The index record field `decode_covering_cell` + `FastRowCtx::get_col` read `src` from.
    fn field_of(&self, src: FastColSrc) -> Option<usize> {
        let n_idx = self.cov.idx_to_tbl_col.len();
        match src {
            FastColSrc::Rowid { side: 0 }
            | FastColSrc::RecordCol {
                side: 0,
                is_ipk: true,
                ..
            } => (self.cov.needs_rowid && n_idx < self.cov.parse_prefix_len).then_some(n_idx),
            FastColSrc::RecordCol {
                side: 0,
                col_idx,
                is_ipk: false,
            } => {
                if (col_idx as usize) >= self.col_buf_len {
                    return None;
                }
                // The general decoder lets the last index field mapped to a table column win.
                (0..self.cov.parse_prefix_len.min(n_idx))
                    .rev()
                    .find(|&i| self.cov.idx_to_tbl_col[i] == Some(col_idx))
            }
            _ => None,
        }
    }

    fn slot_for(&mut self, src: FastColSrc) -> Option<u8> {
        let field = self.field_of(src)?;
        if self.field_slots[field] == INT_RECORD_SKIP {
            if (self.n_slots as usize) >= INT_RECORD_MAX_SLOTS {
                return None;
            }
            self.field_slots[field] = self.n_slots;
            self.n_slots += 1;
        }
        Some(self.field_slots[field])
    }

    /// Flattens an `AND` tree of integer-decidable comparisons into `out` (evaluation order).
    fn collect_tests(&mut self, expr: &FastExpr, out: &mut Vec<IntTestShape>) -> Option<()> {
        match expr {
            FastExpr::And(a, b) => {
                self.collect_tests(a, out)?;
                self.collect_tests(b, out)
            }
            FastExpr::ColCmpRhs {
                col,
                op,
                rhs,
                affinity,
                collation: _,
            } if *affinity != TypeAffinity::Text => {
                out.push(IntTestShape::Cmp {
                    slot: self.slot_for(*col)?,
                    op: *op,
                    rhs: rhs.clone(),
                });
                Some(())
            }
            FastExpr::ColBetweenRhs {
                col,
                low,
                high,
                negated,
                affinity,
                collation: _,
            } if *affinity != TypeAffinity::Text => {
                out.push(IntTestShape::Between {
                    slot: self.slot_for(*col)?,
                    low: low.clone(),
                    high: high.clone(),
                    negated: *negated,
                });
                Some(())
            }
            _ => None,
        }
    }
}

impl CovIntKernelShape {
    /// Derives the kernel for a plan that reads `cov` (decoding into `col_buf_len` table column
    /// slots), filters by `where_pred`, optionally groups by `group_col` and feeds `aggs`.
    /// `None` when any part of the plan is outside what the kernel evaluates.
    pub(super) fn build(
        cov: &FastCoveringIndex,
        col_buf_len: usize,
        where_pred: Option<&FastExpr>,
        group_col: Option<FastColSrc>,
        aggs: &[FastAggSpec],
    ) -> Option<Self> {
        if cov.parse_prefix_len > MAX_PREFIX_FIELDS {
            return None;
        }
        let mut alloc = SlotAlloc {
            cov,
            col_buf_len,
            field_slots: vec![INT_RECORD_SKIP; cov.parse_prefix_len],
            n_slots: 0,
        };
        let mut tests = Vec::new();
        if let Some(pred) = where_pred {
            alloc.collect_tests(pred, &mut tests)?;
        }
        if tests.len() > MAX_TESTS {
            return None;
        }
        let group_slot = match group_col {
            None => 0,
            Some(src) => alloc.slot_for(src)?,
        };
        let mut ops = Vec::with_capacity(aggs.len());
        for spec in aggs {
            ops.push(match spec {
                FastAggSpec::CountStar => IntAggOp::Count,
                FastAggSpec::Count(FastExpr::Col(c)) => {
                    alloc.slot_for(*c)?;
                    IntAggOp::Count
                }
                FastAggSpec::Sum(FastExpr::Col(c))
                | FastAggSpec::Total(FastExpr::Col(c))
                | FastAggSpec::Avg(FastExpr::Col(c)) => IntAggOp::Sum(alloc.slot_for(*c)?),
                FastAggSpec::Min(FastExpr::Col(c), _) | FastAggSpec::Max(FastExpr::Col(c), _) => {
                    IntAggOp::MinMax(alloc.slot_for(*c)?)
                }
                _ => return None,
            });
        }
        Some(Self {
            field_slots: alloc.field_slots.into_boxed_slice(),
            tests: tests.into_boxed_slice(),
            aggs: ops.into_boxed_slice(),
            group_slot,
        })
    }

    /// Binds the kernel to the parameters of one execution; `None` unless every comparison
    /// operand is an INTEGER (anything else has NULL / affinity semantics the general path owns).
    #[inline]
    pub(super) fn bind(&self, params: &[Value]) -> Option<CovIntKernel<'_>> {
        let mut tests = [IntRangeTest {
            slot: 0,
            negated: false,
            lo: 0,
            hi: 0,
        }; MAX_TESTS];
        for (out, shape) in tests.iter_mut().zip(self.tests.iter()) {
            *out = match shape {
                IntTestShape::Cmp { slot, op, rhs } => {
                    let Value::Integer(v) = *rhs.resolve(params) else {
                        return None;
                    };
                    // `x op v` as `lo <= x <= hi` (possibly negated); `(1, 0)` is the empty range.
                    let (lo, hi, negated) = match op {
                        FastCmpOp::Eq | FastCmpOp::Is => (v, v, false),
                        FastCmpOp::Ne | FastCmpOp::IsNot => (v, v, true),
                        FastCmpOp::Lt => match v.checked_sub(1) {
                            Some(h) => (i64::MIN, h, false),
                            None => (1, 0, false),
                        },
                        FastCmpOp::Le => (i64::MIN, v, false),
                        FastCmpOp::Gt => match v.checked_add(1) {
                            Some(l) => (l, i64::MAX, false),
                            None => (1, 0, false),
                        },
                        FastCmpOp::Ge => (v, i64::MAX, false),
                    };
                    IntRangeTest {
                        slot: *slot,
                        negated,
                        lo,
                        hi,
                    }
                }
                IntTestShape::Between {
                    slot,
                    low,
                    high,
                    negated,
                } => {
                    let (Value::Integer(lo), Value::Integer(hi)) =
                        (low.resolve(params), high.resolve(params))
                    else {
                        return None;
                    };
                    IntRangeTest {
                        slot: *slot,
                        negated: *negated,
                        lo: *lo,
                        hi: *hi,
                    }
                }
            };
        }
        Some(CovIntKernel {
            field_slots: &self.field_slots,
            aggs: &self.aggs,
            group_slot: self.group_slot,
            n_tests: self.tests.len(),
            tests,
        })
    }
}

/// `(lo <= x && x <= hi) != negated` over slot `slot`.
#[derive(Debug, Clone, Copy)]
struct IntRangeTest {
    slot: u8,
    negated: bool,
    lo: i64,
    hi: i64,
}

/// Decoded integer slots of one index cell.
pub(super) type IntRow = [i64; INT_RECORD_MAX_SLOTS];

#[inline(always)]
fn slot_val(row: &IntRow, slot: u8) -> i64 {
    row[(slot as usize) % INT_RECORD_MAX_SLOTS]
}

/// A [`CovIntKernelShape`] bound to the parameters of one execution.
pub(super) struct CovIntKernel<'k> {
    field_slots: &'k [u8],
    aggs: &'k [IntAggOp],
    group_slot: u8,
    n_tests: usize,
    tests: [IntRangeTest; MAX_TESTS],
}

impl CovIntKernel<'_> {
    /// Decodes the integer columns of an index cell; `false` sends the cell to the general
    /// row code (see [`decode_int_record_fields`]).
    #[inline(always)]
    pub(super) fn decode(&self, cell: &[u8], row: &mut IntRow) -> bool {
        decode_int_record_fields(cell, self.field_slots, row)
    }

    /// The plan's `WHERE` for a decoded row (`eval_where_true`).
    #[inline(always)]
    pub(super) fn passes(&self, row: &IntRow) -> bool {
        for t in &self.tests[..self.n_tests] {
            let x = slot_val(row, t.slot);
            if ((t.lo <= x) & (x <= t.hi)) == t.negated {
                return false;
            }
        }
        true
    }

    /// The `GROUP BY` column of a decoded row.
    #[inline(always)]
    pub(super) fn group_key(&self, row: &IntRow) -> i64 {
        slot_val(row, self.group_slot)
    }

    /// Steps the plan's aggregate states for a decoded row (`FastAggState::step` for each).
    #[inline(always)]
    pub(super) fn step(&self, states: &mut [FastAggState], row: &IntRow, enc: TextEncoding) {
        for (st, op) in states.iter_mut().zip(self.aggs.iter()) {
            match (op, st) {
                (IntAggOp::Count, FastAggState::Count(n)) => *n += 1,
                (IntAggOp::Sum(slot), FastAggState::Sum(acc)) => acc.step_i64(slot_val(row, *slot)),
                (
                    IntAggOp::MinMax(slot),
                    FastAggState::MinMax {
                        is_max,
                        collation,
                        best,
                    },
                ) => {
                    let v = slot_val(row, *slot);
                    match best {
                        None => *best = Some(Value::Integer(v)),
                        Some(Value::Integer(cur)) => {
                            if (*is_max && v > *cur) || (!*is_max && v < *cur) {
                                *cur = v;
                            }
                        }
                        Some(cur) => {
                            let nv = Value::Integer(v);
                            let ord = nv.cmp_sqlite_with_encoding(cur, *collation, enc);
                            let better = if *is_max {
                                ord == Ordering::Greater
                            } else {
                                ord == Ordering::Less
                            };
                            if better {
                                *cur = nv;
                            }
                        }
                    }
                }
                _ => debug_assert!(false, "int kernel op does not match its aggregate state"),
            }
        }
    }
}
