//! Built-in SQLite scalar functions, `LIKE` / `GLOB` pattern matcher, and aggregate accumulators
//! ported from `sqlite/src/func.c`.
#![forbid(unsafe_code)]

use std::cell::Cell;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

thread_local! {
    static TEST_FUNCTIONS_ENABLED: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard enabling `testfixture` SQL functions (`randstr`) during `prepare` on connections
/// configured with `enable_test_functions`.
pub struct TestFunctionsScope {
    prev: bool,
}

impl TestFunctionsScope {
    pub fn enter(enabled: bool) -> Self {
        let prev = TEST_FUNCTIONS_ENABLED.with(|c| c.replace(enabled));
        Self { prev }
    }
}

impl Drop for TestFunctionsScope {
    fn drop(&mut self) {
        TEST_FUNCTIONS_ENABLED.with(|c| c.set(self.prev));
    }
}

static PRNG_STATE: AtomicU64 = AtomicU64::new(0);

fn next_prng_u64() -> u64 {
    let mut cur = PRNG_STATE.load(AtomicOrdering::Relaxed);
    if cur == 0 {
        cur = (system_utc_millis() as u64) ^ 0x9E37_79B9_7F4A_7C15;
        if cur == 0 {
            cur = 0xDEAD_BEEF_CAFE_BABE;
        }
    }
    let mut x = cur;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let next = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    PRNG_STATE.store(if next == 0 { 1 } else { next }, AtomicOrdering::Relaxed);
    next
}

use sqliters_types::json::{
    EditMode, JsonGroupAccum, eval_json, eval_json_array, eval_json_array_length, eval_json_arrow,
    eval_json_edit, eval_json_error_position, eval_json_extract, eval_json_object, eval_json_patch,
    eval_json_pretty, eval_json_quote, eval_json_remove, eval_json_type, eval_json_valid,
};
use sqliters_types::{
    AggregateStepId, Collation, ErrorCode, FunctionFlags, Result, RuntimeValue, SQLITE_SOURCE_ID,
    SQLITE_VERSION, SqliteError, TextEncoding, Value, enforce_result_subtype_contract,
    format_sqlite_real, strip_unsubscribed_subtypes,
};

/// Connection-level execution metadata exposed to SQL functions (`last_insert_rowid()`, `changes()`,
/// and statement-consistent UTC timestamp `stmt_timestamp_ms`).
#[derive(Debug, Clone, Copy, Default)]
pub struct VmRuntimeContext {
    pub last_insert_rowid: i64,
    pub last_changes: u64,
    pub total_changes: u64,
    /// UTC Unix epoch milliseconds captured at the start of statement execution (`sqlite3StmtCurrentTime`).
    pub stmt_timestamp_ms: Option<i64>,
    /// Enables `testfixture` SQL functions (`randstr`, `sqlite/src/test_func.c`).
    pub enable_test_functions: bool,
}

impl VmRuntimeContext {
    /// Returns the statement's frozen UTC timestamp in Unix epoch milliseconds,
    /// falling back to `SystemTime::now()` if not explicitly initialized.
    #[inline]
    pub fn effective_timestamp_ms(&self) -> i64 {
        self.stmt_timestamp_ms.unwrap_or_else(system_utc_millis)
    }
}

/// Current OS UTC clock in Unix epoch milliseconds.
pub fn system_utc_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Converts Unix epoch seconds into proleptic Gregorian `(year, month 1..=12, day 1..=31, hour, min, sec)` in UTC
/// (Howard Hinnant's `civil_from_days` algorithm, matching SQLite `computeYMD` / `computeHMS` in `date.c`).
pub fn utc_ymdhms_from_unix_ms(unix_ms: i64) -> (i32, u32, u32, u32, u32, u32) {
    let total_secs = unix_ms.div_euclid(1_000);
    let days = total_secs.div_euclid(86_400);
    let sec_of_day = total_secs.rem_euclid(86_400) as u32;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let mut y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    if m <= 2 {
        y += 1;
    }

    let hh = sec_of_day / 3_600;
    let mm = (sec_of_day % 3_600) / 60;
    let ss = sec_of_day % 60;
    (y as i32, m, d, hh, mm, ss)
}

pub fn format_utc_date(unix_ms: i64) -> String {
    let (y, m, d, _, _, _) = utc_ymdhms_from_unix_ms(unix_ms);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn format_utc_time(unix_ms: i64) -> String {
    let (_, _, _, hh, mm, ss) = utc_ymdhms_from_unix_ms(unix_ms);
    format!("{hh:02}:{mm:02}:{ss:02}")
}

pub fn format_utc_timestamp(unix_ms: i64) -> String {
    let (y, m, d, hh, mm, ss) = utc_ymdhms_from_unix_ms(unix_ms);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

#[inline]
fn with_ascii_upper<R>(name: &str, f: impl FnOnce(&str) -> R) -> R {
    let bytes = name.as_bytes();
    if bytes.iter().all(|b| !b.is_ascii_lowercase()) {
        return f(name);
    }
    if bytes.len() <= 64 {
        let mut buf = [0u8; 64];
        for (dst, &src) in buf[..bytes.len()].iter_mut().zip(bytes) {
            *dst = src.to_ascii_uppercase();
        }
        let s = std::str::from_utf8(&buf[..bytes.len()]).unwrap_or(name);
        return f(s);
    }
    let owned = name.to_ascii_uppercase();
    f(&owned)
}

#[inline]
fn builtin_scalar_flags_upper(upper: &str, enable_test_functions: bool) -> Option<FunctionFlags> {
    match upper {
        "SUBTYPE" => {
            Some(FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS | FunctionFlags::SUBTYPE)
        }
        "COALESCE" | "IFNULL" | "NULLIF" | "IIF" | "IF" | "MIN" | "MAX" | "LIKELY" | "UNLIKELY"
        | "LIKELIHOOD" | "->" | "->>" | "JSON" | "JSON_ARRAY" | "JSON_ARRAY_INSERT"
        | "JSON_EXTRACT" | "JSON_INSERT" | "JSON_OBJECT" | "JSON_PATCH" | "JSON_QUOTE"
        | "JSON_REMOVE" | "JSON_REPLACE" | "JSON_SET" => Some(
            FunctionFlags::DETERMINISTIC
                | FunctionFlags::INNOCUOUS
                | FunctionFlags::SUBTYPE
                | FunctionFlags::RESULT_SUBTYPE,
        ),
        "JSONB_ARRAY" | "JSONB_ARRAY_INSERT" | "JSONB_INSERT" | "JSONB_OBJECT"
        | "JSONB_REPLACE" | "JSONB_SET" => {
            Some(FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS | FunctionFlags::SUBTYPE)
        }
        "TEST_SET_SUBTYPE" if enable_test_functions => Some(
            FunctionFlags::DETERMINISTIC | FunctionFlags::SUBTYPE | FunctionFlags::RESULT_SUBTYPE,
        ),
        "TEST_BAD_SUBTYPE" if enable_test_functions => {
            Some(FunctionFlags::DETERMINISTIC | FunctionFlags::SUBTYPE)
        }
        "TEST_PLAIN_ARG_SUBTYPE" if enable_test_functions => {
            Some(FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS)
        }
        "RANDSTR" | "TEST_ERROR" if enable_test_functions => Some(FunctionFlags::INNOCUOUS),
        "RANDOM" | "RANDOMBLOB" | "LAST_INSERT_ROWID" | "CHANGES" | "TOTAL_CHANGES" => {
            Some(FunctionFlags::INNOCUOUS)
        }
        "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP" | "DATE" | "TIME" | "DATETIME"
        | "UNIXEPOCH" | "JULIANDAY" | "STRFTIME" => Some(FunctionFlags::INNOCUOUS),
        _ => scalar_function_arity_with_flag(upper, enable_test_functions)
            .map(|_| FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS),
    }
}

/// Returns the [`FunctionFlags`] metadata for a built-in scalar SQL function (`func.c`), or
/// `None` if `name` is not a recognized built-in scalar function.
pub fn builtin_scalar_flags(name: &str, enable_test_functions: bool) -> Option<FunctionFlags> {
    with_ascii_upper(name, |upper| {
        builtin_scalar_flags_upper(upper, enable_test_functions)
    })
}

/// Evaluates a built-in SQLite scalar function with [`RuntimeValue`] subtype semantics and strict
/// `SQLITE_STRICT_SUBTYPE` checking enabled (`func.c`, `vdbeapi.c`).
pub fn eval_scalar_function_rt(
    name: &str,
    args: &[RuntimeValue],
    ctx: &VmRuntimeContext,
) -> Result<RuntimeValue> {
    eval_scalar_function_rt_with_strict(name, args, ctx, true)
}

/// Evaluates a built-in SQLite scalar function with [`RuntimeValue`] subtype semantics and
/// configurable `strict_subtype` checking.
pub fn eval_scalar_function_rt_with_strict(
    name: &str,
    args: &[RuntimeValue],
    ctx: &VmRuntimeContext,
    strict_subtype: bool,
) -> Result<RuntimeValue> {
    with_ascii_upper(name, |upper| {
        eval_scalar_function_rt_upper(name, upper, args, ctx, strict_subtype)
    })
}

fn eval_scalar_function_rt_upper(
    name: &str,
    upper: &str,
    args: &[RuntimeValue],
    ctx: &VmRuntimeContext,
    strict_subtype: bool,
) -> Result<RuntimeValue> {
    let flags = builtin_scalar_flags_upper(upper, ctx.enable_test_functions)
        .ok_or_else(|| SqliteError::new(ErrorCode::Error, format!("no such function: {name}")))?;
    let mut scratch = Vec::new();
    let filtered = strip_unsubscribed_subtypes(flags, args, &mut scratch);

    let raw_out = match upper {
        "SUBTYPE" => {
            check_arity_rt(name, filtered, 1, 1)?;
            RuntimeValue::plain(Value::Integer(i64::from(filtered[0].subtype)))
        }
        "TEST_SET_SUBTYPE" if ctx.enable_test_functions => {
            check_arity_rt(name, filtered, 2, 2)?;
            let sub = filtered[1].value.to_integer_lossy() as u32;
            RuntimeValue::with_subtype(filtered[0].value.clone(), sub)
        }
        "TEST_BAD_SUBTYPE" if ctx.enable_test_functions => {
            check_arity_rt(name, filtered, 2, 2)?;
            let sub = filtered[1].value.to_integer_lossy() as u32;
            RuntimeValue::with_subtype(filtered[0].value.clone(), sub)
        }
        "TEST_PLAIN_ARG_SUBTYPE" if ctx.enable_test_functions => {
            check_arity_rt(name, filtered, 1, 1)?;
            RuntimeValue::plain(Value::Integer(i64::from(filtered[0].subtype)))
        }
        "TEST_ERROR" if ctx.enable_test_functions => {
            check_arity_rt(name, filtered, 1, 2)?;
            let msg = if filtered[0].value.is_null() {
                String::new()
            } else {
                filtered[0].value.to_text_lossy()
            };
            let err = if filtered.len() == 2 {
                let raw_code = filtered[1].value.to_integer_lossy() as i32;
                SqliteError::with_extended(raw_code, msg)
            } else {
                SqliteError::new(ErrorCode::Error, msg)
            };
            return Err(err);
        }
        "COALESCE" | "IFNULL" => {
            let min = 2;
            let max = if upper == "IFNULL" { 2 } else { usize::MAX };
            check_arity_rt(name, filtered, min, max)?;
            filtered
                .iter()
                .find(|rv| !rv.value.is_null())
                .cloned()
                .unwrap_or_else(RuntimeValue::null)
        }
        "NULLIF" => {
            check_arity_rt(name, filtered, 2, 2)?;
            if filtered[0]
                .value
                .cmp_sqlite(&filtered[1].value, Collation::Binary)
                .is_eq()
            {
                RuntimeValue::null()
            } else {
                filtered[0].clone()
            }
        }
        "IIF" | "IF" => {
            check_arity_rt(name, filtered, 2, usize::MAX)?;
            let mut idx = 0;
            let mut matched = None;
            while idx + 1 < filtered.len() {
                if filtered[idx].value.truthiness() == Some(true) {
                    matched = Some(filtered[idx + 1].clone());
                    break;
                }
                idx += 2;
            }
            matched.unwrap_or_else(|| {
                if idx < filtered.len() {
                    filtered[idx].clone()
                } else {
                    RuntimeValue::null()
                }
            })
        }
        "LIKELY" | "UNLIKELY" => {
            check_arity_rt(name, filtered, 1, 1)?;
            filtered[0].clone()
        }
        "LIKELIHOOD" => {
            check_arity_rt(name, filtered, 2, 2)?;
            match filtered[1].value {
                Value::Real(p) if (0.0..=1.0).contains(&p) => {}
                _ => {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "second argument to likelihood() must be a constant between 0.0 and 1.0",
                    ));
                }
            }
            filtered[0].clone()
        }
        "MAX" => {
            check_arity_rt(name, filtered, 2, usize::MAX)?;
            let mut best = &filtered[0];
            let mut has_null = false;
            for rv in filtered {
                if rv.value.is_null() {
                    has_null = true;
                    break;
                }
                if rv.value.cmp_sqlite(&best.value, Collation::Binary).is_gt() {
                    best = rv;
                }
            }
            if has_null {
                RuntimeValue::null()
            } else {
                best.clone()
            }
        }
        "MIN" => {
            check_arity_rt(name, filtered, 2, usize::MAX)?;
            let mut best = &filtered[0];
            let mut has_null = false;
            for rv in filtered {
                if rv.value.is_null() {
                    has_null = true;
                    break;
                }
                if rv.value.cmp_sqlite(&best.value, Collation::Binary).is_lt() {
                    best = rv;
                }
            }
            if has_null {
                RuntimeValue::null()
            } else {
                best.clone()
            }
        }
        "->" | "->>" => {
            check_arity_rt(name, filtered, 2, 2)?;
            eval_json_arrow(&filtered[0].value, &filtered[1].value, upper == "->>")?
        }
        "JSON" | "JSONB" => {
            check_arity_rt(name, filtered, 1, 1)?;
            eval_json(&filtered[0], upper == "JSONB")?
        }
        "JSON_ARRAY" | "JSONB_ARRAY" => eval_json_array(filtered, upper == "JSONB_ARRAY")?,
        "JSON_OBJECT" | "JSONB_OBJECT" => eval_json_object(filtered, upper == "JSONB_OBJECT")?,
        "JSON_QUOTE" => {
            check_arity_rt(name, filtered, 1, 1)?;
            eval_json_quote(&filtered[0])?
        }
        "JSON_ARRAY_LENGTH" => {
            check_arity_rt(name, filtered, 1, 2)?;
            eval_json_array_length(filtered)?
        }
        "JSON_TYPE" => {
            check_arity_rt(name, filtered, 1, 2)?;
            eval_json_type(filtered)?
        }
        "JSON_VALID" => {
            check_arity_rt(name, filtered, 1, 2)?;
            eval_json_valid(filtered)?
        }
        "JSON_ERROR_POSITION" => {
            check_arity_rt(name, filtered, 1, 1)?;
            eval_json_error_position(&filtered[0])?
        }
        "JSON_EXTRACT" | "JSONB_EXTRACT" => {
            check_arity_rt(name, filtered, 0, usize::MAX)?;
            eval_json_extract(filtered, upper == "JSONB_EXTRACT")?
        }
        "JSON_INSERT" | "JSONB_INSERT" => {
            check_arity_rt(name, filtered, 1, usize::MAX)?;
            eval_json_edit(name, filtered, EditMode::Insert, upper == "JSONB_INSERT")?
        }
        "JSON_REPLACE" | "JSONB_REPLACE" => {
            check_arity_rt(name, filtered, 1, usize::MAX)?;
            eval_json_edit(name, filtered, EditMode::Replace, upper == "JSONB_REPLACE")?
        }
        "JSON_SET" | "JSONB_SET" => {
            check_arity_rt(name, filtered, 1, usize::MAX)?;
            eval_json_edit(name, filtered, EditMode::Set, upper == "JSONB_SET")?
        }
        "JSON_ARRAY_INSERT" | "JSONB_ARRAY_INSERT" => {
            check_arity_rt(name, filtered, 1, usize::MAX)?;
            eval_json_edit(
                name,
                filtered,
                EditMode::ArrayInsert,
                upper == "JSONB_ARRAY_INSERT",
            )?
        }
        "JSON_REMOVE" | "JSONB_REMOVE" => {
            check_arity_rt(name, filtered, 1, usize::MAX)?;
            eval_json_remove(filtered, upper == "JSONB_REMOVE")?
        }
        "JSON_PATCH" | "JSONB_PATCH" => {
            check_arity_rt(name, filtered, 2, 2)?;
            eval_json_patch(&filtered[0], &filtered[1], upper == "JSONB_PATCH")?
        }
        "JSON_PRETTY" => {
            check_arity_rt(name, filtered, 1, 2)?;
            eval_json_pretty(filtered)?
        }
        _ => {
            let plain_vals: Vec<Value> = filtered.iter().map(|rv| rv.value.clone()).collect();
            let v = eval_scalar_function_plain(name, upper, &plain_vals, ctx)?;
            RuntimeValue::plain(v)
        }
    };

    if raw_out.subtype != 0 && !flags.contains(FunctionFlags::RESULT_SUBTYPE) && strict_subtype {
        let canonical = name.to_ascii_lowercase();
        return enforce_result_subtype_contract(&canonical, flags, strict_subtype, raw_out);
    }
    Ok(raw_out)
}

/// Evaluates a built-in SQLite scalar function (`func.c`), discarding any ephemeral result subtype.
pub fn eval_scalar_function(name: &str, args: &[Value], ctx: &VmRuntimeContext) -> Result<Value> {
    let rt_args: Vec<RuntimeValue> = args.iter().cloned().map(RuntimeValue::plain).collect();
    eval_scalar_function_rt(name, &rt_args, ctx).map(|rv| rv.value)
}

fn eval_scalar_function_plain(
    name: &str,
    upper: &str,
    args: &[Value],
    ctx: &VmRuntimeContext,
) -> Result<Value> {
    match upper {
        "TYPEOF" => {
            check_arity(name, args, 1, 1)?;
            Ok(Value::Text(args[0].storage_class().typeof_str().to_owned()))
        }
        "ABS" => {
            check_arity(name, args, 1, 1)?;
            match &args[0] {
                Value::Null => Ok(Value::Null),
                Value::Integer(i) => i
                    .checked_abs()
                    .map(Value::Integer)
                    .ok_or_else(|| SqliteError::new(ErrorCode::Mismatch, "integer overflow")),
                other => Ok(Value::real(other.to_real_lossy().abs())),
            }
        }
        "SIGN" => {
            check_arity(name, args, 1, 1)?;
            match &args[0] {
                Value::Null => Ok(Value::Null),
                Value::Integer(i) => Ok(Value::Integer(i.signum())),
                Value::Real(r) => {
                    let s = if *r > 0.0 {
                        1
                    } else if *r < 0.0 {
                        -1
                    } else {
                        0
                    };
                    Ok(Value::Integer(s))
                }
                Value::Text(s) => {
                    if let Some(num) = sqliters_types::parse_numeric_text(s) {
                        eval_scalar_function("SIGN", &[num], ctx)
                    } else {
                        Ok(Value::Null)
                    }
                }
                Value::Blob(_) => Ok(Value::Null),
            }
        }
        "COALESCE" | "IFNULL" => {
            let min = 2;
            let max = if upper == "IFNULL" { 2 } else { usize::MAX };
            check_arity(name, args, min, max)?;
            for v in args {
                if !v.is_null() {
                    return Ok(v.clone());
                }
            }
            Ok(Value::Null)
        }
        "NULLIF" => {
            check_arity(name, args, 2, 2)?;
            if args[0].cmp_sqlite(&args[1], Collation::Binary).is_eq() {
                Ok(Value::Null)
            } else {
                Ok(args[0].clone())
            }
        }
        "IIF" | "IF" => {
            check_arity(name, args, 2, usize::MAX)?;
            let mut idx = 0;
            while idx + 1 < args.len() {
                if args[idx].truthiness() == Some(true) {
                    return Ok(args[idx + 1].clone());
                }
                idx += 2;
            }
            if idx < args.len() {
                Ok(args[idx].clone())
            } else {
                Ok(Value::Null)
            }
        }
        "LENGTH" => {
            check_arity(name, args, 1, 1)?;
            match &args[0] {
                Value::Null => Ok(Value::Null),
                Value::Blob(b) => Ok(Value::Integer(b.len() as i64)),
                other => {
                    let s = other.to_text_lossy();
                    let before_nul = s.split('\0').next().unwrap_or("");
                    Ok(Value::Integer(before_nul.chars().count() as i64))
                }
            }
        }
        "OCTET_LENGTH" => {
            check_arity(name, args, 1, 1)?;
            match &args[0] {
                Value::Null => Ok(Value::Null),
                Value::Blob(b) => Ok(Value::Integer(b.len() as i64)),
                other => Ok(Value::Integer(other.to_text_lossy().len() as i64)),
            }
        }
        "LOWER" => {
            check_arity(name, args, 1, 1)?;
            if args[0].is_null() {
                Ok(Value::Null)
            } else {
                Ok(Value::Text(args[0].to_text_lossy().to_ascii_lowercase()))
            }
        }
        "UPPER" => {
            check_arity(name, args, 1, 1)?;
            if args[0].is_null() {
                Ok(Value::Null)
            } else {
                Ok(Value::Text(args[0].to_text_lossy().to_ascii_uppercase()))
            }
        }
        "TRIM" | "LTRIM" | "RTRIM" => {
            check_arity(name, args, 1, 2)?;
            if args[0].is_null() || (args.len() == 2 && args[1].is_null()) {
                return Ok(Value::Null);
            }
            let s = args[0].to_text_lossy();
            let cutset: Vec<char> = if args.len() == 2 {
                args[1].to_text_lossy().chars().collect()
            } else {
                vec![' ']
            };
            let trimmed = match upper {
                "LTRIM" => s.trim_start_matches(cutset.as_slice()),
                "RTRIM" => s.trim_end_matches(cutset.as_slice()),
                _ => s.trim_matches(cutset.as_slice()),
            };
            Ok(Value::Text(trimmed.to_owned()))
        }
        "SUBSTR" | "SUBSTRING" => {
            check_arity(name, args, 2, 3)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let chars: Vec<char> = args[0].to_text_lossy().chars().collect();
            let total = chars.len() as i64;
            let raw_start = args[1].to_integer_lossy();
            let raw_len = args.get(2).map_or(total, Value::to_integer_lossy);

            // SQLite 1-based substr indexing rules (including 0 and negative offsets!)
            let (start_idx, count) = if raw_start > 0 {
                ((raw_start - 1), raw_len)
            } else if raw_start == 0 {
                (0, raw_len - 1)
            } else {
                let s = total + raw_start;
                if s < 0 {
                    (0, raw_len + s)
                } else {
                    (s, raw_len)
                }
            };
            if count <= 0 || start_idx >= total {
                return Ok(Value::Text(String::new()));
            }
            let st = start_idx.max(0) as usize;
            let en = ((start_idx + count).min(total)) as usize;
            Ok(Value::Text(chars[st..en].iter().collect()))
        }
        "REPLACE" => {
            check_arity(name, args, 3, 3)?;
            if args.iter().any(Value::is_null) {
                return Ok(Value::Null);
            }
            let hay = args[0].to_text_lossy();
            let needle = args[1].to_text_lossy();
            if needle.is_empty() {
                return Ok(Value::Text(hay));
            }
            let rep = args[2].to_text_lossy();
            Ok(Value::Text(hay.replace(&needle, &rep)))
        }
        "INSTR" => {
            check_arity(name, args, 2, 2)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let hay = args[0].to_text_lossy();
            let needle = args[1].to_text_lossy();
            if needle.is_empty() {
                return Ok(Value::Integer(1));
            }
            let pos = hay
                .find(&needle)
                .map_or(0, |byte_off| hay[..byte_off].chars().count() as i64 + 1);
            Ok(Value::Integer(pos))
        }
        "HEX" => {
            check_arity(name, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            const HEX_UPPER: &[u8; 16] = b"0123456789ABCDEF";
            let write_hex = |src: &[u8]| -> String {
                let mut out = String::with_capacity(src.len() * 2);
                for &b in src {
                    out.push(HEX_UPPER[(b >> 4) as usize] as char);
                    out.push(HEX_UPPER[(b & 0x0f) as usize] as char);
                }
                out
            };
            let out = match &args[0] {
                Value::Blob(b) => write_hex(b),
                Value::Text(s) => write_hex(s.as_bytes()),
                Value::Integer(i) => {
                    let mut buf = [0u8; 24];
                    write_hex(format_i64_into_stack(*i, &mut buf))
                }
                other => write_hex(other.to_text_lossy().as_bytes()),
            };
            Ok(Value::Text(out))
        }
        "UNHEX" => {
            check_arity(name, args, 1, 2)?;
            if args[0].is_null() || (args.len() == 2 && args[1].is_null()) {
                return Ok(Value::Null);
            }
            let s = args[0].to_text_lossy();
            if args.len() == 1 {
                if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Ok(Value::Null);
                }
                let mut out = Vec::with_capacity(s.len() / 2);
                for pair in s.as_bytes().as_chunks::<2>().0 {
                    let byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16)
                        .unwrap_or(0);
                    out.push(byte);
                }
                Ok(Value::Blob(out))
            } else {
                let ignore = args[1].to_text_lossy();
                let mut out = Vec::with_capacity(s.len() / 2);
                let mut chars = s.chars().peekable();
                while let Some(c) = chars.next() {
                    if ignore.contains(c) {
                        continue;
                    }
                    let Some(hi) = c.to_digit(16) else {
                        return Ok(Value::Null);
                    };
                    let Some(c2) = chars.next() else {
                        return Ok(Value::Null);
                    };
                    let Some(lo) = c2.to_digit(16) else {
                        return Ok(Value::Null);
                    };
                    out.push(((hi << 4) | lo) as u8);
                }
                Ok(Value::Blob(out))
            }
        }
        "UNICODE" => {
            check_arity(name, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let s = args[0].to_text_lossy();
            Ok(s.chars()
                .next()
                .map_or(Value::Null, |c| Value::Integer(u32::from(c) as i64)))
        }
        "CHAR" => {
            let mut s = String::with_capacity(args.len());
            for a in args {
                if !a.is_null() {
                    let cp = a.to_integer_lossy() as u32;
                    s.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                }
            }
            Ok(Value::Text(s))
        }
        "QUOTE" => {
            check_arity(name, args, 1, 1)?;
            let rendered = match &args[0] {
                Value::Null => "NULL".to_owned(),
                Value::Integer(i) => i.to_string(),
                Value::Real(r) => format_sqlite_real(*r),
                Value::Text(s) => {
                    let before_nul = s.split('\0').next().unwrap_or("");
                    format!("'{}'", before_nul.replace('\'', "''"))
                }
                Value::Blob(b) => {
                    let mut hex = String::with_capacity(b.len() * 2 + 3);
                    hex.push_str("X'");
                    for byte in b {
                        hex.push_str(&format!("{byte:02X}"));
                    }
                    hex.push('\'');
                    hex
                }
            };
            Ok(Value::Text(rendered))
        }
        "ZEROBLOB" => {
            check_arity(name, args, 1, 1)?;
            let raw_n = args[0].to_integer_lossy().max(0);
            if raw_n > 1_000_000_000 {
                return Err(SqliteError::new(
                    ErrorCode::TooBig,
                    "string or blob too big",
                ));
            }
            Ok(Value::Blob(vec![0u8; raw_n as usize]))
        }
        "RANDOM" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Integer(next_prng_u64() as i64))
        }
        "RANDOMBLOB" => {
            check_arity(name, args, 1, 1)?;
            let raw_n = args[0].to_integer_lossy().max(1);
            if raw_n > 1_000_000_000 {
                return Err(SqliteError::new(
                    ErrorCode::TooBig,
                    "string or blob too big",
                ));
            }
            let mut buf = vec![0u8; raw_n as usize];
            for chunk in buf.chunks_mut(8) {
                let bytes = next_prng_u64().to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
            Ok(Value::Blob(buf))
        }
        "RANDSTR" if ctx.enable_test_functions => {
            check_arity(name, args, 2, 2)?;
            const Z_SRC: &[u8] =
                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-!,:*^+=_|?/<> ";
            let mut i_min = args[0].to_integer_lossy().clamp(0, 999) as usize;
            let mut i_max = args[1].to_integer_lossy();
            if i_max < i_min as i64 {
                i_max = i_min as i64;
            }
            let i_max = i_max.min(999) as usize;
            if i_min > i_max {
                i_min = i_max;
            }
            let mut n = i_min;
            if i_max > i_min {
                let r = (next_prng_u64() as u32) & 0x7fff_ffff;
                n += (r as usize) % (i_max + 1 - i_min);
            }
            let mut out = String::with_capacity(n);
            let mut rem = n;
            while rem > 0 {
                let bytes = next_prng_u64().to_le_bytes();
                let take = rem.min(8);
                for &b in &bytes[..take] {
                    out.push(Z_SRC[(b as usize) % Z_SRC.len()] as char);
                }
                rem -= take;
            }
            Ok(Value::Text(out))
        }
        "ROUND" => {
            check_arity(name, args, 1, 2)?;
            if args[0].is_null() || (args.len() == 2 && args[1].is_null()) {
                return Ok(Value::Null);
            }
            let x = args[0].to_real_lossy();
            let digits = args
                .get(1)
                .map_or(0, Value::to_integer_lossy)
                .clamp(-30, 30);
            if digits <= 0 {
                // SQLite rounds half away from zero:
                let r = if x >= 0.0 {
                    (x + 0.5).floor()
                } else {
                    (x - 0.5).ceil()
                };
                Ok(Value::real(r))
            } else {
                let scale = 10_f64.powi(digits as i32);
                let scaled = x * scale;
                let r = if scaled >= 0.0 {
                    (scaled + 0.5).floor() / scale
                } else {
                    (scaled - 0.5).ceil() / scale
                };
                Ok(Value::real(r))
            }
        }
        "MAX" => {
            // Multi-argument scalar MAX(x, y, ...): returns NULL if ANY argument is NULL in SQLite!
            check_arity(name, args, 2, usize::MAX)?;
            let mut best = &args[0];
            for v in args {
                if v.is_null() {
                    return Ok(Value::Null);
                }
                if v.cmp_sqlite(best, Collation::Binary).is_gt() {
                    best = v;
                }
            }
            Ok(best.clone())
        }
        "MIN" => {
            // Multi-argument scalar MIN(x, y, ...): returns NULL if ANY argument is NULL in SQLite!
            check_arity(name, args, 2, usize::MAX)?;
            let mut best = &args[0];
            for v in args {
                if v.is_null() {
                    return Ok(Value::Null);
                }
                if v.cmp_sqlite(best, Collation::Binary).is_lt() {
                    best = v;
                }
            }
            Ok(best.clone())
        }
        "PRINTF" | "FORMAT" => {
            check_arity(name, args, 1, usize::MAX)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Text(simple_sqlite_printf(
                &args[0].to_text_lossy(),
                &args[1..],
            )))
        }
        "LIKE" => {
            check_arity(name, args, 2, 3)?;
            // Note: SQLite's function form is LIKE(pattern, string [, escape])!
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let esc = if args.len() == 3 {
                if args[2].is_null() {
                    return Ok(Value::Null);
                }
                Some(parse_escape_char(&args[2].to_text_lossy())?)
            } else {
                None
            };
            let matched =
                sqlite_like_match(&args[0].to_text_lossy(), &args[1].to_text_lossy(), esc);
            Ok(Value::Integer(i64::from(matched)))
        }
        "GLOB" => {
            check_arity(name, args, 2, 2)?;
            if args[0].is_null() || args[1].is_null() {
                return Ok(Value::Null);
            }
            let matched = sqlite_glob_match(&args[0].to_text_lossy(), &args[1].to_text_lossy());
            Ok(Value::Integer(i64::from(matched)))
        }
        "SQLITE_VERSION" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Text(SQLITE_VERSION.to_owned()))
        }
        "SQLITE_SOURCE_ID" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Text(SQLITE_SOURCE_ID.to_owned()))
        }
        "LAST_INSERT_ROWID" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Integer(ctx.last_insert_rowid))
        }
        "CHANGES" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Integer(ctx.last_changes as i64))
        }
        "TOTAL_CHANGES" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Integer(ctx.total_changes as i64))
        }
        "CURRENT_DATE" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Text(format_utc_date(ctx.effective_timestamp_ms())))
        }
        "CURRENT_TIME" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Text(format_utc_time(ctx.effective_timestamp_ms())))
        }
        "CURRENT_TIMESTAMP" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Text(format_utc_timestamp(
                ctx.effective_timestamp_ms(),
            )))
        }
        "DATE" | "TIME" | "DATETIME" | "UNIXEPOCH" | "JULIANDAY" => {
            eval_datetime_function(upper, args, ctx)
        }
        "STRFTIME" => eval_strftime_function(args, ctx),
        "PI" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::real(std::f64::consts::PI))
        }
        "CEIL" | "CEILING" | "FLOOR" | "TRUNC" => {
            check_arity(name, args, 1, 1)?;
            let Some(nv) = math_numeric_value(&args[0]) else {
                return Ok(Value::Null);
            };
            match nv {
                Value::Integer(i) => Ok(Value::Integer(i)),
                Value::Real(r) => {
                    let res = match upper {
                        "CEIL" | "CEILING" => r.ceil(),
                        "FLOOR" => r.floor(),
                        _ => r.trunc(),
                    };
                    Ok(Value::real(res))
                }
                _ => Ok(Value::Null),
            }
        }
        "ACOS" | "ACOSH" | "ASIN" | "ASINH" | "ATAN" | "ATANH" | "COS" | "COSH" | "DEGREES"
        | "EXP" | "LN" | "LOG10" | "LOG2" | "RADIANS" | "SIN" | "SINH" | "SQRT" | "TAN"
        | "TANH" => {
            check_arity(name, args, 1, 1)?;
            let Some(x) = math_real_arg(&args[0]) else {
                return Ok(Value::Null);
            };
            let res = match upper {
                "ACOS" => x.acos(),
                "ACOSH" => x.acosh(),
                "ASIN" => x.asin(),
                "ASINH" => x.asinh(),
                "ATAN" => x.atan(),
                "ATANH" => x.atanh(),
                "COS" => x.cos(),
                "COSH" => x.cosh(),
                "DEGREES" => x.to_degrees(),
                "EXP" => x.exp(),
                "LN" => x.ln(),
                "LOG10" => x.log10(),
                "LOG2" => x.log2(),
                "RADIANS" => x.to_radians(),
                "SIN" => x.sin(),
                "SINH" => x.sinh(),
                "SQRT" => x.sqrt(),
                "TAN" => x.tan(),
                _ => x.tanh(),
            };
            Ok(Value::real(res))
        }
        "LOG" => {
            check_arity(name, args, 1, 2)?;
            if args.len() == 1 {
                let Some(x) = math_real_arg(&args[0]) else {
                    return Ok(Value::Null);
                };
                Ok(Value::real(x.log10()))
            } else {
                let (Some(b), Some(x)) = (math_real_arg(&args[0]), math_real_arg(&args[1])) else {
                    return Ok(Value::Null);
                };
                if b <= 0.0 || b == 1.0 || x <= 0.0 {
                    Ok(Value::Null)
                } else if b == 2.0 {
                    Ok(Value::real(x.log2()))
                } else if b == 10.0 {
                    Ok(Value::real(x.log10()))
                } else {
                    Ok(Value::real(x.log(b)))
                }
            }
        }
        "POW" | "POWER" | "ATAN2" | "MOD" => {
            check_arity(name, args, 2, 2)?;
            let (Some(a), Some(b)) = (math_real_arg(&args[0]), math_real_arg(&args[1])) else {
                return Ok(Value::Null);
            };
            let res = match upper {
                "POW" | "POWER" => a.powf(b),
                "ATAN2" => a.atan2(b),
                _ => a % b,
            };
            Ok(Value::real(res))
        }
        _ => Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such function: {name}"),
        )),
    }
}

fn check_arity_rt(name: &str, args: &[RuntimeValue], min: usize, max: usize) -> Result<()> {
    if args.len() < min || args.len() > max {
        Err(SqliteError::new(
            ErrorCode::Error,
            format!("wrong number of arguments to function {name}()"),
        ))
    } else {
        Ok(())
    }
}

fn check_arity(name: &str, args: &[Value], min: usize, max: usize) -> Result<()> {
    if args.len() < min || args.len() > max {
        Err(SqliteError::new(
            ErrorCode::Error,
            format!("wrong number of arguments to function {name}()"),
        ))
    } else {
        Ok(())
    }
}

fn scalar_function_arity(upper: &str) -> Option<(usize, usize)> {
    scalar_function_arity_with_flag(upper, TEST_FUNCTIONS_ENABLED.with(Cell::get))
}

fn scalar_function_arity_with_flag(
    upper: &str,
    enable_test_functions: bool,
) -> Option<(usize, usize)> {
    match upper {
        "TYPEOF"
        | "SUBTYPE"
        | "ABS"
        | "SIGN"
        | "LENGTH"
        | "OCTET_LENGTH"
        | "LOWER"
        | "UPPER"
        | "HEX"
        | "UNICODE"
        | "QUOTE"
        | "ZEROBLOB"
        | "RANDOMBLOB"
        | "LIKELY"
        | "UNLIKELY"
        | "JSON"
        | "JSONB"
        | "JSON_QUOTE"
        | "JSON_ERROR_POSITION"
        | "ACOS"
        | "ACOSH"
        | "ASIN"
        | "ASINH"
        | "ATAN"
        | "ATANH"
        | "CEIL"
        | "CEILING"
        | "COS"
        | "COSH"
        | "DEGREES"
        | "EXP"
        | "FLOOR"
        | "LN"
        | "LOG10"
        | "LOG2"
        | "RADIANS"
        | "SIN"
        | "SINH"
        | "SQRT"
        | "TAN"
        | "TANH"
        | "TRUNC" => Some((1, 1)),
        "RANDOM" | "SQLITE_VERSION" | "SQLITE_SOURCE_ID" | "LAST_INSERT_ROWID" | "CHANGES"
        | "TOTAL_CHANGES" | "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP" | "PI" => {
            Some((0, 0))
        }
        "COALESCE" | "IIF" | "IF" => Some((2, usize::MAX)),
        "IFNULL" | "NULLIF" | "INSTR" | "GLOB" | "LIKELIHOOD" | "->" | "->>" | "JSON_PATCH"
        | "JSONB_PATCH" | "POW" | "POWER" | "ATAN2" | "MOD" => Some((2, 2)),
        "RANDSTR" | "TEST_SET_SUBTYPE" | "TEST_BAD_SUBTYPE" if enable_test_functions => {
            Some((2, 2))
        }
        "TEST_ERROR" if enable_test_functions => Some((1, 2)),
        "TEST_PLAIN_ARG_SUBTYPE" if enable_test_functions => Some((1, 1)),
        "REPLACE" => Some((3, 3)),
        "TRIM" | "LTRIM" | "RTRIM" | "ROUND" | "UNHEX" | "LOG" | "JSON_ARRAY_LENGTH"
        | "JSON_TYPE" | "JSON_VALID" | "JSON_PRETTY" => Some((1, 2)),
        "SUBSTR" | "SUBSTRING" | "LIKE" => Some((2, 3)),
        "CHAR" | "DATE" | "TIME" | "DATETIME" | "UNIXEPOCH" | "JULIANDAY" | "STRFTIME"
        | "JSON_ARRAY" | "JSONB_ARRAY" | "JSON_OBJECT" | "JSONB_OBJECT" | "JSON_EXTRACT"
        | "JSONB_EXTRACT" => Some((0, usize::MAX)),
        "PRINTF" | "FORMAT" | "JSON_INSERT" | "JSONB_INSERT" | "JSON_REPLACE" | "JSONB_REPLACE"
        | "JSON_SET" | "JSONB_SET" | "JSON_REMOVE" | "JSONB_REMOVE" | "JSON_ARRAY_INSERT"
        | "JSONB_ARRAY_INSERT" => Some((1, usize::MAX)),
        _ => None,
    }
}

/// Returns `(min_args, max_args)` if `upper` is one of SQLite's 11 built-in window-only functions
/// (`SQLITE_FUNC_WINDOW` in `sqlite/src/window.c:614-630`).
pub fn builtin_window_arity(name: &str) -> Option<(usize, usize)> {
    match name.to_ascii_uppercase().as_str() {
        "ROW_NUMBER" | "RANK" | "DENSE_RANK" | "PERCENT_RANK" | "CUME_DIST" => Some((0, 0)),
        "NTILE" | "FIRST_VALUE" | "LAST_VALUE" => Some((1, 1)),
        "NTH_VALUE" => Some((2, 2)),
        "LEAD" | "LAG" => Some((1, 3)),
        _ => None,
    }
}

/// Returns `true` if `name` (ASCII case-insensitive) is one of SQLite's 11 built-in window-only
/// functions (`row_number`..`nth_value`).
#[inline]
pub fn is_builtin_window_only_function(name: &str) -> bool {
    builtin_window_arity(&name.to_ascii_uppercase()).is_some()
}

/// Validates a SQL function call (`name`, `*` vs `arg_count`, `DISTINCT`, `FILTER`, and whether
/// aggregate functions are permitted in the enclosing SQL clause) at `prepare` time.
pub fn validate_function_call(
    name: &str,
    star_args: bool,
    arg_count: usize,
    distinct: bool,
    has_filter: bool,
    allow_aggregates: bool,
) -> Result<()> {
    let upper = name.to_ascii_uppercase();
    if let Some((min, max)) = builtin_window_arity(&upper) {
        let bad_arity = if max == 0 {
            !star_args && arg_count != 0
        } else {
            star_args || arg_count < min || arg_count > max
        };
        if bad_arity {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("wrong number of arguments to function {name}()"),
            ));
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("misuse of window function {name}()"),
        ));
    }
    match upper.as_str() {
        "COUNT" => {
            if !star_args && arg_count > 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if (star_args || arg_count == 0) && distinct {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "DISTINCT aggregates must have exactly one argument",
                ));
            }
            if !allow_aggregates {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of aggregate function {name}()"),
                ));
            }
            Ok(())
        }
        "SUM" | "TOTAL" | "AVG" | "JSON_GROUP_ARRAY" | "JSONB_GROUP_ARRAY" => {
            if star_args || arg_count != 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if !allow_aggregates {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of aggregate function {name}()"),
                ));
            }
            Ok(())
        }
        "JSON_GROUP_OBJECT" | "JSONB_GROUP_OBJECT" => {
            if star_args || arg_count != 2 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if distinct {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "DISTINCT aggregates must have exactly one argument",
                ));
            }
            if !allow_aggregates {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of aggregate function {name}()"),
                ));
            }
            Ok(())
        }
        "GROUP_CONCAT" | "STRING_AGG" => {
            if star_args || arg_count == 0 || arg_count > 2 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if distinct && arg_count != 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "DISTINCT aggregates must have exactly one argument",
                ));
            }
            if !allow_aggregates {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of aggregate function {name}()"),
                ));
            }
            Ok(())
        }
        "MIN" | "MAX" => {
            if star_args || arg_count == 0 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if arg_count == 1 {
                if !allow_aggregates {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of aggregate function {name}()"),
                    ));
                }
                Ok(())
            } else {
                if distinct {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "DISTINCT aggregates must have exactly one argument",
                    ));
                }
                if has_filter {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("FILTER may not be used with non-aggregate {name}()"),
                    ));
                }
                Ok(())
            }
        }
        _ => {
            let Some((min, max)) = scalar_function_arity(&upper) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such function: {name}"),
                ));
            };
            if star_args || arg_count < min || arg_count > max {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if has_filter {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("FILTER may not be used with non-aggregate {name}()"),
                ));
            }
            if distinct {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("{name}() is not an aggregate function"),
                ));
            }
            Ok(())
        }
    }
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + i64::from(doe) - 719_468
}

fn parse_time_value_to_unix_ms(
    val: &Value,
    modifiers: &[Value],
    ctx: &VmRuntimeContext,
) -> Option<i64> {
    if val.is_null() || modifiers.iter().any(Value::is_null) {
        return None;
    }
    let has_unixepoch = modifiers
        .iter()
        .any(|m| m.to_text_lossy().trim().eq_ignore_ascii_case("unixepoch"));

    let mut ms = match val {
        Value::Integer(i) if has_unixepoch => i.saturating_mul(1_000),
        Value::Real(r) if has_unixepoch => (*r * 1_000.0).round() as i64,
        Value::Integer(jd) => {
            // Julian day -> Unix ms (`2440587.5` JD = Unix epoch)
            ((*jd as f64 - 2_440_587.5) * 86_400_000.0).round() as i64
        }
        Value::Real(jd) => ((*jd - 2_440_587.5) * 86_400_000.0).round() as i64,
        Value::Text(s) => {
            let t = s.trim();
            if t.eq_ignore_ascii_case("now") {
                ctx.effective_timestamp_ms()
            } else if has_unixepoch && let Ok(f) = t.parse::<f64>() {
                (f * 1_000.0).round() as i64
            } else {
                parse_iso_datetime_ms(t)?
            }
        }
        Value::Null | Value::Blob(_) => return None,
    };

    for m in modifiers {
        let mod_str = m.to_text_lossy();
        let mt = mod_str.trim();
        if mt.eq_ignore_ascii_case("unixepoch")
            || mt.eq_ignore_ascii_case("utc")
            || mt.eq_ignore_ascii_case("auto")
        {
            continue;
        }
        ms = apply_datetime_modifier_ms(ms, mt)?;
    }
    Some(ms)
}

fn parse_iso_datetime_ms(s: &str) -> Option<i64> {
    // Supports "YYYY-MM-DD", "YYYY-MM-DD HH:MM[:SS]", "YYYY-MM-DDTHH:MM[:SS][Z]", or "HH:MM[:SS]"
    let s = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix('z'))
        .unwrap_or(s);
    if s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let y: i64 = s.get(0..4)?.parse().ok()?;
        let m: u32 = s.get(5..7)?.parse().ok()?;
        let d: u32 = s.get(8..10)?.parse().ok()?;
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let days = days_from_civil(y, m, d);
        let rest = s.get(10..)?.trim_start_matches([' ', 'T', 't']);
        let hms_ms = if rest.is_empty() {
            0
        } else {
            parse_hms_ms(rest)?
        };
        Some(days * 86_400_000 + hms_ms)
    } else if s.contains(':') {
        // SQLite treats bare "HH:MM:SS" as on 2000-01-01
        let days = days_from_civil(2000, 1, 1);
        Some(days * 86_400_000 + parse_hms_ms(s)?)
    } else {
        None
    }
}

fn parse_hms_ms(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let hh: u32 = parts[0].parse().ok()?;
    let mm: u32 = parts[1].parse().ok()?;
    let (ss, ms) = if parts.len() == 3 {
        if let Some((sec_part, frac_part)) = parts[2].split_once('.') {
            let sec: u32 = sec_part.parse().ok()?;
            if !frac_part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let mut frac_digits: String = frac_part.chars().take(3).collect();
            while frac_digits.len() < 3 {
                frac_digits.push('0');
            }
            let millis: u32 = frac_digits.parse().ok()?;
            (sec, millis)
        } else {
            (parts[2].parse().ok()?, 0)
        }
    } else {
        (0, 0)
    };
    if hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    Some((i64::from(hh) * 3_600 + i64::from(mm) * 60 + i64::from(ss)) * 1_000 + i64::from(ms))
}

fn apply_datetime_modifier_ms(ms: i64, s: &str) -> Option<i64> {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "start of day" => {
            let (y, mo, d, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            return Some(days_from_civil(i64::from(y), mo, d) * 86_400_000);
        }
        "start of month" => {
            let (y, mo, _, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            return Some(days_from_civil(i64::from(y), mo, 1) * 86_400_000);
        }
        "start of year" => {
            let (y, _, _, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            return Some(days_from_civil(i64::from(y), 1, 1) * 86_400_000);
        }
        "subsec" | "subsecond" => return Some(ms),
        _ => {}
    }
    if let Some(rest) = lower.strip_prefix("weekday") {
        if rest.is_empty() || !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let wf: f64 = rest.trim().parse().ok()?;
        if !wf.is_finite() || wf.fract() != 0.0 || !(0.0..7.0).contains(&wf) {
            return None;
        }
        let target_dow = wf as i64;
        let days = ms.div_euclid(86_400_000);
        let dow = (days + 4).rem_euclid(7);
        let add_days = (target_dow - dow).rem_euclid(7);
        return Some(ms.saturating_add(add_days * 86_400_000));
    }

    let mut parts = s.split_whitespace();
    let num: f64 = parts.next()?.parse().ok()?;
    if !num.is_finite() {
        return None;
    }
    let unit = parts.next()?.to_ascii_lowercase();
    if parts.next().is_some() {
        return None;
    }
    match unit.trim_end_matches('s') {
        "second" => Some(ms.saturating_add((num * 1_000.0).round() as i64)),
        "minute" => Some(ms.saturating_add((num * 60_000.0).round() as i64)),
        "hour" => Some(ms.saturating_add((num * 3_600_000.0).round() as i64)),
        "day" => Some(ms.saturating_add((num * 86_400_000.0).round() as i64)),
        "month" => {
            let whole_months = num.trunc() as i64;
            let frac_months = num - (whole_months as f64);
            let (y, mo, d, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            let tod_ms = ms.rem_euclid(86_400_000);
            let total_m0 = i64::from(y) * 12 + (i64::from(mo) - 1) + whole_months;
            let new_y = total_m0.div_euclid(12);
            let new_m = (total_m0.rem_euclid(12) + 1) as u32;
            let base_days = days_from_civil(new_y, new_m, 1) + i64::from(d) - 1;
            let frac_ms = (frac_months * 30.0 * 86_400_000.0).round() as i64;
            Some(
                base_days
                    .saturating_mul(86_400_000)
                    .saturating_add(tod_ms)
                    .saturating_add(frac_ms),
            )
        }
        "year" => {
            let whole_years = num.trunc() as i64;
            let frac_years = num - (whole_years as f64);
            let (y, mo, d, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            let tod_ms = ms.rem_euclid(86_400_000);
            let new_y = i64::from(y) + whole_years;
            let base_days = days_from_civil(new_y, mo, 1) + i64::from(d) - 1;
            let frac_ms = (frac_years * 365.0 * 86_400_000.0).round() as i64;
            Some(
                base_days
                    .saturating_mul(86_400_000)
                    .saturating_add(tod_ms)
                    .saturating_add(frac_ms),
            )
        }
        _ => None,
    }
}

fn eval_datetime_function(
    upper_name: &str,
    args: &[Value],
    ctx: &VmRuntimeContext,
) -> Result<Value> {
    let unix_ms = if args.is_empty() {
        ctx.effective_timestamp_ms()
    } else {
        let Some(ms) = parse_time_value_to_unix_ms(&args[0], &args[1..], ctx) else {
            return Ok(Value::Null);
        };
        ms
    };

    match upper_name {
        "DATE" => Ok(Value::Text(format_utc_date(unix_ms))),
        "TIME" => Ok(Value::Text(format_utc_time(unix_ms))),
        "DATETIME" => Ok(Value::Text(format_utc_timestamp(unix_ms))),
        "UNIXEPOCH" => Ok(Value::Integer(unix_ms.div_euclid(1_000))),
        "JULIANDAY" => Ok(Value::real((unix_ms as f64) / 86_400_000.0 + 2_440_587.5)),
        _ => unreachable!(),
    }
}

fn eval_strftime_function(args: &[Value], ctx: &VmRuntimeContext) -> Result<Value> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let fmt = args[0].to_text_lossy();
    let unix_ms = if args.len() == 1 {
        ctx.effective_timestamp_ms()
    } else {
        let Some(ms) = parse_time_value_to_unix_ms(&args[1], &args[2..], ctx) else {
            return Ok(Value::Null);
        };
        ms
    };
    let (y, m, d, hh, mm, ss) = utc_ymdhms_from_unix_ms(unix_ms);
    let ms_rem = unix_ms.rem_euclid(1_000) as u32;
    let total_secs = unix_ms.div_euclid(1_000);
    let days = total_secs.div_euclid(86_400);
    let dow = (days + 4).rem_euclid(7); // 1970-01-01 was Thursday (4); Sunday = 0
    let jan1_days = days_from_civil(i64::from(y), 1, 1);
    let doy_1_based = (days - jan1_days) + 1;
    let jd = (unix_ms as f64) / 86_400_000.0 + 2_440_587.5;

    let mut out = String::with_capacity(fmt.len() + 16);
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(spec) = chars.next() else {
            break;
        };
        match spec {
            '%' => out.push('%'),
            'Y' => out.push_str(&format!("{y:04}")),
            'm' => out.push_str(&format!("{m:02}")),
            'd' => out.push_str(&format!("{d:02}")),
            'e' => out.push_str(&format!("{d:2}")),
            'H' => out.push_str(&format!("{hh:02}")),
            'I' => {
                let h12 = match hh % 12 {
                    0 => 12,
                    v => v,
                };
                out.push_str(&format!("{h12:02}"));
            }
            'M' => out.push_str(&format!("{mm:02}")),
            'S' => out.push_str(&format!("{ss:02}")),
            'f' => out.push_str(&format!("{ss:02}.{ms_rem:03}")),
            'F' => out.push_str(&format!("{y:04}-{m:02}-{d:02}")),
            'T' => out.push_str(&format!("{hh:02}:{mm:02}:{ss:02}")),
            'R' => out.push_str(&format!("{hh:02}:{mm:02}")),
            'j' => out.push_str(&format!("{doy_1_based:03}")),
            'J' => out.push_str(&format!("{jd:.9}")),
            's' => out.push_str(&format!("{total_secs}")),
            'w' => out.push_str(&format!("{dow}")),
            'u' => out.push_str(&format!("{}", if dow == 0 { 7 } else { dow })),
            'P' => out.push_str(if hh < 12 { "am" } else { "pm" }),
            'p' => out.push_str(if hh < 12 { "AM" } else { "PM" }),
            _ => {
                // Unknown specifier evaluates to NULL in SQLite `strftime`
                return Ok(Value::Null);
            }
        }
    }
    Ok(Value::Text(out))
}

pub fn parse_escape_char(s: &str) -> Result<char> {
    let mut chars = s.chars();
    let Some(c) = chars.next() else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "ESCAPE expression must be a single character",
        ));
    };
    if chars.next().is_some() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "ESCAPE expression must be a single character",
        ));
    }
    Ok(c)
}

#[inline(always)]
const fn swar_haszero_u64(x: u64) -> u64 {
    x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080
}

#[inline]
fn swar_find_ascii_byte(hay: &[u8], target_lower_or_exact: u8, fold_alpha: bool) -> Option<usize> {
    let bcast = 0x0101_0101_0101_0101u64.wrapping_mul(u64::from(target_lower_or_exact));
    let fold_mask = if fold_alpha {
        0x2020_2020_2020_2020u64
    } else {
        0
    };
    let mut pos = 0usize;
    while pos + 8 <= hay.len() {
        let s = &hay[pos..pos + 8];
        let w = u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
        let x = (w | fold_mask) ^ bcast;
        let hit = swar_haszero_u64(x);
        if hit != 0 {
            return Some(pos + (hit.trailing_zeros() >> 3) as usize);
        }
        pos += 8;
    }
    let fm8 = fold_mask as u8;
    while pos < hay.len() {
        if (hay[pos] | fm8) == target_lower_or_exact {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LikeToken {
    Star,
    AnyOne,
    Literal(char),
}

#[derive(Debug, Clone)]
enum CompiledLikeKind {
    NeverMatch,
    MatchAll,
    ExactAscii(Box<[u8]>),
    PrefixAscii(Box<[u8]>),
    SuffixAscii(Box<[u8]>),
    PrefixSuffixAscii {
        prefix: Box<[u8]>,
        suffix: Box<[u8]>,
    },
    ContainsAsciiOne(u8),
    ContainsAsciiSwar {
        needle: Box<[u8]>,
        first_bcast: u64,
        last_bcast: u64,
        first_fold: u64,
        last_fold: u64,
        packed_needle4_8: u64,
    },
    General(Box<[LikeToken]>),
}

/// Pre-compiled SQLite `LIKE` pattern with zero-allocation byte-slice matching and a 64-bit
/// SWAR first+last byte `haszero` filter + packed `u64` case-folded window matcher for `%<ascii_literal>%`.
#[derive(Debug, Clone)]
pub struct CompiledLikePattern {
    kind: CompiledLikeKind,
    case_sensitive: bool,
}

impl CompiledLikePattern {
    pub fn compile(pattern: &str, escape: Option<char>, case_sensitive: bool) -> Self {
        let mut tokens: Vec<LikeToken> = Vec::with_capacity(pattern.len());
        let mut chars = pattern.chars();
        while let Some(ch) = chars.next() {
            if Some(ch) == escape {
                if let Some(escaped) = chars.next() {
                    tokens.push(LikeToken::Literal(escaped));
                } else {
                    // Trailing dangling escape char in SQLite LIKE (`func.c:patternCompare`) never matches.
                    return Self {
                        kind: CompiledLikeKind::NeverMatch,
                        case_sensitive,
                    };
                }
            } else if ch == '%' {
                if tokens.last() != Some(&LikeToken::Star) {
                    tokens.push(LikeToken::Star);
                }
            } else if ch == '_' {
                tokens.push(LikeToken::AnyOne);
            } else {
                tokens.push(LikeToken::Literal(ch));
            }
        }

        if tokens.as_slice() == [LikeToken::Star] {
            return Self {
                kind: CompiledLikeKind::MatchAll,
                case_sensitive,
            };
        }

        let fold_ascii = |b: u8| -> u8 {
            if case_sensitive {
                b
            } else {
                b.to_ascii_lowercase()
            }
        };

        let extract_ascii_lit = |slice: &[LikeToken]| -> Option<Box<[u8]>> {
            let mut out = Vec::with_capacity(slice.len());
            for tok in slice {
                match tok {
                    LikeToken::Literal(c) if c.is_ascii() => out.push(fold_ascii(*c as u8)),
                    _ => return None,
                }
            }
            Some(out.into_boxed_slice())
        };

        // `ExactAscii`: `lit` (including empty pattern `""`)
        if let Some(exact) = extract_ascii_lit(&tokens) {
            return Self {
                kind: CompiledLikeKind::ExactAscii(exact),
                case_sensitive,
            };
        }

        // `ContainsAscii`: `%lit%`
        if tokens.len() >= 3
            && tokens.first() == Some(&LikeToken::Star)
            && tokens.last() == Some(&LikeToken::Star)
            && let Some(needle) = extract_ascii_lit(&tokens[1..tokens.len() - 1])
        {
            if needle.len() == 1 {
                return Self {
                    kind: CompiledLikeKind::ContainsAsciiOne(needle[0]),
                    case_sensitive,
                };
            }
            let m = needle.len();
            let first = needle[0];
            let last = needle[m - 1];
            let first_bcast = 0x0101_0101_0101_0101u64.wrapping_mul(u64::from(first));
            let last_bcast = 0x0101_0101_0101_0101u64.wrapping_mul(u64::from(last));
            let first_fold = if !case_sensitive && first.is_ascii_lowercase() {
                0x2020_2020_2020_2020u64
            } else {
                0
            };
            let last_fold = if !case_sensitive && last.is_ascii_lowercase() {
                0x2020_2020_2020_2020u64
            } else {
                0
            };
            let packed_needle4_8 = if (4..=8).contains(&m) {
                let lo = u32::from_le_bytes([needle[0], needle[1], needle[2], needle[3]]);
                let hi = u32::from_le_bytes([
                    needle[m - 4],
                    needle[m - 3],
                    needle[m - 2],
                    needle[m - 1],
                ]);
                u64::from(lo) | (u64::from(hi) << 32)
            } else {
                0
            };
            return Self {
                kind: CompiledLikeKind::ContainsAsciiSwar {
                    needle,
                    first_bcast,
                    last_bcast,
                    first_fold,
                    last_fold,
                    packed_needle4_8,
                },
                case_sensitive,
            };
        }

        // `PrefixAscii`: `lit%`
        if tokens.len() >= 2
            && tokens.last() == Some(&LikeToken::Star)
            && let Some(prefix) = extract_ascii_lit(&tokens[..tokens.len() - 1])
        {
            return Self {
                kind: CompiledLikeKind::PrefixAscii(prefix),
                case_sensitive,
            };
        }

        // `SuffixAscii`: `%lit`
        if tokens.len() >= 2
            && tokens.first() == Some(&LikeToken::Star)
            && let Some(suffix) = extract_ascii_lit(&tokens[1..])
        {
            return Self {
                kind: CompiledLikeKind::SuffixAscii(suffix),
                case_sensitive,
            };
        }

        // `PrefixSuffixAscii`: `pre%suf`
        if let Some(star_pos) = tokens.iter().position(|t| *t == LikeToken::Star)
            && star_pos > 0
            && star_pos + 1 < tokens.len()
            && let (Some(prefix), Some(suffix)) = (
                extract_ascii_lit(&tokens[..star_pos]),
                extract_ascii_lit(&tokens[star_pos + 1..]),
            )
        {
            return Self {
                kind: CompiledLikeKind::PrefixSuffixAscii { prefix, suffix },
                case_sensitive,
            };
        }

        Self {
            kind: CompiledLikeKind::General(tokens.into_boxed_slice()),
            case_sensitive,
        }
    }

    #[inline]
    fn eq_slice_folded(hay: &[u8], lower_needle: &[u8], case_sensitive: bool) -> bool {
        let len = lower_needle.len();
        if hay.len() != len {
            return false;
        }
        if case_sensitive {
            return hay == lower_needle;
        }
        if len >= 8 {
            let mut pos = 0usize;
            while pos + 8 <= len {
                let sh = &hay[pos..pos + 8];
                let sn = &lower_needle[pos..pos + 8];
                let wh =
                    u64::from_le_bytes([sh[0], sh[1], sh[2], sh[3], sh[4], sh[5], sh[6], sh[7]]);
                let wn =
                    u64::from_le_bytes([sn[0], sn[1], sn[2], sn[3], sn[4], sn[5], sn[6], sn[7]]);
                if sqliters_types::swar_fold_ascii_lower_u64(wh) != wn {
                    return false;
                }
                pos += 8;
            }
            if pos < len {
                let t = len - 8;
                let sh = &hay[t..len];
                let sn = &lower_needle[t..len];
                let wh =
                    u64::from_le_bytes([sh[0], sh[1], sh[2], sh[3], sh[4], sh[5], sh[6], sh[7]]);
                let wn =
                    u64::from_le_bytes([sn[0], sn[1], sn[2], sn[3], sn[4], sn[5], sn[6], sn[7]]);
                if sqliters_types::swar_fold_ascii_lower_u64(wh) != wn {
                    return false;
                }
            }
            return true;
        }
        if len >= 4 {
            let t = len - 4;
            let h0 = u32::from_le_bytes([hay[0], hay[1], hay[2], hay[3]]);
            let h1 = u32::from_le_bytes([hay[t], hay[t + 1], hay[t + 2], hay[t + 3]]);
            let n0 = u32::from_le_bytes([
                lower_needle[0],
                lower_needle[1],
                lower_needle[2],
                lower_needle[3],
            ]);
            let n1 = u32::from_le_bytes([
                lower_needle[t],
                lower_needle[t + 1],
                lower_needle[t + 2],
                lower_needle[t + 3],
            ]);
            let wh = u64::from(h0) | (u64::from(h1) << 32);
            let wn = u64::from(n0) | (u64::from(n1) << 32);
            return sqliters_types::swar_fold_ascii_lower_u64(wh) == wn;
        }
        for i in 0..len {
            if hay[i].to_ascii_lowercase() != lower_needle[i] {
                return false;
            }
        }
        true
    }

    /// Evaluates this pre-compiled `LIKE` pattern directly on UTF-8 bytes with zero heap allocation
    /// for all ASCII fast-path shapes and valid UTF-8 general patterns.
    #[inline]
    pub fn matches_utf8_bytes(&self, haystack: &[u8]) -> bool {
        let cs = self.case_sensitive;
        match &self.kind {
            CompiledLikeKind::NeverMatch => false,
            CompiledLikeKind::MatchAll => true,
            CompiledLikeKind::ExactAscii(lit) => Self::eq_slice_folded(haystack, lit, cs),
            CompiledLikeKind::PrefixAscii(pre) => {
                haystack.len() >= pre.len()
                    && Self::eq_slice_folded(&haystack[..pre.len()], pre, cs)
            }
            CompiledLikeKind::SuffixAscii(suf) => {
                haystack.len() >= suf.len()
                    && Self::eq_slice_folded(&haystack[haystack.len() - suf.len()..], suf, cs)
            }
            CompiledLikeKind::PrefixSuffixAscii { prefix, suffix } => {
                haystack.len() >= prefix.len() + suffix.len()
                    && Self::eq_slice_folded(&haystack[..prefix.len()], prefix, cs)
                    && Self::eq_slice_folded(&haystack[haystack.len() - suffix.len()..], suffix, cs)
            }
            CompiledLikeKind::ContainsAsciiOne(target) => {
                let t = *target;
                swar_find_ascii_byte(haystack, t, !cs && t.is_ascii_lowercase()).is_some()
            }
            CompiledLikeKind::ContainsAsciiSwar {
                needle,
                first_bcast,
                last_bcast,
                first_fold,
                last_fold,
                packed_needle4_8,
            } => {
                let m = needle.len();
                let n = haystack.len();
                if n < m {
                    return false;
                }
                let span = n - m + 1;
                let check_at = |idx: usize| -> bool {
                    if (4..=8).contains(&m) {
                        let win = &haystack[idx..idx + m];
                        let lo = u32::from_le_bytes([win[0], win[1], win[2], win[3]]);
                        let hi =
                            u32::from_le_bytes([win[m - 4], win[m - 3], win[m - 2], win[m - 1]]);
                        let w = u64::from(lo) | (u64::from(hi) << 32);
                        let fw = if cs {
                            w
                        } else {
                            sqliters_types::swar_fold_ascii_lower_u64(w)
                        };
                        fw == *packed_needle4_8
                    } else {
                        Self::eq_slice_folded(&haystack[idx..idx + m], needle, cs)
                    }
                };
                if span >= 8 {
                    let fb = *first_bcast;
                    let lb = *last_bcast;
                    let ff = *first_fold;
                    let lf = *last_fold;
                    let mut pos = 0usize;
                    while pos + 8 <= span {
                        let sf = &haystack[pos..pos + 8];
                        let sl = &haystack[pos + m - 1..pos + m - 1 + 8];
                        let wf = u64::from_le_bytes([
                            sf[0], sf[1], sf[2], sf[3], sf[4], sf[5], sf[6], sf[7],
                        ]);
                        let wl = u64::from_le_bytes([
                            sl[0], sl[1], sl[2], sl[3], sl[4], sl[5], sl[6], sl[7],
                        ]);
                        let mut hit =
                            swar_haszero_u64((wf | ff) ^ fb) & swar_haszero_u64((wl | lf) ^ lb);
                        while hit != 0 {
                            let j = (hit.trailing_zeros() >> 3) as usize;
                            if check_at(pos + j) {
                                return true;
                            }
                            hit &= hit - 1;
                        }
                        pos += 8;
                    }
                    if pos < span {
                        let t = span - 8;
                        let sf = &haystack[t..t + 8];
                        let sl = &haystack[t + m - 1..t + m - 1 + 8];
                        let wf = u64::from_le_bytes([
                            sf[0], sf[1], sf[2], sf[3], sf[4], sf[5], sf[6], sf[7],
                        ]);
                        let wl = u64::from_le_bytes([
                            sl[0], sl[1], sl[2], sl[3], sl[4], sl[5], sl[6], sl[7],
                        ]);
                        let mut hit =
                            swar_haszero_u64((wf | ff) ^ fb) & swar_haszero_u64((wl | lf) ^ lb);
                        while hit != 0 {
                            let j = (hit.trailing_zeros() >> 3) as usize;
                            if check_at(t + j) {
                                return true;
                            }
                            hit &= hit - 1;
                        }
                    }
                    false
                } else {
                    for idx in 0..span {
                        if check_at(idx) {
                            return true;
                        }
                    }
                    false
                }
            }
            CompiledLikeKind::General(tokens) => {
                if let Ok(s) = std::str::from_utf8(haystack) {
                    like_tokens_str(tokens, s, cs)
                } else {
                    let lossy = String::from_utf8_lossy(haystack);
                    like_tokens_str(tokens, &lossy, cs)
                }
            }
        }
    }

    #[inline]
    pub fn matches_str(&self, text: &str) -> bool {
        match &self.kind {
            CompiledLikeKind::General(tokens) => like_tokens_str(tokens, text, self.case_sensitive),
            _ => self.matches_utf8_bytes(text.as_bytes()),
        }
    }

    /// Evaluates this pre-compiled `LIKE` pattern directly on a borrowed [`sqliters_types::RawField`]
    /// inside a 4 KiB B-tree page buffer with zero heap allocation on UTF-8 text, blobs, and integers.
    #[inline]
    pub fn matches_raw_field(
        &self,
        field: sqliters_types::RawField<'_>,
        enc: TextEncoding,
    ) -> Option<bool> {
        match field {
            sqliters_types::RawField::Null => None,
            sqliters_types::RawField::Text(bytes) => {
                if enc == TextEncoding::Utf8 {
                    Some(self.matches_utf8_bytes(bytes))
                } else {
                    let decoded = enc.decode_bytes(bytes);
                    Some(self.matches_str(&decoded))
                }
            }
            sqliters_types::RawField::Blob(bytes) => Some(self.matches_utf8_bytes(bytes)),
            sqliters_types::RawField::Int(i) => {
                let mut buf = [0u8; 24];
                let slice = format_i64_into_stack(i, &mut buf);
                Some(self.matches_utf8_bytes(slice))
            }
            sqliters_types::RawField::Float(f) => {
                let s = sqliters_types::format_sqlite_real(f);
                Some(self.matches_str(&s))
            }
        }
    }
}

#[inline]
pub fn format_i64_into_stack(val: i64, buf: &mut [u8; 24]) -> &[u8] {
    if val == 0 {
        buf[23] = b'0';
        return &buf[23..24];
    }
    let neg = val < 0;
    let mut mag = val.unsigned_abs();
    let mut pos = 24usize;
    while mag > 0 {
        pos -= 1;
        buf[pos] = b'0' + (mag % 10) as u8;
        mag /= 10;
    }
    if neg {
        pos -= 1;
        buf[pos] = b'-';
    }
    &buf[pos..24]
}

fn like_tokens_str(pat: &[LikeToken], mut txt: &str, case_sensitive: bool) -> bool {
    let mut pi = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_txt: &str = "";

    loop {
        if pi < pat.len() {
            match pat[pi] {
                LikeToken::Star => {
                    star_pi = Some(pi + 1);
                    star_txt = txt;
                    pi += 1;
                    continue;
                }
                LikeToken::AnyOne => {
                    let mut chars = txt.chars();
                    if chars.next().is_some() {
                        txt = chars.as_str();
                        pi += 1;
                        continue;
                    }
                }
                LikeToken::Literal(pc) => {
                    let mut chars = txt.chars();
                    if let Some(tc) = chars.next() {
                        let eq = if case_sensitive {
                            pc == tc
                        } else {
                            ascii_eq_nocase(pc, tc)
                        };
                        if eq {
                            txt = chars.as_str();
                            pi += 1;
                            continue;
                        }
                    }
                }
            }
        } else if txt.is_empty() {
            return true;
        }

        if let Some(spi) = star_pi {
            let mut chars = star_txt.chars();
            if chars.next().is_some() {
                star_txt = chars.as_str();
                txt = star_txt;
                pi = spi;
                continue;
            }
        }
        return false;
    }
}

/// SQLite `LIKE` matcher (`%` = 0+ chars, `_` = 1 char, ASCII case-insensitive).
pub fn sqlite_like_match(pattern: &str, text: &str, escape: Option<char>) -> bool {
    if escape.is_none() && pattern.is_ascii() && text.is_ascii() {
        return like_ascii_bytes_nocase(pattern.as_bytes(), text.as_bytes());
    }
    like_str_iter(pattern, text, escape)
}

#[inline]
fn like_ascii_bytes_nocase(pat: &[u8], txt: &[u8]) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star_pi = usize::MAX;
    let mut star_ti = 0usize;

    while ti < txt.len() {
        if pi < pat.len() {
            let pb = pat[pi];
            if pb == b'%' {
                while pi < pat.len() && pat[pi] == b'%' {
                    pi += 1;
                }
                if pi == pat.len() {
                    return true;
                }
                if pat[pi] != b'_' {
                    let next_lo = pat[pi].to_ascii_lowercase();
                    let Some(off) =
                        swar_find_ascii_byte(&txt[ti..], next_lo, next_lo.is_ascii_lowercase())
                    else {
                        return false;
                    };
                    ti += off;
                }
                star_pi = pi;
                star_ti = ti;
                continue;
            }
            if pb == b'_' || pb.eq_ignore_ascii_case(&txt[ti]) {
                pi += 1;
                ti += 1;
                continue;
            }
        }
        if star_pi != usize::MAX {
            star_ti += 1;
            if pat[star_pi] != b'_' {
                if star_ti >= txt.len() {
                    return false;
                }
                let next_lo = pat[star_pi].to_ascii_lowercase();
                let Some(off) =
                    swar_find_ascii_byte(&txt[star_ti..], next_lo, next_lo.is_ascii_lowercase())
                else {
                    return false;
                };
                star_ti += off;
            }
            ti = star_ti;
            pi = star_pi;
            continue;
        }
        return false;
    }
    while pi < pat.len() && pat[pi] == b'%' {
        pi += 1;
    }
    pi == pat.len()
}

fn like_str_iter(mut pat: &str, mut txt: &str, escape: Option<char>) -> bool {
    let mut star_pat: Option<&str> = None;
    let mut star_txt: &str = "";

    loop {
        let mut pchars = pat.chars();
        if let Some(pc) = pchars.next() {
            if Some(pc) == escape {
                let Some(esc_lit) = pchars.next() else {
                    return false;
                };
                let mut tchars = txt.chars();
                if let Some(tc) = tchars.next()
                    && ascii_eq_nocase(esc_lit, tc)
                {
                    pat = pchars.as_str();
                    txt = tchars.as_str();
                    continue;
                }
            } else if pc == '%' {
                let mut rest = pchars.as_str();
                while let Some(after) = rest.strip_prefix('%') {
                    rest = after;
                }
                if rest.is_empty() {
                    return true;
                }
                star_pat = Some(rest);
                star_txt = txt;
                pat = rest;
                continue;
            } else if pc == '_' {
                let mut tchars = txt.chars();
                if tchars.next().is_some() {
                    pat = pchars.as_str();
                    txt = tchars.as_str();
                    continue;
                }
            } else {
                let mut tchars = txt.chars();
                if let Some(tc) = tchars.next()
                    && ascii_eq_nocase(pc, tc)
                {
                    pat = pchars.as_str();
                    txt = tchars.as_str();
                    continue;
                }
            }
        } else if txt.is_empty() {
            return true;
        }

        if let Some(sp) = star_pat {
            let mut tchars = star_txt.chars();
            if tchars.next().is_some() {
                star_txt = tchars.as_str();
                txt = star_txt;
                pat = sp;
                continue;
            }
        }
        return false;
    }
}

#[inline]
fn ascii_eq_nocase(a: char, b: char) -> bool {
    if a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

/// SQLite `GLOB` matcher (`*` = 0+ chars, `?` = 1 char, `[abc]`/`[a-z]`/`[^a]` character classes, case-sensitive).
pub fn sqlite_glob_match(pattern: &str, text: &str) -> bool {
    let pchars: Vec<char> = pattern.chars().collect();
    let tchars: Vec<char> = text.chars().collect();
    glob_rec(&pchars, &tchars)
}

fn glob_rec(pat: &[char], txt: &[char]) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    while pi < pat.len() {
        match pat[pi] {
            '*' => {
                while pi < pat.len() && pat[pi] == '*' {
                    pi += 1;
                }
                if pi == pat.len() {
                    return true;
                }
                for k in ti..=txt.len() {
                    if glob_rec(&pat[pi..], &txt[k..]) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if ti >= txt.len() {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            '[' => {
                if ti >= txt.len() {
                    return false;
                }
                let ch = txt[ti];
                pi += 1;
                let invert = pi < pat.len() && pat[pi] == '^';
                if invert {
                    pi += 1;
                }
                let mut matched = false;
                let mut first = true;
                while pi < pat.len() && (first || pat[pi] != ']') {
                    first = false;
                    let c1 = pat[pi];
                    if pi + 2 < pat.len() && pat[pi + 1] == '-' && pat[pi + 2] != ']' {
                        let c2 = pat[pi + 2];
                        if ch >= c1 && ch <= c2 {
                            matched = true;
                        }
                        pi += 3;
                    } else {
                        if ch == c1 {
                            matched = true;
                        }
                        pi += 1;
                    }
                }
                if pi >= pat.len() || pat[pi] != ']' {
                    return false;
                }
                if invert {
                    matched = !matched;
                }
                if !matched {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            c => {
                if ti >= txt.len() || txt[ti] != c {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == txt.len()
}

fn math_numeric_value(v: &Value) -> Option<Value> {
    match v {
        Value::Integer(i) => Some(Value::Integer(*i)),
        Value::Real(r) => Some(Value::Real(*r)),
        Value::Text(s) => sqliters_types::parse_numeric_text(s),
        Value::Null | Value::Blob(_) => None,
    }
}

fn math_real_arg(v: &Value) -> Option<f64> {
    match math_numeric_value(v)? {
        Value::Integer(i) => Some(i as f64),
        Value::Real(r) => Some(r),
        _ => None,
    }
}

fn format_sqlite_float_sci(abs: f64, prec: usize, upper: bool, force_dot: bool) -> String {
    let raw = format!("{abs:.prec$e}");
    let Some((mant, exp_str)) = raw.split_once('e') else {
        return raw;
    };
    let exp = exp_str.parse::<i32>().unwrap_or(0);
    let mut m = mant.to_owned();
    if prec == 0 && force_dot && !m.contains('.') {
        m.push('.');
    }
    let e_ch = if upper { 'E' } else { 'e' };
    let e_sign = if exp < 0 { '-' } else { '+' };
    let abs_exp = exp.unsigned_abs();
    format!("{m}{e_ch}{e_sign}{abs_exp:02}")
}

fn format_sqlite_float_g(
    abs: f64,
    prec: Option<usize>,
    upper: bool,
    alt_hash: bool,
    alt_bang: bool,
) -> String {
    let p = prec.unwrap_or(6).max(1);
    if abs == 0.0 {
        if alt_hash {
            if p > 1 {
                return format!("0.{}", "0".repeat(p - 1));
            }
            return "0".to_owned();
        }
        if alt_bang {
            return "0.0".to_owned();
        }
        return "0".to_owned();
    }

    let sig = if alt_bang { p.min(26) } else { p.min(15) };
    let sci_prec = sig.saturating_sub(1);
    let raw = format!("{abs:.sci_prec$e}");
    let Some((mant, exp_str)) = raw.split_once('e') else {
        return raw;
    };
    let exp = exp_str.parse::<i32>().unwrap_or(0);
    let digits: String = mant.chars().filter(|&c| c != '.').collect();

    if exp >= -4 && exp < (p as i32) {
        if exp < 0 {
            let leading_zeros = (-1 - exp) as usize;
            let mut s = format!("0.{}{digits}", "0".repeat(leading_zeros));
            if alt_hash {
                if p > digits.len() {
                    s.push_str(&"0".repeat(p - digits.len()));
                }
            } else {
                while s.ends_with('0') {
                    s.pop();
                }
                if s.ends_with('.') {
                    if alt_bang {
                        s.push('0');
                    } else {
                        s.pop();
                    }
                }
            }
            s
        } else {
            let int_digits = (exp + 1) as usize;
            if int_digits < digits.len() {
                let mut s = format!("{}.{}", &digits[..int_digits], &digits[int_digits..]);
                if alt_hash {
                    if p > digits.len() {
                        s.push_str(&"0".repeat(p - digits.len()));
                    }
                } else {
                    while s.ends_with('0') {
                        s.pop();
                    }
                    if s.ends_with('.') {
                        if alt_bang {
                            s.push('0');
                        } else {
                            s.pop();
                        }
                    }
                }
                s
            } else {
                let mut s = format!("{digits}{}", "0".repeat(int_digits - digits.len()));
                if alt_hash {
                    if p > int_digits {
                        s.push('.');
                        s.push_str(&"0".repeat(p - int_digits));
                    }
                } else if alt_bang {
                    s.push_str(".0");
                }
                s
            }
        }
    } else {
        let mut m = if digits.len() <= 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        if alt_hash {
            if digits.len() <= 1 && p > 1 {
                m.push('.');
                m.push_str(&"0".repeat(p - 1));
            } else if p > digits.len() {
                if !m.contains('.') {
                    m.push('.');
                }
                m.push_str(&"0".repeat(p - digits.len()));
            }
        } else if m.contains('.') {
            while m.ends_with('0') {
                m.pop();
            }
            if m.ends_with('.') {
                if alt_bang {
                    m.push('0');
                } else {
                    m.pop();
                }
            }
        } else if alt_bang {
            m.push_str(".0");
        }
        let e_ch = if upper { 'E' } else { 'e' };
        let e_sign = if exp < 0 { '-' } else { '+' };
        let abs_exp = exp.unsigned_abs();
        format!("{m}{e_ch}{e_sign}{abs_exp:02}")
    }
}

fn insert_thousands_commas(digits: &str) -> String {
    let len = digits.len();
    if len <= 3 {
        return digits.to_owned();
    }
    let mut out = String::with_capacity(len + (len - 1) / 3);
    let first_group = len % 3;
    let mut idx = 0;
    if first_group > 0 {
        out.push_str(&digits[..first_group]);
        idx = first_group;
    }
    while idx < len {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(&digits[idx..idx + 3]);
        idx += 3;
    }
    out
}

fn simple_sqlite_printf(fmt: &str, args: &[Value]) -> String {
    let mut out = String::with_capacity(fmt.len() + 16);
    let bytes = fmt.as_bytes();
    let mut i = 0usize;
    let mut arg_idx = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'%' {
            let ch = fmt[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        i += 1;
        if i < bytes.len() && bytes[i] == b'%' {
            out.push('%');
            i += 1;
            continue;
        }
        let mut zero_pad = false;
        let mut left_align = false;
        let mut plus_sign = false;
        let mut space_sign = false;
        let mut alt_hash = false;
        let mut alt_bang = false;
        let mut comma_sep = false;
        while i < bytes.len() {
            match bytes[i] {
                b'0' => {
                    zero_pad = true;
                    i += 1;
                }
                b'-' => {
                    left_align = true;
                    i += 1;
                }
                b'+' => {
                    plus_sign = true;
                    i += 1;
                }
                b' ' => {
                    space_sign = true;
                    i += 1;
                }
                b'#' => {
                    alt_hash = true;
                    i += 1;
                }
                b'!' => {
                    alt_bang = true;
                    i += 1;
                }
                b',' => {
                    comma_sep = true;
                    i += 1;
                }
                _ => break,
            }
        }
        let mut width = 0usize;
        if i < bytes.len() && bytes[i] == b'*' {
            i += 1;
            let w = args.get(arg_idx).map_or(0, Value::to_integer_lossy);
            arg_idx += 1;
            if w < 0 {
                left_align = true;
                width = w.unsigned_abs() as usize;
            } else {
                width = w as usize;
            }
        } else {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                width = width * 10 + (bytes[i] - b'0') as usize;
                i += 1;
            }
        }
        let mut prec: Option<usize> = None;
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            if i < bytes.len() && bytes[i] == b'*' {
                i += 1;
                let p = args.get(arg_idx).map_or(0, Value::to_integer_lossy);
                arg_idx += 1;
                if p >= 0 {
                    prec = Some(p as usize);
                }
            } else {
                let mut p = 0usize;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    p = p * 10 + (bytes[i] - b'0') as usize;
                    i += 1;
                }
                prec = Some(p);
            }
        }
        // Optional length modifiers (`l`, `ll`, `h`, `z`)
        while i < bytes.len() && matches!(bytes[i], b'l' | b'h') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let spec = bytes[i] as char;
        i += 1;
        let val = args.get(arg_idx).cloned().unwrap_or(Value::Null);
        arg_idx += 1;

        let (prefix, body) = match spec {
            'd' | 'i' => {
                let n = val.to_integer_lossy();
                let sign = if n < 0 {
                    "-"
                } else if plus_sign {
                    "+"
                } else if space_sign {
                    " "
                } else {
                    ""
                };
                let mut digits = format!("{}", n.unsigned_abs());
                if let Some(p) = prec {
                    if p == 0 && digits == "0" {
                        digits.clear();
                    } else if digits.len() < p {
                        digits = format!("{}{digits}", "0".repeat(p - digits.len()));
                    }
                }
                if comma_sep && !digits.is_empty() {
                    digits = insert_thousands_commas(&digits);
                }
                (sign.to_owned(), digits)
            }
            'u' => {
                let mut digits = format!("{}", val.to_integer_lossy() as u64);
                if let Some(p) = prec {
                    if p == 0 && digits == "0" {
                        digits.clear();
                    } else if digits.len() < p {
                        digits = format!("{}{digits}", "0".repeat(p - digits.len()));
                    }
                }
                if comma_sep && !digits.is_empty() {
                    digits = insert_thousands_commas(&digits);
                }
                (String::new(), digits)
            }
            'o' => {
                let v = val.to_integer_lossy() as u64;
                let mut digits = format!("{v:o}");
                if let Some(p) = prec {
                    if p == 0 && v == 0 {
                        digits.clear();
                    } else if digits.len() < p {
                        digits = format!("{}{digits}", "0".repeat(p - digits.len()));
                    }
                }
                let pfx = if alt_hash && v != 0 && !digits.starts_with('0') {
                    "0"
                } else {
                    ""
                };
                (pfx.to_owned(), digits)
            }
            'x' | 'X' => {
                let v = val.to_integer_lossy() as u64;
                let mut digits = if spec == 'X' {
                    format!("{v:X}")
                } else {
                    format!("{v:x}")
                };
                if let Some(p) = prec {
                    if p == 0 && v == 0 {
                        digits.clear();
                    } else if digits.len() < p {
                        digits = format!("{}{digits}", "0".repeat(p - digits.len()));
                    }
                }
                let pfx = if alt_hash && v != 0 {
                    if spec == 'X' { "0X" } else { "0x" }
                } else {
                    ""
                };
                (pfx.to_owned(), digits)
            }
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                let r = val.to_real_lossy();
                if r.is_nan() {
                    (String::new(), "NaN".to_owned())
                } else {
                    let is_neg = r < 0.0;
                    let sign = if is_neg {
                        "-"
                    } else if plus_sign {
                        "+"
                    } else if space_sign {
                        " "
                    } else {
                        ""
                    };
                    if r.is_infinite() {
                        (sign.to_owned(), "Inf".to_owned())
                    } else {
                        let abs = if r == 0.0 { 0.0 } else { r.abs() };
                        let rendered = match spec {
                            'f' | 'F' => {
                                let p = prec.unwrap_or(6);
                                let mut s = format!("{abs:.p$}");
                                if p == 0 && alt_hash && !s.contains('.') {
                                    s.push('.');
                                }
                                s
                            }
                            'e' | 'E' => {
                                let p = prec.unwrap_or(6);
                                format_sqlite_float_sci(abs, p, spec == 'E', alt_hash)
                            }
                            _ => format_sqlite_float_g(abs, prec, spec == 'G', alt_hash, alt_bang),
                        };
                        (sign.to_owned(), rendered)
                    }
                }
            }
            'c' => {
                let repeat = prec.unwrap_or(1).max(1);
                let s = val.to_text_lossy();
                let rendered = if let Some(ch) = s.chars().next() {
                    let mut buf = String::with_capacity(ch.len_utf8() * repeat);
                    for _ in 0..repeat {
                        buf.push(ch);
                    }
                    buf
                } else {
                    String::new()
                };
                (String::new(), rendered)
            }
            'q' => {
                let rendered = if val.is_null() {
                    "(NULL)".to_owned()
                } else {
                    let s = val.to_text_lossy();
                    let before_nul = s.split('\0').next().unwrap_or("");
                    before_nul.replace('\'', "''")
                };
                (String::new(), rendered)
            }
            'Q' => {
                let rendered = if val.is_null() {
                    "NULL".to_owned()
                } else {
                    let s = val.to_text_lossy();
                    let before_nul = s.split('\0').next().unwrap_or("");
                    format!("'{}'", before_nul.replace('\'', "''"))
                };
                (String::new(), rendered)
            }
            's' | 'z' => {
                let s = val.to_text_lossy();
                let before_nul = s.split('\0').next().unwrap_or("");
                let rendered = if let Some(p) = prec {
                    before_nul.chars().take(p).collect()
                } else {
                    before_nul.to_owned()
                };
                (String::new(), rendered)
            }
            _ => (String::new(), val.to_text_lossy()),
        };

        let char_len = prefix.chars().count() + body.chars().count();
        if char_len < width {
            let pad = width - char_len;
            if left_align {
                out.push_str(&prefix);
                out.push_str(&body);
                for _ in 0..pad {
                    out.push(' ');
                }
                continue;
            }
            let use_zero = zero_pad
                && !matches!(spec, 's' | 'z' | 'c' | 'q' | 'Q')
                && body != "Inf"
                && body != "NaN";
            if use_zero {
                out.push_str(&prefix);
                for _ in 0..pad {
                    out.push('0');
                }
                out.push_str(&body);
                continue;
            }
            for _ in 0..pad {
                out.push(' ');
            }
        }
        out.push_str(&prefix);
        out.push_str(&body);
    }
    out
}

// ============================================================================
// Aggregate Functions (`COUNT`, `SUM`, `TOTAL`, `AVG`, `MIN`, `MAX`, `GROUP_CONCAT`)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    CountStar,
    Count,
    Sum,
    Total,
    Avg,
    Min,
    Max,
    GroupConcat,
    JsonGroupArray,
    JsonbGroupArray,
    JsonGroupObject,
    JsonbGroupObject,
}

impl AggregateKind {
    pub fn from_name(name: &str, star_args: bool) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "COUNT" if star_args => Some(Self::CountStar),
            "COUNT" => Some(Self::Count),
            "SUM" => Some(Self::Sum),
            "TOTAL" => Some(Self::Total),
            "AVG" => Some(Self::Avg),
            "MIN" => Some(Self::Min),
            "MAX" => Some(Self::Max),
            "GROUP_CONCAT" | "STRING_AGG" => Some(Self::GroupConcat),
            "JSON_GROUP_ARRAY" => Some(Self::JsonGroupArray),
            "JSONB_GROUP_ARRAY" => Some(Self::JsonbGroupArray),
            "JSON_GROUP_OBJECT" => Some(Self::JsonGroupObject),
            "JSONB_GROUP_OBJECT" => Some(Self::JsonbGroupObject),
            _ => None,
        }
    }

    #[inline]
    pub fn function_flags(self) -> FunctionFlags {
        match self {
            Self::JsonGroupArray | Self::JsonGroupObject => {
                FunctionFlags::DETERMINISTIC
                    | FunctionFlags::INNOCUOUS
                    | FunctionFlags::SUBTYPE
                    | FunctionFlags::RESULT_SUBTYPE
            }
            Self::JsonbGroupArray | Self::JsonbGroupObject => {
                FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS | FunctionFlags::SUBTYPE
            }
            _ => FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SumCtx {
    r_sum: f64,
    r_err: f64,
    i_sum: i64,
    approx: bool,
    ovrfl: bool,
}

#[derive(Debug, Clone, Copy)]
enum SumNumeric {
    Int(i64),
    Float(f64),
}

impl SumCtx {
    #[inline]
    fn kbn_step(&mut self, r: f64) {
        let s = self.r_sum;
        let t = s + r;
        if s.abs() > r.abs() {
            self.r_err += (s - t) + r;
        } else {
            self.r_err += (r - t) + s;
        }
        self.r_sum = t;
    }

    #[inline]
    fn kbn_step_i64(&mut self, i_val: i64) {
        const SPLIT_LIMIT: i64 = 4_503_599_627_370_496; // 2^52
        if i_val <= -SPLIT_LIMIT || i_val >= SPLIT_LIMIT {
            let i_sm = i_val % 16_384;
            let i_big = i_val - i_sm;
            self.kbn_step(i_big as f64);
            self.kbn_step(i_sm as f64);
        } else {
            self.kbn_step(i_val as f64);
        }
    }

    #[inline]
    fn kbn_init(&mut self, i_val: i64) {
        const SPLIT_LIMIT: i64 = 4_503_599_627_370_496; // 2^52
        if i_val <= -SPLIT_LIMIT || i_val >= SPLIT_LIMIT {
            let i_sm = i_val % 16_384;
            self.r_sum = (i_val - i_sm) as f64;
            self.r_err = i_sm as f64;
        } else {
            self.r_sum = i_val as f64;
            self.r_err = 0.0;
        }
    }

    #[inline]
    fn classify_numeric(v: &Value) -> Option<SumNumeric> {
        match v {
            Value::Null => None,
            Value::Integer(i) => Some(SumNumeric::Int(*i)),
            Value::Real(r) => Some(SumNumeric::Float(*r)),
            Value::Text(s) => match sqliters_types::parse_numeric_text(s) {
                Some(Value::Integer(i)) => Some(SumNumeric::Int(i)),
                Some(Value::Real(r)) => Some(SumNumeric::Float(r)),
                _ => Some(SumNumeric::Float(v.to_real_lossy())),
            },
            Value::Blob(_) => Some(SumNumeric::Float(0.0)),
        }
    }

    fn step_val(&mut self, v: &Value) {
        let Some(num) = Self::classify_numeric(v) else {
            return;
        };
        if !self.approx {
            match num {
                SumNumeric::Float(f) => {
                    self.kbn_init(self.i_sum);
                    self.approx = true;
                    self.kbn_step(f);
                }
                SumNumeric::Int(i) => {
                    if let Some(next) = self.i_sum.checked_add(i) {
                        self.i_sum = next;
                    } else {
                        self.ovrfl = true;
                        self.kbn_init(self.i_sum);
                        self.approx = true;
                        self.kbn_step_i64(i);
                    }
                }
            }
        } else {
            match num {
                SumNumeric::Int(i) => self.kbn_step_i64(i),
                SumNumeric::Float(f) => {
                    self.ovrfl = false;
                    self.kbn_step(f);
                }
            }
        }
    }

    fn inverse_val(&mut self, v: &Value) {
        let Some(num) = Self::classify_numeric(v) else {
            return;
        };
        if !self.approx
            && let SumNumeric::Int(i) = num
        {
            if let Some(next) = self.i_sum.checked_sub(i) {
                self.i_sum = next;
                return;
            }
            self.ovrfl = true;
            self.approx = true;
            self.kbn_init(self.i_sum);
        }
        match num {
            SumNumeric::Int(i) => {
                if i != i64::MIN {
                    self.kbn_step_i64(-i);
                } else {
                    self.kbn_step_i64(i64::MAX);
                    self.kbn_step_i64(1);
                }
            }
            SumNumeric::Float(f) => {
                self.kbn_step(-f);
            }
        }
    }

    #[inline]
    fn combined_f64(&self) -> f64 {
        if self.approx {
            if self.r_err.is_finite() {
                self.r_sum + self.r_err
            } else {
                self.r_sum
            }
        } else {
            self.i_sum as f64
        }
    }
}

/// Zero-allocation streaming `SUM` / `TOTAL` / `AVG` accumulator sharing exact SQLite `SumCtx`
/// overflow and Kahan-Babuška-Neumaier floating-point semantics.
#[derive(Debug, Clone, Default)]
pub struct FastSumAccum {
    count: i64,
    ctx: SumCtx,
}

impl FastSumAccum {
    #[inline]
    pub const fn new() -> Self {
        Self {
            count: 0,
            ctx: SumCtx {
                r_sum: 0.0,
                r_err: 0.0,
                i_sum: 0,
                approx: false,
                ovrfl: false,
            },
        }
    }

    #[inline(always)]
    pub const fn count(&self) -> i64 {
        self.count
    }

    #[inline(always)]
    pub fn step_i64(&mut self, i: i64) {
        self.count += 1;
        if !self.ctx.approx {
            if let Some(next) = self.ctx.i_sum.checked_add(i) {
                self.ctx.i_sum = next;
                return;
            }
            self.ctx.ovrfl = true;
            self.ctx.kbn_init(self.ctx.i_sum);
            self.ctx.approx = true;
        }
        self.ctx.kbn_step_i64(i);
    }

    #[inline]
    pub fn step_f64(&mut self, f: f64) {
        if f.is_nan() {
            return;
        }
        self.count += 1;
        if !self.ctx.approx {
            self.ctx.kbn_init(self.ctx.i_sum);
            self.ctx.approx = true;
        } else {
            self.ctx.ovrfl = false;
        }
        self.ctx.kbn_step(f);
    }

    #[inline]
    pub fn step_value(&mut self, v: &Value) {
        if v.is_null() {
            return;
        }
        self.count += 1;
        self.ctx.step_val(v);
    }

    #[inline]
    pub fn inverse_value(&mut self, v: &Value) {
        if v.is_null() {
            return;
        }
        self.count -= 1;
        self.ctx.inverse_val(v);
    }

    #[inline]
    pub fn step_raw_field(&mut self, field: sqliters_types::RawField<'_>, enc: TextEncoding) {
        match field {
            sqliters_types::RawField::Null => {}
            sqliters_types::RawField::Int(i) => self.step_i64(i),
            sqliters_types::RawField::Float(f) => self.step_f64(f),
            sqliters_types::RawField::Blob(_) => self.step_f64(0.0),
            sqliters_types::RawField::Text(b) => {
                if enc == TextEncoding::Utf8
                    && let Ok(s) = std::str::from_utf8(b)
                {
                    match sqliters_types::parse_numeric_text(s) {
                        Some(Value::Integer(i)) => self.step_i64(i),
                        Some(Value::Real(r)) => self.step_f64(r),
                        _ => self.step_f64(sqliters_types::parse_leading_f64(s)),
                    }
                } else {
                    let owned = field.to_owned_value(enc);
                    self.step_value(&owned);
                }
            }
        }
    }

    #[inline]
    pub fn finalize_sum(&self) -> Result<Value> {
        if self.count == 0 {
            return Ok(Value::Null);
        }
        if self.ctx.approx {
            if self.ctx.ovrfl {
                Err(SqliteError::new(ErrorCode::Mismatch, "integer overflow"))
            } else {
                Ok(Value::real(self.ctx.combined_f64()))
            }
        } else {
            Ok(Value::Integer(self.ctx.i_sum))
        }
    }

    #[inline]
    pub fn finalize_total(&self) -> Value {
        if self.count == 0 && !self.ctx.approx {
            Value::Real(0.0)
        } else {
            Value::real(self.ctx.combined_f64())
        }
    }

    #[inline]
    pub fn finalize_avg(&self) -> Value {
        if self.count == 0 {
            Value::Null
        } else {
            Value::real(self.ctx.combined_f64() / (self.count as f64))
        }
    }
}

#[derive(Debug, Clone)]
pub struct AggregateAccumulator {
    kind: AggregateKind,
    distinct: bool,
    collation: Collation,
    encoding: TextEncoding,
    seen: HashSet<Vec<u8>>,
    seen_collated: Vec<Value>,
    count: i64,
    sum_ctx: SumCtx,
    best: Option<Value>,
    concat_buf: String,
    concat_count: usize,
    concat_sep_lens: VecDeque<usize>,
    json_accum: Option<Box<JsonGroupAccum>>,
}

#[inline]
fn value_to_text_in_encoding(v: &Value, encoding: TextEncoding) -> String {
    match v {
        Value::Blob(b) => encoding.decode_bytes(b),
        other => other.to_text_lossy(),
    }
}

impl AggregateAccumulator {
    pub fn new(kind: AggregateKind, distinct: bool) -> Self {
        Self::new_with_collation(kind, distinct, Collation::Binary)
    }

    pub fn new_with_collation(kind: AggregateKind, distinct: bool, collation: Collation) -> Self {
        let json_accum = match kind {
            AggregateKind::JsonGroupArray => Some(Box::new(JsonGroupAccum::new_array(false))),
            AggregateKind::JsonbGroupArray => Some(Box::new(JsonGroupAccum::new_array(true))),
            AggregateKind::JsonGroupObject => Some(Box::new(JsonGroupAccum::new_object(false))),
            AggregateKind::JsonbGroupObject => Some(Box::new(JsonGroupAccum::new_object(true))),
            _ => None,
        };
        Self {
            kind,
            distinct,
            collation,
            encoding: TextEncoding::Utf8,
            seen: HashSet::new(),
            seen_collated: Vec::new(),
            count: 0,
            sum_ctx: SumCtx::default(),
            best: None,
            concat_buf: String::new(),
            concat_count: 0,
            concat_sep_lens: VecDeque::new(),
            json_accum,
        }
    }

    #[inline]
    pub fn with_encoding(mut self, encoding: TextEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    #[inline]
    pub fn kind(&self) -> AggregateKind {
        self.kind
    }

    /// Returns `true` if this accumulator supports O(1) sliding-frame row removal via
    /// [`Self::inverse`] (`xInverse` in `sqlite/src/func.c` / `json.c`).
    #[inline]
    pub fn supports_inverse(&self) -> bool {
        !self.distinct
            && matches!(
                self.kind,
                AggregateKind::CountStar
                    | AggregateKind::Count
                    | AggregateKind::Sum
                    | AggregateKind::Total
                    | AggregateKind::Avg
                    | AggregateKind::GroupConcat
                    | AggregateKind::JsonGroupArray
                    | AggregateKind::JsonbGroupArray
                    | AggregateKind::JsonGroupObject
                    | AggregateKind::JsonbGroupObject
            )
    }

    /// Feeds one row with [`RuntimeValue`] subtype tags preserved (`JSON_SUBTYPE = 74`).
    pub fn step_rt(&mut self, args: &[RuntimeValue]) -> Result<()> {
        let coll = self.collation;
        self.step_rt_with_cmp(args, |a, b| Ok(a.cmp_sqlite(b, coll)))
    }

    /// Feeds one row with [`RuntimeValue`] subtype tags preserved and a fallible comparator.
    pub fn step_rt_with_cmp<F>(&mut self, args: &[RuntimeValue], cmp_fn: F) -> Result<()>
    where
        F: FnOnce(&Value, &Value) -> Result<std::cmp::Ordering>,
    {
        if let Some(jacc) = &mut self.json_accum {
            if self.distinct {
                let Some(first) = args.first().map(|rv| &rv.value) else {
                    return Ok(());
                };
                if self.collation == Collation::Binary {
                    let key = sqliters_types::encode_record(std::slice::from_ref(first));
                    if !self.seen.insert(key) {
                        return Ok(());
                    }
                } else {
                    let already = if first.is_null() {
                        self.seen_collated.iter().any(Value::is_null)
                    } else {
                        self.seen_collated
                            .iter()
                            .any(|v| !v.is_null() && v.cmp_sqlite(first, self.collation).is_eq())
                    };
                    if already {
                        return Ok(());
                    }
                    self.seen_collated.push(first.clone());
                }
            }
            return jacc.step(args);
        }
        let plain: Vec<Value> = args.iter().map(|rv| rv.value.clone()).collect();
        self.step_with_cmp(&plain, cmp_fn)
    }

    pub fn step(&mut self, args: &[Value]) -> Result<()> {
        let coll = self.collation;
        self.step_with_cmp(args, |a, b| Ok(a.cmp_sqlite(b, coll)))
    }

    /// Feeds one row into this accumulator using a fallible value comparator `cmp_fn`
    /// (supporting custom connection collations on `MIN` / `MAX`).
    pub fn step_with_cmp<F>(&mut self, args: &[Value], cmp_fn: F) -> Result<()>
    where
        F: FnOnce(&Value, &Value) -> Result<std::cmp::Ordering>,
    {
        if self.json_accum.is_some() {
            let rt_args: Vec<RuntimeValue> =
                args.iter().cloned().map(RuntimeValue::plain).collect();
            return self.step_rt_with_cmp(&rt_args, cmp_fn);
        }
        if self.kind == AggregateKind::CountStar {
            self.count += 1;
            return Ok(());
        }
        let Some(first) = args.first() else {
            return Ok(());
        };
        if first.is_null() {
            return Ok(());
        }
        if self.distinct {
            if self.collation == Collation::Binary {
                let key = sqliters_types::encode_record(std::slice::from_ref(first));
                if !self.seen.insert(key) {
                    return Ok(());
                }
            } else {
                if self
                    .seen_collated
                    .iter()
                    .any(|v| v.cmp_sqlite(first, self.collation).is_eq())
                {
                    return Ok(());
                }
                self.seen_collated.push(first.clone());
            }
        }

        match self.kind {
            AggregateKind::CountStar => unreachable!(),
            AggregateKind::Count => {
                self.count += 1;
            }
            AggregateKind::Sum | AggregateKind::Total | AggregateKind::Avg => {
                self.count += 1;
                self.sum_ctx.step_val(first);
            }
            AggregateKind::Min => {
                let replace = match &self.best {
                    None => true,
                    Some(cur) => cmp_fn(first, cur)?.is_lt(),
                };
                if replace {
                    self.best = Some(first.clone());
                }
            }
            AggregateKind::Max => {
                let replace = match &self.best {
                    None => true,
                    Some(cur) => cmp_fn(first, cur)?.is_gt(),
                };
                if replace {
                    self.best = Some(first.clone());
                }
            }
            AggregateKind::GroupConcat => {
                // Match `groupConcatStep` (`sqlite/src/func.c:2206-2274`):
                // 1-arg uses ","; 2-arg with NULL separator uses "" (`nSep = 0`).
                let text = value_to_text_in_encoding(first, self.encoding);
                if self.concat_count == 0 {
                    self.concat_buf.push_str(&text);
                    self.concat_count = 1;
                } else {
                    let sep = match args.get(1) {
                        None => ",".to_owned(),
                        Some(Value::Null) => String::new(),
                        Some(v) => value_to_text_in_encoding(v, self.encoding),
                    };
                    self.concat_sep_lens.push_back(sep.len());
                    self.concat_buf.push_str(&sep);
                    self.concat_buf.push_str(&text);
                    self.concat_count += 1;
                }
            }
            AggregateKind::JsonGroupArray
            | AggregateKind::JsonbGroupArray
            | AggregateKind::JsonGroupObject
            | AggregateKind::JsonbGroupObject => unreachable!(),
        }
        Ok(())
    }

    /// Removes a departing window-frame row with [`RuntimeValue`] args (`xInverse`).
    pub fn inverse_rt(&mut self, args: &[RuntimeValue]) -> Result<()> {
        if !self.supports_inverse() {
            return Err(SqliteError::misuse(
                "aggregate accumulator does not support inverse",
            ));
        }
        if let Some(jacc) = &mut self.json_accum {
            jacc.inverse();
            return Ok(());
        }
        let plain: Vec<Value> = args.iter().map(|rv| rv.value.clone()).collect();
        self.inverse(&plain)
    }

    /// Removes a departing window-frame row from this accumulator (`xInverse` in `sqlite/src/func.c`).
    pub fn inverse(&mut self, args: &[Value]) -> Result<()> {
        if !self.supports_inverse() {
            return Err(SqliteError::misuse(
                "aggregate accumulator does not support inverse",
            ));
        }
        if let Some(jacc) = &mut self.json_accum {
            jacc.inverse();
            return Ok(());
        }
        match self.kind {
            AggregateKind::CountStar => {
                self.count -= 1;
            }
            AggregateKind::Count => {
                if let Some(first) = args.first()
                    && !first.is_null()
                {
                    self.count -= 1;
                }
            }
            AggregateKind::Sum | AggregateKind::Total | AggregateKind::Avg => {
                if let Some(first) = args.first()
                    && !first.is_null()
                {
                    self.count -= 1;
                    self.sum_ctx.inverse_val(first);
                }
            }
            AggregateKind::GroupConcat => {
                let Some(first) = args.first() else {
                    return Ok(());
                };
                if first.is_null() {
                    return Ok(());
                }
                let val_len = value_to_text_in_encoding(first, self.encoding).len();
                self.concat_count = self.concat_count.saturating_sub(1);
                let sep_len = self.concat_sep_lens.pop_front().unwrap_or(0);
                let remove_bytes = (val_len + sep_len).min(self.concat_buf.len());
                self.concat_buf.drain(..remove_bytes);
            }
            AggregateKind::Min | AggregateKind::Max => {
                return Err(SqliteError::misuse(
                    "min/max accumulator does not support inverse",
                ));
            }
            AggregateKind::JsonGroupArray
            | AggregateKind::JsonbGroupArray
            | AggregateKind::JsonGroupObject
            | AggregateKind::JsonbGroupObject => unreachable!(),
        }
        Ok(())
    }

    /// Reads the current window value with [`RuntimeValue`] subtype preserved (`xValue`).
    pub fn value_rt(&self) -> Result<RuntimeValue> {
        if let Some(jacc) = &self.json_accum {
            return jacc.compute();
        }
        Ok(RuntimeValue::plain(self.value()?))
    }

    pub fn finalize_rt(&self) -> Result<RuntimeValue> {
        self.value_rt()
    }

    /// Reads the current window value without consuming or resetting accumulator state (`xValue`).
    pub fn value(&self) -> Result<Value> {
        if let Some(jacc) = &self.json_accum {
            return jacc.compute().map(|rv| rv.value);
        }
        match self.kind {
            AggregateKind::CountStar | AggregateKind::Count => Ok(Value::Integer(self.count)),
            AggregateKind::Sum => {
                if self.count == 0 {
                    return Ok(Value::Null);
                }
                if self.sum_ctx.approx {
                    if self.sum_ctx.ovrfl {
                        Err(SqliteError::new(ErrorCode::Mismatch, "integer overflow"))
                    } else {
                        Ok(Value::real(self.sum_ctx.combined_f64()))
                    }
                } else {
                    Ok(Value::Integer(self.sum_ctx.i_sum))
                }
            }
            AggregateKind::Total => {
                if self.count == 0 && !self.sum_ctx.approx {
                    Ok(Value::Real(0.0))
                } else {
                    Ok(Value::real(self.sum_ctx.combined_f64()))
                }
            }
            AggregateKind::Avg => {
                if self.count == 0 {
                    Ok(Value::Null)
                } else {
                    Ok(Value::real(
                        self.sum_ctx.combined_f64() / (self.count as f64),
                    ))
                }
            }
            AggregateKind::Min | AggregateKind::Max => Ok(self.best.clone().unwrap_or(Value::Null)),
            AggregateKind::GroupConcat => {
                if self.concat_count == 0 {
                    Ok(Value::Null)
                } else {
                    Ok(Value::Text(self.concat_buf.clone()))
                }
            }
            AggregateKind::JsonGroupArray
            | AggregateKind::JsonbGroupArray
            | AggregateKind::JsonGroupObject
            | AggregateKind::JsonbGroupObject => unreachable!(),
        }
    }

    pub fn finalize(&self) -> Result<Value> {
        self.value()
    }
}

/// Reusable table of open built-in [`AggregateAccumulator`] instances backing
/// [`sqliters_types::ExecutionServices`] (`agg_open`, `agg_step`, `agg_value`, `agg_inverse`, `agg_finalize`).
#[derive(Debug, Default, Clone)]
pub struct AccumulatorTable {
    slots: Vec<Option<(String, FunctionFlags, AggregateAccumulator)>>,
}

impl AccumulatorTable {
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(
        &mut self,
        name: &str,
        arity: usize,
        distinct: bool,
        collation: Collation,
        encoding: TextEncoding,
    ) -> Result<AggregateStepId> {
        let upper = name.to_ascii_uppercase();
        let kind = match (upper.as_str(), arity) {
            ("COUNT", 0) => AggregateKind::CountStar,
            ("COUNT", 1) => AggregateKind::Count,
            ("SUM", 1) => AggregateKind::Sum,
            ("TOTAL", 1) => AggregateKind::Total,
            ("AVG", 1) => AggregateKind::Avg,
            ("MIN", 1) => AggregateKind::Min,
            ("MAX", 1) => AggregateKind::Max,
            ("GROUP_CONCAT", 1 | 2) | ("STRING_AGG", 2) => AggregateKind::GroupConcat,
            ("JSON_GROUP_ARRAY", 1) => AggregateKind::JsonGroupArray,
            ("JSONB_GROUP_ARRAY", 1) => AggregateKind::JsonbGroupArray,
            ("JSON_GROUP_OBJECT", 2) => AggregateKind::JsonGroupObject,
            ("JSONB_GROUP_OBJECT", 2) => AggregateKind::JsonbGroupObject,
            (
                "COUNT" | "SUM" | "TOTAL" | "AVG" | "MIN" | "MAX" | "GROUP_CONCAT" | "STRING_AGG"
                | "JSON_GROUP_ARRAY" | "JSONB_GROUP_ARRAY" | "JSON_GROUP_OBJECT"
                | "JSONB_GROUP_OBJECT",
                _,
            ) => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            _ => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such function: {name}"),
                ));
            }
        };
        let acc = AggregateAccumulator::new_with_collation(kind, distinct, collation)
            .with_encoding(encoding);
        let flags = kind.function_flags();
        let entry = (name.to_ascii_lowercase(), flags, acc);
        if let Some((idx, slot)) = self.slots.iter_mut().enumerate().find(|(_, s)| s.is_none()) {
            *slot = Some(entry);
            Ok(AggregateStepId(idx))
        } else {
            let idx = self.slots.len();
            self.slots.push(Some(entry));
            Ok(AggregateStepId(idx))
        }
    }

    #[inline]
    pub fn supports_inverse(&self, id: AggregateStepId) -> bool {
        self.slots
            .get(id.0)
            .and_then(Option::as_ref)
            .is_some_and(|(_, _, acc)| acc.supports_inverse())
    }

    pub fn step(&mut self, id: AggregateStepId, args: &[RuntimeValue]) -> Result<()> {
        let Some(Some((_, flags, acc))) = self.slots.get_mut(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate step on unopened accumulator",
            ));
        };
        let mut scratch = Vec::new();
        let filtered = strip_unsubscribed_subtypes(*flags, args, &mut scratch);
        acc.step_rt(filtered)
    }

    pub fn value(&self, id: AggregateStepId, strict_subtype: bool) -> Result<RuntimeValue> {
        let Some(Some((name, flags, acc))) = self.slots.get(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate value on unopened accumulator",
            ));
        };
        let rv = acc.value_rt()?;
        enforce_result_subtype_contract(name, *flags, strict_subtype, rv)
    }

    pub fn inverse(&mut self, id: AggregateStepId, args: &[RuntimeValue]) -> Result<()> {
        let Some(Some((_, flags, acc))) = self.slots.get_mut(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate inverse on unopened accumulator",
            ));
        };
        let mut scratch = Vec::new();
        let filtered = strip_unsubscribed_subtypes(*flags, args, &mut scratch);
        acc.inverse_rt(filtered)
    }

    pub fn finalize(&mut self, id: AggregateStepId, strict_subtype: bool) -> Result<RuntimeValue> {
        let Some(slot) = self.slots.get_mut(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate finalize on unopened accumulator",
            ));
        };
        let Some((name, flags, acc)) = slot.take() else {
            return Err(SqliteError::misuse(
                "aggregate finalize on unopened accumulator",
            ));
        };
        let rv = acc.finalize_rt()?;
        enforce_result_subtype_contract(&name, flags, strict_subtype, rv)
    }
}

#[cfg(test)]
mod swar_like_tests {
    use super::*;

    #[test]
    fn swar_contains_and_folded_eq_boundaries_and_borrow_patterns() {
        // Borrow-adjacent byte pattern: lane 0 matches (`xor == 0x00`), lane 1 has `xor == 0x01`
        // (`t` followed by `u`, where `'u' ^ 't' == 0x01`). Must never false-match!
        let pat = CompiledLikePattern::compile("%token42%", None, false);
        assert!(!pat.matches_utf8_bytes(b"tuXXXX22YYYYYYYYYYYY"));
        assert!(pat.matches_utf8_bytes(b"tuXXXX2YYYYYToKeN42Z"));
        assert!(pat.matches_utf8_bytes(b"TOKEN42"));
        assert!(!pat.matches_utf8_bytes(b"TOKEN4"));

        let pat_cs = CompiledLikePattern::compile("%Token42%", None, true);
        assert!(pat_cs.matches_utf8_bytes(b"xxToken42yy"));
        assert!(!pat_cs.matches_utf8_bytes(b"xxtoken42yy"));

        for m in 1..=18usize {
            let needle: String = (0..m).map(|k| (b'a' + (k % 26) as u8) as char).collect();
            let like_pat = format!("%{}%", needle.to_ascii_uppercase());
            let compiled = CompiledLikePattern::compile(&like_pat, None, false);
            for pad_l in [0usize, 1, 3, 7, 8, 9, 15, 16, 23] {
                for pad_r in [0usize, 1, 4, 8, 13] {
                    let hay = format!("{}{}{}", "-".repeat(pad_l), needle, ".".repeat(pad_r));
                    assert!(
                        compiled.matches_utf8_bytes(hay.as_bytes()),
                        "expected match m={m} pad_l={pad_l} pad_r={pad_r}"
                    );
                    assert!(sqlite_like_match(&like_pat, &hay, None));
                }
            }
        }
    }
}
