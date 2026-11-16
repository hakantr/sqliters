//! `generate_series` eponymous virtual table module (`ext/misc/series.c`).
#![forbid(unsafe_code)]

use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConstraintOp, VTabHostContext,
    VTabIndexInfo, VTabRiskFlags, Value, VirtualCursor, VirtualTable, VirtualTableModule,
    parse_numeric_text,
};

const SERIES_SCHEMA: &str = "CREATE TABLE x(value,start hidden,stop hidden,step hidden)";

const SERIES_BIT_START: i32 = 0x01;
const SERIES_BIT_STOP: i32 = 0x02;
const SERIES_BIT_STEP: i32 = 0x04;
const SERIES_BIT_DESC: i32 = 0x08;
const SERIES_BIT_ASC: i32 = 0x10;
// Constraints on the `value` column handed to the cursor (same bits as series.c).
const SERIES_VALUE_EQ: i32 = 0x0080;
const SERIES_VALUE_GE: i32 = 0x0100;
const SERIES_VALUE_GT: i32 = 0x0200;
const SERIES_VALUE_LE: i32 = 0x1000;
const SERIES_VALUE_LT: i32 = 0x2000;

/// `sqlite3_value_numeric_type(v)==SQLITE_FLOAT`: the value as a double when it is a REAL or
/// text that converts to one.
fn series_numeric_float(v: &Value) -> Option<f64> {
    match v {
        Value::Real(r) => Some(*r),
        Value::Text(s) => match sqliters_types::parse_numeric_text(s) {
            Some(Value::Real(r)) => Some(r),
            _ => None,
        },
        _ => None,
    }
}

/// `seriesRealToI64`.
fn series_real_to_i64(r: f64) -> i64 {
    if r < -9_223_372_036_854_774_784.0 {
        i64::MIN
    } else if r > 9_223_372_036_854_774_784.0 {
        i64::MAX
    } else {
        r as i64
    }
}

/// The output range `[min, max]` that the constraints on `value` leave (series.c
/// seriesFilter); `None` when no row can qualify. `args` are the constraint values in the
/// order value= / lower bound, then upper bound.
fn series_value_range(idx_num: i32, args: &[Value]) -> Option<(i64, i64)> {
    let mut i_min = i64::MIN;
    let mut i_max = i64::MAX;
    let mut it = args.iter();
    if idx_num & SERIES_VALUE_EQ != 0 {
        let v = it.next()?;
        if let Some(r) = series_numeric_float(v) {
            if r == r.ceil() && r >= i64::MIN as f64 && r <= i64::MAX as f64 {
                i_min = series_real_to_i64(r);
                i_max = i_min;
            } else {
                return None;
            }
        } else {
            i_min = v.to_integer_lossy();
            i_max = i_min;
        }
        return Some((i_min, i_max));
    }
    if idx_num & (SERIES_VALUE_GE | SERIES_VALUE_GT) != 0 {
        let v = it.next()?;
        let strict = idx_num & SERIES_VALUE_GT != 0;
        if let Some(r) = series_numeric_float(v) {
            if r <= i64::MIN as f64 {
                i_min = i64::MIN;
            } else if r > i64::MAX as f64 {
                return None;
            } else {
                i_min = series_real_to_i64(r.ceil());
                if strict && r == r.ceil() {
                    if i_min == i64::MAX {
                        return None;
                    }
                    i_min += 1;
                }
            }
        } else {
            i_min = v.to_integer_lossy();
            if strict {
                if i_min == i64::MAX {
                    return None;
                }
                i_min += 1;
            }
        }
    }
    if idx_num & (SERIES_VALUE_LE | SERIES_VALUE_LT) != 0 {
        let v = it.next()?;
        let strict = idx_num & SERIES_VALUE_LT != 0;
        if let Some(r) = series_numeric_float(v) {
            if r >= i64::MAX as f64 {
                i_max = i64::MAX;
            } else if r <= i64::MIN as f64 {
                return None;
            } else {
                i_max = series_real_to_i64(r.floor());
                if strict && r == r.floor() {
                    if i_max == i64::MIN {
                        return None;
                    }
                    i_max -= 1;
                }
            }
        } else {
            i_max = v.to_integer_lossy();
            if strict {
                if i_max == i64::MIN {
                    return None;
                }
                i_max -= 1;
            }
        }
    }
    if i_min > i_max {
        return None;
    }
    Some((i_min, i_max))
}

/// Built-in `generate_series(start [, stop [, step]])` eponymous-only virtual table module.
#[derive(Debug, Default, Clone, Copy)]
pub struct GenerateSeriesModule;

impl VirtualTableModule for GenerateSeriesModule {
    fn name(&self) -> &str {
        "generate_series"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn is_eponymous(&self) -> bool {
        true
    }

    fn is_eponymous_only(&self) -> bool {
        true
    }

    fn create(
        &self,
        _ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        Err(SqliteError::new(
            ErrorCode::Error,
            "no such module: generate_series",
        ))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        ctx.declare_vtab(SERIES_SCHEMA)?;
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
        Ok(Box::new(GenerateSeriesTable))
    }
}

#[derive(Debug, Default)]
struct GenerateSeriesTable;

impl VirtualTable for GenerateSeriesTable {
    /// `seriesBestIndex()` of series.c as the reference shell (3.54.1) builds it
    /// (`SQLITE_SERIES_CONSTRAINT_VERIFY` 0, `SQLITE_INDEX_SCAN_HEX` defined). Used by the port
    /// of C's code generator; the engine's executor plans with [`Self::best_index`].
    ///
    /// The query plan selected is passed in the idxNum parameter. idxNum is a bitmask:
    /// 0x0001 start=, 0x0002 stop=, 0x0004 step=, 0x0008 descending order, 0x0010 ascending
    /// order, 0x0020 LIMIT, 0x0040 OFFSET, 0x0080 value=, 0x0100 value>=, 0x0200 value>,
    /// 0x1000 value<=, 0x2000 value<.
    fn best_index_calls(&self, info: &mut VTabIndexInfo, _calls: &mut dyn sqliters_types::VTabIndexInfoCalls) -> Result<()> {
        let mut idx_num = 0i32; // The query plan bitmask
        let mut b_start_seen = false; // EQ constraint seen on the START column
        let mut unusable_mask = 0i32; // Mask of unusable constraints
        let mut n_arg = 0usize; // Number of arguments that seriesFilter() expects
        // Constraints on start, stop, step, LIMIT, OFFSET, and value. aIdx[5] covers value=,
        // value>=, and value>, aIdx[6] covers value<= and value<
        let mut a_idx = [-1i64; 7];
        for (i, c) in info.constraints.iter().enumerate() {
            let op = c.op.c_code();
            if (73..=74).contains(&op) {
                // SQLITE_INDEX_CONSTRAINT_LIMIT ..= SQLITE_INDEX_CONSTRAINT_OFFSET
                if !c.usable {
                    // do nothing
                } else if op == 73 {
                    a_idx[3] = i as i64;
                    idx_num |= 0x20;
                } else {
                    a_idx[4] = i as i64;
                    idx_num |= 0x40;
                }
                continue;
            }
            if c.i_column < 1 {
                // < SERIES_COLUMN_START
                if (c.i_column == 0 || c.i_column == -1) && c.usable {
                    match op {
                        2 | 72 => {
                            // SQLITE_INDEX_CONSTRAINT_EQ, SQLITE_INDEX_CONSTRAINT_IS
                            idx_num |= 0x0080;
                            idx_num &= !0x3300;
                            a_idx[5] = i as i64;
                            a_idx[6] = -1;
                            b_start_seen = true;
                        }
                        32 => {
                            // SQLITE_INDEX_CONSTRAINT_GE
                            if idx_num & 0x0080 == 0 {
                                idx_num |= 0x0100;
                                idx_num &= !0x0200;
                                a_idx[5] = i as i64;
                                b_start_seen = true;
                            }
                        }
                        4 => {
                            // SQLITE_INDEX_CONSTRAINT_GT
                            if idx_num & 0x0080 == 0 {
                                idx_num |= 0x0200;
                                idx_num &= !0x0100;
                                a_idx[5] = i as i64;
                                b_start_seen = true;
                            }
                        }
                        8 => {
                            // SQLITE_INDEX_CONSTRAINT_LE
                            if idx_num & 0x0080 == 0 {
                                idx_num |= 0x1000;
                                idx_num &= !0x2000;
                                a_idx[6] = i as i64;
                            }
                        }
                        16 => {
                            // SQLITE_INDEX_CONSTRAINT_LT
                            if idx_num & 0x0080 == 0 {
                                idx_num |= 0x2000;
                                idx_num &= !0x1000;
                                a_idx[6] = i as i64;
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }
            let i_col = (c.i_column - 1) as usize; // 0 for start, 1 for stop, 2 for step
            let i_mask = 1i32 << i_col;
            if i_col == 0 && op == 2 {
                b_start_seen = true;
            }
            if !c.usable {
                unusable_mask |= i_mask;
                continue;
            } else if op == 2 {
                idx_num |= i_mask;
                a_idx[i_col] = i as i64;
            }
        }
        if a_idx[3] == 0 {
            // Ignore OFFSET if LIMIT is omitted
            idx_num &= !0x60;
            a_idx[4] = 0;
        }
        for (i, &j) in a_idx.iter().enumerate() {
            if j >= 0 {
                n_arg += 1;
                info.constraint_usage[j as usize].argv_index = n_arg;
                // !SQLITE_SERIES_CONSTRAINT_VERIFY || i>=3
                info.constraint_usage[j as usize].omit = true;
                let _ = i;
            }
        }
        // The current generate_column() implementation requires at least one argument (the
        // START value).
        if !b_start_seen {
            return Err(SqliteError::new(ErrorCode::Error, "first argument to \"generate_series()\" missing or unusable"));
        }
        if (unusable_mask & !idx_num) != 0 {
            // The start, stop, and step columns are inputs. Therefore if there are unusable
            // constraints on any of start, stop, or step then this plan is unusable
            return Err(SqliteError::new(ErrorCode::Constraint, "unusable generate_series constraint"));
        }
        if (idx_num & 0x03) == 0x03 {
            // Both start= and stop= boundaries are available. This is the the preferred case
            info.estimated_cost = (2 - i32::from((idx_num & 4) != 0)) as f64;
            info.estimated_rows = 1000;
            if !info.order_by.is_empty() && info.order_by[0].i_column == 0 {
                if info.order_by[0].desc {
                    idx_num |= 0x08;
                } else {
                    idx_num |= 0x10;
                }
                info.order_by_consumed = true;
            }
        } else if (idx_num & 0x21) == 0x21 {
            // We have start= and LIMIT
            info.estimated_rows = 2500;
        } else {
            // If either boundary is missing, we have to generate a huge span of numbers. Make
            // this case very expensive so that the query planner will work hard to avoid it.
            info.estimated_rows = 2_147_483_647;
        }
        info.idx_num = idx_num;
        info.idx_flags = sqliters_types::SQLITE_INDEX_SCAN_HEX;
        Ok(())
    }

    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut idx_num = 0i32;
        let mut unusable_mask = 0i32;
        let mut arg_indices = [None::<usize>; 3];
        // series.c seriesBestIndex: constraints on `value` (or the rowid) narrow the output
        // range inside the module: [0] covers value=, value>= and value>, [1] value<= and <.
        let mut value_args = [None::<usize>; 2];

        for (i, constraint) in info.constraints.iter().enumerate() {
            if matches!(
                constraint.op,
                VTabConstraintOp::Limit | VTabConstraintOp::Offset
            ) {
                continue;
            }
            if constraint.i_column < 1 {
                if constraint.usable {
                    match constraint.op {
                        VTabConstraintOp::Eq | VTabConstraintOp::Is => {
                            idx_num |= SERIES_VALUE_EQ;
                            idx_num &= !(SERIES_VALUE_GE
                                | SERIES_VALUE_GT
                                | SERIES_VALUE_LE
                                | SERIES_VALUE_LT);
                            value_args = [Some(i), None];
                        }
                        VTabConstraintOp::Ge if idx_num & SERIES_VALUE_EQ == 0 => {
                            idx_num = (idx_num | SERIES_VALUE_GE) & !SERIES_VALUE_GT;
                            value_args[0] = Some(i);
                        }
                        VTabConstraintOp::Gt if idx_num & SERIES_VALUE_EQ == 0 => {
                            idx_num = (idx_num | SERIES_VALUE_GT) & !SERIES_VALUE_GE;
                            value_args[0] = Some(i);
                        }
                        VTabConstraintOp::Le if idx_num & SERIES_VALUE_EQ == 0 => {
                            idx_num = (idx_num | SERIES_VALUE_LE) & !SERIES_VALUE_LT;
                            value_args[1] = Some(i);
                        }
                        VTabConstraintOp::Lt if idx_num & SERIES_VALUE_EQ == 0 => {
                            idx_num = (idx_num | SERIES_VALUE_LT) & !SERIES_VALUE_LE;
                            value_args[1] = Some(i);
                        }
                        _ => {}
                    }
                }
                continue;
            }
            if constraint.i_column > 3 {
                continue;
            }
            let bit_idx = (constraint.i_column - 1) as usize;
            let bit_mask = 1i32 << bit_idx;
            if !constraint.usable {
                unusable_mask |= bit_mask;
                continue;
            }
            if constraint.op == VTabConstraintOp::Eq {
                idx_num |= bit_mask;
                arg_indices[bit_idx] = Some(i);
            }
        }

        let mut n_arg = 0usize;
        for c_idx in arg_indices.into_iter().chain(value_args).flatten() {
            n_arg += 1;
            info.constraint_usage[c_idx].argv_index = n_arg;
            info.constraint_usage[c_idx].omit = true;
        }

        if (unusable_mask & !idx_num) != 0 {
            return Err(SqliteError::new(
                ErrorCode::Constraint,
                "unusable generate_series constraint",
            ));
        }

        if (idx_num & SERIES_BIT_START) == 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "first argument to \"generate_series()\" missing or unusable",
            ));
        }

        if (idx_num & 0x03) == 0x03 {
            let has_step = (idx_num & SERIES_BIT_STEP) != 0;
            info.estimated_cost = if has_step { 1.0 } else { 2.0 };
            info.estimated_rows = 1000;
            if info.order_by.len() == 1 {
                let ob = &info.order_by[0];
                if ob.i_column == 0 || ob.i_column == -1 {
                    if ob.desc {
                        idx_num |= SERIES_BIT_DESC;
                    } else {
                        idx_num |= SERIES_BIT_ASC;
                    }
                    info.order_by_consumed = true;
                }
            }
        } else {
            info.estimated_rows = 2_147_483_647;
        }

        info.idx_num = idx_num;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(GenerateSeriesCursor::default()))
    }
}

/// `series_cursor` of series.c, filled by `seriesFilter` (the C plan, `filter_c`).
#[derive(Debug, Default, Clone, Copy)]
struct SeriesC {
    b_desc: bool,
    b_done: bool,
    i_obase: i64,
    i_oterm: i64,
    i_ostep: i64,
    i_base: i64,
    i_term: i64,
    i_step: u64,
    i_value: i64,
}

/// `span64(a, b)`: a - b as an unsigned 64-bit value (a >= b)
fn span64(a: i64, b: i64) -> u64 {
    (a as u64).wrapping_sub(b as u64)
}

/// `add64`, `sub64`
fn add64(a: i64, b: u64) -> i64 {
    (a as u64).wrapping_add(b) as i64
}
fn sub64(a: i64, b: u64) -> i64 {
    (a as u64).wrapping_sub(b) as i64
}

impl SeriesC {
    /// `seriesSteps(pCur)`
    fn steps(&self) -> u64 {
        if self.b_desc {
            span64(self.i_base, self.i_term) / self.i_step
        } else {
            span64(self.i_term, self.i_base) / self.i_step
        }
    }

    /// `series_no_rows:`
    fn no_rows(&mut self) {
        self.i_base = 0;
        self.i_term = 0;
        self.i_step = 1;
        self.b_desc = false;
        self.b_done = true;
    }

    /// `seriesFilter(pVtabCursor, idxNum, idxStr, argc, argv)`
    #[allow(clippy::too_many_lines)]
    fn filter(&mut self, idx_num: i32, argv: &[Value]) {
        const SMALLEST_INT64: i64 = i64::MIN;
        const LARGEST_INT64: i64 = i64::MAX;
        let mut i_arg = 0usize;
        let mut i_min = SMALLEST_INT64;
        let mut i_max = LARGEST_INT64;
        let mut i_limit: i64 = 0;
        let mut i_offset: i64 = 0;
        let arg = |i: usize| argv.get(i).cloned().unwrap_or(Value::Null);
        // If any constraints have a NULL value, then return no rows.
        if argv.iter().any(Value::is_null) {
            return self.no_rows();
        }
        // Capture the three HIDDEN parameters to the virtual table and insert default values
        // for any parameters that are omitted.
        if idx_num & 0x01 != 0 {
            self.i_obase = arg(i_arg).to_integer_lossy();
            i_arg += 1;
        } else {
            self.i_obase = 0;
        }
        if idx_num & 0x02 != 0 {
            self.i_oterm = arg(i_arg).to_integer_lossy();
            i_arg += 1;
        } else {
            self.i_oterm = 0xffff_ffff;
        }
        if idx_num & 0x04 != 0 {
            self.i_ostep = arg(i_arg).to_integer_lossy();
            i_arg += 1;
            if self.i_ostep == 0 {
                self.i_ostep = 1;
            }
        } else {
            self.i_ostep = 1;
        }
        // If there are constraints on the value column but there are no constraints on the
        // start, stop, and step columns, then initialize the default range to be the entire
        // range of 64-bit signed integers.
        if (idx_num & 0x05) == 0 && (idx_num & 0x0380) != 0 {
            self.i_obase = SMALLEST_INT64;
        }
        if (idx_num & 0x06) == 0 && (idx_num & 0x3080) != 0 {
            self.i_oterm = LARGEST_INT64;
        }
        self.i_base = self.i_obase;
        self.i_term = self.i_oterm;
        if self.i_ostep > 0 {
            self.i_step = self.i_ostep as u64;
        } else if self.i_ostep > SMALLEST_INT64 {
            self.i_step = (-self.i_ostep) as u64;
        } else {
            self.i_step = (LARGEST_INT64 as u64) + 1;
        }
        self.b_desc = self.i_ostep < 0;
        if !self.b_desc && self.i_base > self.i_term {
            return self.no_rows();
        }
        if self.b_desc && self.i_base < self.i_term {
            return self.no_rows();
        }
        // Extract the LIMIT and OFFSET values, but do not apply them yet.
        if idx_num & 0x20 != 0 {
            i_limit = arg(i_arg).to_integer_lossy();
            i_arg += 1;
            if idx_num & 0x40 != 0 {
                i_offset = arg(i_arg).to_integer_lossy();
                i_arg += 1;
            }
        }
        // Narrow the range of iMin and iMax based on constraints on the "value" column.
        if idx_num & 0x3380 != 0 {
            if idx_num & 0x0080 != 0 {
                // value=X
                let v = arg(i_arg);
                i_arg += 1;
                if let Some(r) = series_numeric_float(&v) {
                    if r == r.ceil() && r >= SMALLEST_INT64 as f64 && r <= LARGEST_INT64 as f64 {
                        i_min = series_real_to_i64(r);
                        i_max = i_min;
                    } else {
                        return self.no_rows();
                    }
                } else {
                    i_min = v.to_integer_lossy();
                    i_max = i_min;
                }
            } else {
                if idx_num & 0x0300 != 0 {
                    // value>X (0x200) or value>=X (0x100)
                    let v = arg(i_arg);
                    i_arg += 1;
                    if let Some(r) = series_numeric_float(&v) {
                        if r <= SMALLEST_INT64 as f64 {
                            i_min = SMALLEST_INT64;
                        } else if r > LARGEST_INT64 as f64 {
                            return self.no_rows();
                        } else {
                            i_min = series_real_to_i64(r.ceil());
                            if (idx_num & 0x0200) != 0 && r == r.ceil() {
                                if i_min == LARGEST_INT64 {
                                    return self.no_rows();
                                }
                                i_min += 1;
                            }
                        }
                    } else {
                        i_min = v.to_integer_lossy();
                        if (idx_num & 0x0200) != 0 {
                            if i_min == LARGEST_INT64 {
                                return self.no_rows();
                            }
                            i_min += 1;
                        }
                    }
                }
                if idx_num & 0x3000 != 0 {
                    // value<X (0x2000) or value<=X (0x1000)
                    let v = arg(i_arg);
                    if let Some(r) = series_numeric_float(&v) {
                        if r >= LARGEST_INT64 as f64 {
                            i_max = LARGEST_INT64;
                        } else if r <= SMALLEST_INT64 as f64 {
                            return self.no_rows();
                        } else {
                            i_max = series_real_to_i64(r.floor());
                            if (idx_num & 0x2000) != 0 && r == r.floor() {
                                if i_max == SMALLEST_INT64 {
                                    return self.no_rows();
                                }
                                i_max -= 1;
                            }
                        }
                    } else {
                        i_max = v.to_integer_lossy();
                        if idx_num & 0x2000 != 0 {
                            if i_max == SMALLEST_INT64 {
                                return self.no_rows();
                            }
                            i_max -= 1;
                        }
                    }
                }
                if i_min > i_max {
                    return self.no_rows();
                }
            }
            // Try to reduce the range of values to be generated based on constraints on the
            // "value" column.
            if !self.b_desc {
                if self.i_base < i_min {
                    let span = span64(i_min, self.i_base);
                    self.i_base = add64(self.i_base, (span / self.i_step).wrapping_mul(self.i_step));
                    if self.i_base < i_min {
                        if self.i_base > sub64(LARGEST_INT64, self.i_step) {
                            return self.no_rows();
                        }
                        self.i_base = add64(self.i_base, self.i_step);
                    }
                }
                if self.i_term > i_max {
                    self.i_term = i_max;
                }
            } else {
                if self.i_base > i_max {
                    let span = span64(self.i_base, i_max);
                    self.i_base = sub64(self.i_base, (span / self.i_step).wrapping_mul(self.i_step));
                    if self.i_base > i_max {
                        if self.i_base < add64(SMALLEST_INT64, self.i_step) {
                            return self.no_rows();
                        }
                        self.i_base = sub64(self.i_base, self.i_step);
                    }
                }
                if self.i_term < i_min {
                    self.i_term = i_min;
                }
            }
        }
        // Adjust iTerm so that it is exactly the last value of the series.
        if !self.b_desc {
            if self.i_base > self.i_term {
                return self.no_rows();
            }
            self.i_term = sub64(self.i_term, span64(self.i_term, self.i_base) % self.i_step);
        } else {
            if self.i_base < self.i_term {
                return self.no_rows();
            }
            self.i_term = add64(self.i_term, span64(self.i_base, self.i_term) % self.i_step);
        }
        // Transform the series generator to output values in the requested order.
        if ((idx_num & 0x0008) != 0 && !self.b_desc) || ((idx_num & 0x0010) != 0 && self.b_desc) {
            std::mem::swap(&mut self.i_base, &mut self.i_term);
            self.b_desc = !self.b_desc;
        }
        // Apply LIMIT and OFFSET constraints, if any
        if idx_num & 0x20 != 0 {
            if i_offset > 0 {
                if self.steps() < i_offset as u64 {
                    return self.no_rows();
                } else if self.b_desc {
                    self.i_base = sub64(self.i_base, self.i_step.wrapping_mul(i_offset as u64));
                } else {
                    self.i_base = add64(self.i_base, self.i_step.wrapping_mul(i_offset as u64));
                }
            }
            if i_limit >= 0 && self.steps() > i_limit as u64 {
                self.i_term = add64(self.i_base, ((i_limit - 1) as u64).wrapping_mul(self.i_step));
            }
        }
        self.i_value = self.i_base;
        self.b_done = false;
        let _ = i_arg;
    }

    /// `seriesNext`
    fn next(&mut self) {
        if self.i_value == self.i_term {
            self.b_done = true;
        } else if self.b_desc {
            self.i_value = sub64(self.i_value, self.i_step);
        } else {
            self.i_value = add64(self.i_value, self.i_step);
        }
    }
}

#[derive(Debug, Default)]
struct GenerateSeriesCursor {
    /// set by `filter_c` (C's plan): the cursor runs as series.c's `series_cursor`
    c: Option<SeriesC>,
    is_eof: bool,
    is_desc: bool,
    raw_start: i64,
    raw_stop: i64,
    raw_step: i64,
    low_value: i64,
    high_value: i64,
    abs_step: i64,
    current_value: i64,
}

fn value_to_i64(val: &Value) -> i64 {
    match val {
        Value::Null => 0,
        Value::Integer(i) => *i,
        Value::Real(f) => *f as i64,
        Value::Text(s) => match parse_numeric_text(s) {
            Some(Value::Integer(i)) => i,
            Some(Value::Real(f)) => f as i64,
            _ => {
                let trimmed = s.trim_start();
                let mut end = 0;
                for (idx, ch) in trimmed.char_indices() {
                    if (idx == 0 && (ch == '+' || ch == '-')) || ch.is_ascii_digit() {
                        end = idx + ch.len_utf8();
                    } else {
                        break;
                    }
                }
                trimmed[..end].parse::<i64>().unwrap_or(0)
            }
        },
        Value::Blob(b) => {
            let s = String::from_utf8_lossy(b);
            value_to_i64(&Value::Text(s.into_owned()))
        }
    }
}

impl VirtualCursor for GenerateSeriesCursor {
    fn filter_c(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
        _in_lists: &[Option<Vec<Value>>],
    ) -> Result<()> {
        let mut c = SeriesC::default();
        c.filter(idx_num, args);
        self.c = Some(c);
        Ok(())
    }

    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.c = None;
        if (idx_num & SERIES_BIT_START) == 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "first argument to \"generate_series()\" missing or unusable",
            ));
        }

        if args.iter().any(Value::is_null) {
            self.is_eof = true;
            return Ok(());
        }

        let mut arg_pos = 0usize;
        let min_arg = if (idx_num & SERIES_BIT_START) != 0 {
            let v = args.get(arg_pos).map_or(0, value_to_i64);
            arg_pos += 1;
            v
        } else {
            0
        };
        let max_arg = if (idx_num & SERIES_BIT_STOP) != 0 {
            let v = args.get(arg_pos).map_or(0xffff_ffff, value_to_i64);
            arg_pos += 1;
            v
        } else {
            0xffff_ffff
        };
        let step_arg = if (idx_num & SERIES_BIT_STEP) != 0 {
            let v = args.get(arg_pos).map_or(1, value_to_i64);
            arg_pos += 1;
            v
        } else {
            1
        };
        let value_range = series_value_range(idx_num, args.get(arg_pos..).unwrap_or(&[]));

        let norm_step = if step_arg == 0 { 1 } else { step_arg };
        self.raw_start = min_arg;
        self.raw_stop = max_arg;
        self.raw_step = norm_step;

        if norm_step < 0 {
            self.is_desc = (idx_num & SERIES_BIT_ASC) == 0;
            if min_arg < max_arg {
                self.is_eof = true;
                return Ok(());
            }
            let abs_step_u = (norm_step as i128).unsigned_abs();
            let span_u = (min_arg as i128 - max_arg as i128) as u128;
            let steps = span_u / abs_step_u;
            let low = (min_arg as i128 - (steps * abs_step_u) as i128) as i64;
            self.low_value = low;
            self.high_value = min_arg;
            self.abs_step = (abs_step_u.min(i64::MAX as u128)) as i64;
        } else {
            self.is_desc = (idx_num & SERIES_BIT_DESC) != 0;
            if min_arg > max_arg {
                self.is_eof = true;
                return Ok(());
            }
            let abs_step_u = norm_step as u128;
            let span_u = (max_arg as i128 - min_arg as i128) as u128;
            let steps = span_u / abs_step_u;
            let high = (min_arg as i128 + (steps * abs_step_u) as i128) as i64;
            self.low_value = min_arg;
            self.high_value = high;
            self.abs_step = norm_step;
        }

        // Keep only the members of the sequence inside the range the `value` constraints
        // allow. The sequence is anchored at START: its low end for a positive step, its
        // high end for a negative one.
        let Some((i_min, i_max)) = value_range else {
            self.is_eof = true;
            return Ok(());
        };
        let step = i128::from(self.abs_step.max(1));
        let (mut low, mut high) = (i128::from(self.low_value), i128::from(self.high_value));
        let (i_min, i_max) = (i128::from(i_min), i128::from(i_max));
        if norm_step < 0 {
            if i_max < high {
                high -= (high - i_max + step - 1) / step * step;
            }
            if i_min > low {
                if i_min > high {
                    self.is_eof = true;
                    return Ok(());
                }
                low = high - (high - i_min) / step * step;
            }
        } else {
            if i_min > low {
                low += (i_min - low + step - 1) / step * step;
            }
            if i_max < high {
                if i_max < low {
                    self.is_eof = true;
                    return Ok(());
                }
                high = low + (i_max - low) / step * step;
            }
        }
        if low > high {
            self.is_eof = true;
            return Ok(());
        }
        self.low_value = low as i64;
        self.high_value = high as i64;

        self.is_eof = false;
        self.current_value = if self.is_desc {
            self.high_value
        } else {
            self.low_value
        };
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if let Some(c) = self.c.as_mut() {
            c.next();
            return Ok(());
        }
        if self.is_eof {
            return Ok(());
        }
        if self.is_desc {
            if let Some(next_val) = self.current_value.checked_sub(self.abs_step)
                && next_val >= self.low_value
            {
                self.current_value = next_val;
                return Ok(());
            }
            self.is_eof = true;
        } else {
            if let Some(next_val) = self.current_value.checked_add(self.abs_step)
                && next_val <= self.high_value
            {
                self.current_value = next_val;
                return Ok(());
            }
            self.is_eof = true;
        }
        Ok(())
    }

    fn eof(&self) -> bool {
        match &self.c {
            Some(c) => c.b_done,
            None => self.is_eof,
        }
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        if let Some(c) = &self.c {
            // seriesColumn
            return Ok(Value::Integer(match col_idx {
                1 => c.i_obase,
                2 => c.i_oterm,
                3 => c.i_ostep,
                _ => c.i_value,
            }));
        }
        let val = match col_idx {
            0 => Value::Integer(self.current_value),
            1 => Value::Integer(self.raw_start),
            2 => Value::Integer(self.raw_stop),
            3 => Value::Integer(self.raw_step),
            _ => Value::Null,
        };
        Ok(val)
    }

    fn rowid(&self) -> Result<i64> {
        if let Some(c) = &self.c {
            return Ok(c.i_value);
        }
        Ok(self.current_value)
    }
}
