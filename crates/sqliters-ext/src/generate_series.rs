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

        for (i, constraint) in info.constraints.iter().enumerate() {
            if constraint.i_column < 1 || constraint.i_column > 3 {
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
        for c_idx in arg_indices.into_iter().flatten() {
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
            args.get(arg_pos).map_or(1, value_to_i64)
        } else {
            1
        };

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
