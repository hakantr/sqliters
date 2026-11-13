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

#[derive(Debug, Default)]
struct GenerateSeriesCursor {
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
    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
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
        self.is_eof
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
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
        Ok(self.current_value)
    }
}
