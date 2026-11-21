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
    /// Rows changed by the nested statements of virtual-table modules (the statements a C
    /// module steps on the host connection). Part of `total_changes()`; kept apart from
    /// `total_changes` because C does not take these rows back when the statement that
    /// caused them fails.
    pub nested_changes: u64,
    /// UTC Unix epoch milliseconds captured at the start of statement execution (`sqlite3StmtCurrentTime`).
    pub stmt_timestamp_ms: Option<i64>,
    /// Enables `testfixture` SQL functions (`randstr`, `sqlite/src/test_func.c`).
    pub enable_test_functions: bool,
    /// Text encoding of the database (`sqlite3.enc`): `hex()` and `octet_length()` see the
    /// stored bytes of a TEXT value, which are UTF-16 in a UTF-16 database.
    pub text_encoding: sqliters_types::TextEncoding,
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
    let sign = if y < 0 { "-" } else { "" };
    let year = y.unsigned_abs();
    format!("{sign}{year:04}-{m:02}-{d:02}")
}

pub fn format_utc_time(unix_ms: i64) -> String {
    let (_, _, _, hh, mm, ss) = utc_ymdhms_from_unix_ms(unix_ms);
    format!("{hh:02}:{mm:02}:{ss:02}")
}

pub fn format_utc_timestamp(unix_ms: i64) -> String {
    let (y, m, d, hh, mm, ss) = utc_ymdhms_from_unix_ms(unix_ms);
    let sign = if y < 0 { "-" } else { "" };
    let year = y.unsigned_abs();
    format!("{sign}{year:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
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
        "RANDOM" | "RANDOMBLOB" | "LAST_INSERT_ROWID" | "CHANGES" | "TOTAL_CHANGES"
        | "SQLITE_COMPILEOPTION_GET" | "SQLITE_COMPILEOPTION_USED" => {
            Some(FunctionFlags::INNOCUOUS)
        }
        "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP" | "DATE" | "TIME" | "DATETIME"
        | "UNIXEPOCH" | "JULIANDAY" | "STRFTIME" | "TIMEDIFF" => Some(FunctionFlags::INNOCUOUS),
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
                // minmaxFunc (func.c): min() moves to the later argument on a tie.
                if rv.value.cmp_sqlite(&best.value, Collation::Binary).is_le() {
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
            check_arity_rt(name, filtered, 0, usize::MAX)?;
            eval_json_edit(name, filtered, EditMode::Insert, upper == "JSONB_INSERT")?
        }
        "JSON_REPLACE" | "JSONB_REPLACE" => {
            check_arity_rt(name, filtered, 0, usize::MAX)?;
            eval_json_edit(name, filtered, EditMode::Replace, upper == "JSONB_REPLACE")?
        }
        "JSON_SET" | "JSONB_SET" => {
            check_arity_rt(name, filtered, 0, usize::MAX)?;
            eval_json_edit(name, filtered, EditMode::Set, upper == "JSONB_SET")?
        }
        "JSON_ARRAY_INSERT" | "JSONB_ARRAY_INSERT" => {
            check_arity_rt(name, filtered, 0, usize::MAX)?;
            eval_json_edit(
                name,
                filtered,
                EditMode::ArrayInsert,
                upper == "JSONB_ARRAY_INSERT",
            )?
        }
        "JSON_REMOVE" | "JSONB_REMOVE" => {
            check_arity_rt(name, filtered, 0, usize::MAX)?;
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
    // Byte-exact ports of func.c; `None` = not ported or wrong argument count (the arms below
    // then report the arity error).
    if matches!(
        upper.as_bytes().first(),
        Some(b'C' | b'G' | b'H' | b'I' | b'L' | b'M' | b'O' | b'Q' | b'R' | b'S' | b'T' | b'U')
    ) && let Some(r) = crate::func_c::eval(upper, args, ctx)
    {
        return r;
    }
    match upper {
        "CONCAT" | "CONCAT_WS" | "UNISTR" | "UNISTR_QUOTE"
        | "SQLITE_COMPILEOPTION_GET" | "SQLITE_COMPILEOPTION_USED" => Err(SqliteError::new(
            ErrorCode::Error,
            format!("wrong number of arguments to function {name}()"),
        )),
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
                    .ok_or_else(|| SqliteError::new(ErrorCode::Error, "integer overflow")),
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
                // "%!0.17g": infinity is shown as 9.0e+999.
                Value::Real(r) if r.is_infinite() => {
                    if *r > 0.0 { "9.0e+999" } else { "-9.0e+999" }.to_owned()
                }
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
            // func.c printfFunc: no arguments or a NULL format give NULL.
            if args.is_empty() || args[0].is_null() {
                return Ok(Value::Null);
            }
            let fmt_owned;
            let fmt: &[u8] = match &args[0] {
                Value::Text(s) => s.as_bytes(),
                Value::Blob(b) => b.as_slice(),
                other => {
                    fmt_owned = other.to_text_lossy();
                    fmt_owned.as_bytes()
                }
            };
            match crate::printf::sqlite_printf(fmt, &args[1..]) {
                Ok(bytes) => Ok(Value::Text(match String::from_utf8(bytes) {
                    Ok(s) => s,
                    Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
                })),
                Err(crate::printf::PrintfTooBig) => Err(SqliteError::new(
                    ErrorCode::TooBig,
                    "string or blob too big",
                )),
            }
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
            let source_id = SQLITE_SOURCE_ID
                .strip_suffix(" (sqliters)")
                .unwrap_or(SQLITE_SOURCE_ID);
            Ok(Value::Text(source_id.to_owned()))
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
            Ok(Value::Integer(
                (ctx.total_changes + ctx.nested_changes) as i64,
            ))
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
        "TIMEDIFF" => {
            check_arity(name, args, 2, 2)?;
            Ok(eval_timediff(&args[0], &args[1], ctx))
        }
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
        | "SQLITE_COMPILEOPTION_GET"
        | "SQLITE_COMPILEOPTION_USED"
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
        | "JSONB_PATCH" | "POW" | "POWER" | "ATAN2" | "MOD" | "TIMEDIFF" => Some((2, 2)),
        "RANDSTR" | "TEST_SET_SUBTYPE" | "TEST_BAD_SUBTYPE" if enable_test_functions => {
            Some((2, 2))
        }
        "TEST_ERROR" if enable_test_functions => Some((1, 2)),
        "TEST_PLAIN_ARG_SUBTYPE" if enable_test_functions => Some((1, 1)),
        "REPLACE" => Some((3, 3)),
        "UNISTR" | "UNISTR_QUOTE" => Some((1, 1)),
        "CONCAT" => Some((1, usize::MAX)),
        "CONCAT_WS" => Some((2, usize::MAX)),
        "TRIM" | "LTRIM" | "RTRIM" | "ROUND" | "UNHEX" | "LOG" | "JSON_ARRAY_LENGTH"
        | "JSON_TYPE" | "JSON_VALID" | "JSON_PRETTY" => Some((1, 2)),
        "SUBSTR" | "SUBSTRING" | "LIKE" => Some((2, 3)),
        "CHAR" | "DATE" | "TIME" | "DATETIME" | "UNIXEPOCH" | "JULIANDAY" | "STRFTIME"
        | "JSON_ARRAY" | "JSONB_ARRAY" | "JSON_OBJECT" | "JSONB_OBJECT" | "JSON_EXTRACT"
        | "JSONB_EXTRACT" => Some((0, usize::MAX)),
        "PRINTF" | "FORMAT" | "JSON_INSERT" | "JSONB_INSERT" | "JSON_REPLACE" | "JSONB_REPLACE"
        | "JSON_SET" | "JSONB_SET" | "JSON_REMOVE" | "JSONB_REMOVE" | "JSON_ARRAY_INSERT"
        | "JSONB_ARRAY_INSERT" => Some((0, usize::MAX)),
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
            // parse.y builds fn(*) with an empty argument list.
            let argc = if star_args { 0 } else { arg_count };
            if argc < min || argc > max {
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

const UNIX_EPOCH_JULIAN_MS: i64 = 210_866_760_000_000;
const JULIAN_MS_LIMIT: i64 = 464_269_060_800_000;

fn valid_julian_unix_ms(unix_ms: i64) -> bool {
    (-UNIX_EPOCH_JULIAN_MS..JULIAN_MS_LIMIT - UNIX_EPOCH_JULIAN_MS).contains(&unix_ms)
}

#[derive(Clone, Copy)]
struct BareHour24 {
    offset_ms: i64,
    legacy_accepted: bool,
}

#[derive(Clone, Copy, Default)]
struct ParsedDateTime {
    unix_ms: i64,
    raw_seconds: Option<f64>,
    is_utc: bool,
    signed_hour24: bool,
    calendar_retained: bool,
    bare_hour24: Option<BareHour24>,
}

impl ParsedDateTime {
    fn from_unix_ms(unix_ms: i64) -> Self {
        Self {
            unix_ms,
            ..Self::default()
        }
    }
}

fn parse_datetime_value(
    val: &Value,
    modifiers: &[Value],
    ctx: &VmRuntimeContext,
) -> Option<(ParsedDateTime, bool)> {
    if val.is_null() || modifiers.iter().any(Value::is_null) {
        return None;
    }
    let packed_hms = match modifiers {
        [Value::Text(text)] => parse_packed_hms_modifier(text),
        _ => None,
    };
    // The signed date-and-clock shift applies to the ordinary unmodified value.
    let modifiers = if packed_hms.is_some() { &[] } else { modifiers };
    // Host-local conversion for a signed ISO year is outside this parser path.
    let allow_signed_year = !matches!(val, Value::Text(text) if text.starts_with('-')
        && modifiers.iter().any(|m| m.to_text_lossy().trim().eq_ignore_ascii_case("utc")));
    let first_modifier = modifiers
        .first()
        .map(|modifier| value_to_text_in_encoding(modifier, ctx.text_encoding));
    let first_name = first_modifier
        .as_deref()
        .map(datetime_c_string)
        .unwrap_or_default();

    let mut single_typed_modifier_days = None;
    let mut single_typed_modifier_fields = None;
    let mut typed_modifier_fields = None;
    let mut typed_elapsed_chain = false;
    let (mut time, modifiers, check_julian_range) = if first_name.eq_ignore_ascii_case("unixepoch")
    {
        let seconds = parse_raw_datetime_number(val, ctx.text_encoding)?;
        (
            ParsedDateTime::from_unix_ms(unix_seconds_to_ms(seconds)?),
            &modifiers[1..],
            true,
        )
    } else if first_name.eq_ignore_ascii_case("julianday") {
        let days = parse_raw_datetime_number(val, ctx.text_encoding)?;
        (
            ParsedDateTime::from_unix_ms(julian_days_to_ms(days)?),
            &modifiers[1..],
            true,
        )
    } else if first_name.eq_ignore_ascii_case("auto") {
        (
            parse_auto_datetime(val, ctx, allow_signed_year)?,
            &modifiers[1..],
            true,
        )
    } else {
        let is_output_precision = |modifier: &Value| {
            matches!(modifier, Value::Text(text)
                if text.eq_ignore_ascii_case("subsec") || text.eq_ignore_ascii_case("subsecond"))
        };
        let (mut time, check_julian_range) = match val {
            Value::Integer(_) | Value::Real(_) => {
                let days = parse_raw_datetime_number(val, ctx.text_encoding)?;
                let mut other_modifiers = modifiers.iter().filter(|m| !is_output_precision(m));
                match other_modifiers.next() {
                    None => (ParsedDateTime::from_unix_ms(julian_days_to_ms(days)?), true),
                    Some(modifier) => {
                        let mut single_start_of = false;
                        if other_modifiers.next().is_some() {
                            // Reuse the numeric fields if the chain needs the legacy dispatcher.
                            let fields: Vec<_> = modifiers
                                .iter()
                                .map(|modifier| match modifier {
                                    Value::Text(text) => split_numeric_datetime_modifier(text),
                                    _ => None,
                                })
                                .collect();
                            typed_elapsed_chain =
                                modifiers.iter().zip(&fields).all(|(modifier, fields)| {
                                    is_output_precision(modifier)
                                        || fields.is_some_and(|fields| fields.elapsed.is_some())
                                });
                            typed_modifier_fields = Some(fields);
                        } else if let Value::Text(text) = modifier {
                            single_start_of = ["start of day", "start of month", "start of year"]
                                .iter()
                                .any(|name| text.eq_ignore_ascii_case(name));
                            let fields = split_numeric_datetime_modifier(text);
                            if let Some(fields) = fields
                                && let Some((month_scale, limit)) = fields.whole_calendar_unit()
                            {
                                let amount = fields.amount.parse::<i64>().ok()?;
                                if amount <= -limit || amount >= limit {
                                    return None;
                                }
                                let ms = julian_days_to_ms(days)
                                    .filter(|&ms| valid_julian_unix_ms(ms))?;
                                let shifted =
                                    apply_calendar_shift(ms, amount * month_scale, 0, false)?;
                                // All other modifiers are exact output precision flags.
                                return valid_julian_unix_ms(shifted).then_some((
                                    ParsedDateTime::from_unix_ms(shifted),
                                    modifiers.len() > 1,
                                ));
                            }
                            single_typed_modifier_fields = Some(fields);
                            single_typed_modifier_days = (!single_start_of).then_some(days);
                        }
                        let check_julian_range = typed_elapsed_chain || single_start_of;
                        let ms = if check_julian_range {
                            let ms = julian_days_to_ms(days)?;
                            // Start-of reads the calendar before producing its new Julian day.
                            if single_start_of && !valid_julian_unix_ms(ms) {
                                return None;
                            }
                            ms
                        } else {
                            ((days - 2_440_587.5) * 86_400_000.0).round() as i64
                        };
                        (ParsedDateTime::from_unix_ms(ms), check_julian_range)
                    }
                }
            }
            Value::Text(s) => parse_datetime_text(s, ctx, allow_signed_year).or_else(|| {
                if s.contains('\0') || !modifiers.iter().all(is_output_precision) {
                    return None;
                }
                let days = parse_raw_datetime_number(val, ctx.text_encoding)?;
                let ms = julian_days_to_ms(days)?;
                Some((ParsedDateTime::from_unix_ms(ms), true))
            })?,
            Value::Null | Value::Blob(_) => return None,
        };
        if let Value::Text(stamp) = val {
            let mut date_modifiers = modifiers.iter().filter(|m| !is_output_precision(m));
            if let (Some(Value::Text(modifier)), None) =
                (date_modifiers.next(), date_modifiers.next())
            {
                let finish = |unix_ms| {
                    valid_julian_unix_ms(unix_ms)
                        .then_some((ParsedDateTime::from_unix_ms(unix_ms), modifiers.len() > 1))
                };
                if modifier
                    .get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("end of "))
                {
                    return finish(end_of_iso_period_ms(time.unix_ms, stamp, modifier)?);
                }
                if let Some(amount) = modifier
                    .get(8..)
                    .filter(|_| modifier[..8].eq_ignore_ascii_case("weekday "))
                    // SQLite looks for a literal minus, including a negative zero or exponent.
                    && amount.contains('-')
                    && !amount.contains('\0')
                    && valid_julian_unix_ms(time.unix_ms)
                    && is_normal_iso_calendar(stamp)
                {
                    let (weekday, status) = sqliters_types::sqlite_atof(amount.as_bytes());
                    if status <= 0 || !(-6.0..=6.0).contains(&weekday) || weekday.fract() != 0.0 {
                        return None;
                    }
                    return finish(shift_weekday_ms(time.unix_ms, weekday.abs() as i64, true));
                }
            }
        }
        // A canonical date is already normalized and has no raw clock fields.
        let modifiers = if let Value::Text(date) = val
            && matches!(date.as_bytes(), [_, _, _, _, b'-', _, _, b'-', _, _])
        {
            let raw_day = || date.get(8..)?.parse::<u32>().ok();
            let floor_month_overflow = |unix_ms, original_day| {
                let normalized_day = utc_ymdhms_from_unix_ms(unix_ms).2;
                // After a month rollover the new day counts the overflow days.
                if normalized_day == original_day {
                    unix_ms
                } else {
                    unix_ms - i64::from(normalized_day) * 86_400_000
                }
            };
            match modifiers {
                [Value::Text(modifier)]
                    if modifier.eq_ignore_ascii_case("ceiling")
                        || modifier.eq_ignore_ascii_case("floor") =>
                {
                    if modifier.eq_ignore_ascii_case("floor") {
                        time.unix_ms = floor_month_overflow(time.unix_ms, raw_day()?);
                    }
                    &modifiers[..0]
                }
                [Value::Text(modifier)] => {
                    if let Some(unix_ms) =
                        apply_packed_calendar_shift(time.unix_ms, date, modifier, false)
                    {
                        time.unix_ms = unix_ms;
                        &modifiers[..0]
                    } else {
                        modifiers
                    }
                }
                [Value::Text(shift), Value::Text(modifier)]
                    if modifier.eq_ignore_ascii_case("ceiling")
                        || modifier.eq_ignore_ascii_case("floor") =>
                {
                    if let Some(unix_ms) = apply_packed_calendar_shift(
                        time.unix_ms,
                        date,
                        shift,
                        modifier.eq_ignore_ascii_case("floor"),
                    ) {
                        time.unix_ms = unix_ms;
                        &modifiers[..0]
                    } else {
                        let original_day = raw_day()?;
                        if original_day == utc_ymdhms_from_unix_ms(time.unix_ms).2
                            && let Some(fields) = split_numeric_datetime_modifier(shift)
                            && fields.is_signed_integer_calendar_shift()
                        {
                            time.unix_ms =
                                apply_datetime_modifier_ms(&mut time, shift, None, false, || {
                                    Some(fields)
                                })?;
                            if modifier.eq_ignore_ascii_case("floor") {
                                // A whole shift cannot roll the last day of December into year 10000.
                                if !valid_julian_unix_ms(time.unix_ms) {
                                    return None;
                                }
                                time.unix_ms = floor_month_overflow(time.unix_ms, original_day);
                            }
                            &modifiers[..0]
                        } else {
                            modifiers
                        }
                    }
                }
                _ => modifiers,
            }
        } else {
            modifiers
        };
        (time, modifiers, check_julian_range)
    };

    if let Some((shift, clock_ms)) = packed_hms {
        if !check_julian_range
            || time.bare_hour24.is_some()
            || time.signed_hour24
            || matches!(val, Value::Text(text) if has_raw_calendar_overflow(text))
        {
            return None;
        }
        let shifted = apply_calendar_shift(
            time.unix_ms,
            shift.direction * shift.months,
            shift.direction * shift.days,
            false,
        )?;
        time = ParsedDateTime::from_unix_ms(shifted + shift.direction * clock_ms);
    }
    let initial_time = time;
    let mut bare_hour24 = time.bare_hour24;
    let use_subsec = 'modifiers: loop {
        let mut use_subsec = false;
        for (index, m) in modifiers.iter().enumerate() {
            let mod_str = m.to_text_lossy();
            if mod_str.eq_ignore_ascii_case("subsec") || mod_str.eq_ignore_ascii_case("subsecond") {
                use_subsec = true;
                continue;
            }
            let mt = mod_str.trim();
            if mt.eq_ignore_ascii_case("utc") {
                if !time.is_utc {
                    // Continue to use the previous path for host timezone conversion.
                    if let Some(clock) = bare_hour24.take() {
                        if !clock.legacy_accepted {
                            return None;
                        }
                        time = initial_time;
                        time.bare_hour24 = None;
                        time.is_utc = true;
                        continue 'modifiers;
                    }
                    time.raw_seconds = None;
                }
                continue;
            }
            let numeric_modifier = || match (&typed_modifier_fields, m) {
                (Some(fields), Value::Text(_)) => fields[index],
                _ => single_typed_modifier_fields
                    .unwrap_or_else(|| split_numeric_datetime_modifier(&mod_str)),
            };
            time.unix_ms = apply_datetime_modifier_ms(
                &mut time,
                &mod_str,
                single_typed_modifier_days,
                typed_elapsed_chain,
                numeric_modifier,
            )?;
            time.raw_seconds = None;
        }
        break use_subsec;
    };

    if check_julian_range && !valid_julian_unix_ms(time.unix_ms) {
        return None;
    }
    Some((time, use_subsec))
}

fn datetime_c_string(text: &str) -> &str {
    text.split_once('\0').map_or(text, |(prefix, _)| prefix)
}

fn parse_datetime_text(
    text: &str,
    ctx: &VmRuntimeContext,
    allow_signed_year: bool,
) -> Option<(ParsedDateTime, bool)> {
    if text.trim().eq_ignore_ascii_case("now") {
        Some((
            ParsedDateTime::from_unix_ms(ctx.effective_timestamp_ms()),
            false,
        ))
    } else {
        // Validate the ISO timestamp after all modifiers have run.
        parse_iso_datetime(text, allow_signed_year).map(|time| (time, true))
    }
}

fn parse_raw_datetime_number(val: &Value, encoding: TextEncoding) -> Option<f64> {
    match val {
        Value::Integer(value) => Some(*value as f64),
        Value::Real(value) => Some(*value),
        Value::Text(_) | Value::Blob(_) => {
            let text = value_to_text_in_encoding(val, encoding);
            let (value, result) = sqliters_types::sqlite_atof(text.as_bytes());
            (result > 0).then_some(value)
        }
        Value::Null => None,
    }
}

fn parse_auto_datetime(
    val: &Value,
    ctx: &VmRuntimeContext,
    allow_signed_year: bool,
) -> Option<ParsedDateTime> {
    if let Some(number) = parse_raw_datetime_number(val, ctx.text_encoding) {
        if let Some(ms) = julian_days_to_ms(number) {
            return Some(ParsedDateTime::from_unix_ms(ms));
        }
        // The auto Unix range ends at the last whole second of year 9999.
        if (-210_866_760_000.0..=253_402_300_799.0).contains(&number) {
            return unix_seconds_to_ms(number).map(ParsedDateTime::from_unix_ms);
        }
        return None;
    }

    match val {
        Value::Text(text) => {
            let prefix = datetime_c_string(text);
            parse_iso_datetime(prefix, allow_signed_year && prefix.len() == text.len())
                .or_else(|| parse_datetime_text(text, ctx, allow_signed_year).map(|(time, _)| time))
        }
        Value::Blob(_) => {
            let text = value_to_text_in_encoding(val, ctx.text_encoding);
            parse_iso_datetime(datetime_c_string(&text), false)
        }
        _ => None,
    }
}

fn julian_days_to_ms(days: f64) -> Option<i64> {
    (0.0..5373484.5)
        .contains(&days)
        .then(|| (days * 86_400_000.0 + 0.5) as i64 - UNIX_EPOCH_JULIAN_MS)
}

fn unix_seconds_to_ms(seconds: f64) -> Option<i64> {
    // C checks the unrounded Julian value before applying any later modifiers.
    let julian_ms = seconds * 1_000.0 + UNIX_EPOCH_JULIAN_MS as f64;
    (0.0..JULIAN_MS_LIMIT as f64)
        .contains(&julian_ms)
        .then(|| (julian_ms + 0.5) as i64 - UNIX_EPOCH_JULIAN_MS)
}

fn parse_iso_datetime(input: &str, allow_signed_year: bool) -> Option<ParsedDateTime> {
    const YEAR_10000_MS: i64 = 253_402_300_800_000;
    let signed_date = input.strip_prefix('-').filter(|date| {
        allow_signed_year
            && date.as_bytes().get(..5).is_some_and(|prefix| {
                prefix[4] == b'-' && prefix[..4].iter().all(u8::is_ascii_digit)
            })
    });
    let s = signed_date
        .unwrap_or(input)
        .trim_end_matches(is_date_time_space);
    let signed = signed_date.is_some();
    let has_date = s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-';
    if signed && (!has_date || s.contains('\0')) {
        return None;
    }
    let (days, clock, signed_year_end) = if has_date {
        if !s.as_bytes()[..10]
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        {
            return None;
        }
        let mut y: i64 = s.get(0..4)?.parse().ok()?;
        let m: u32 = s.get(5..7)?.parse().ok()?;
        let d: u32 = s.get(8..10)?.parse().ok()?;
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        if signed {
            y = -y;
            if y < -4713 {
                return None;
            }
            let month_days =
                days_from_civil(y + i64::from(m == 12), m % 12 + 1, 1) - days_from_civil(y, m, 1);
            if i64::from(d) > month_days {
                return None;
            }
        }
        (
            days_from_civil(y, m, d),
            s.get(10..)?
                .trim_start_matches(|c| c == 'T' || is_date_time_space(c)),
            (signed && m == 12 && d == 31).then_some(y),
        )
    } else if s.contains(':') {
        // SQLite treats a bare time as being on 2000-01-01.
        (days_from_civil(2000, 1, 1), s, None)
    } else {
        return None;
    };
    let midnight_ms = days * 86_400_000;
    let local_clock_ms = |clock_ms| {
        if let Some(year) = signed_year_end.filter(|_| clock_ms == 86_400_000) {
            // SQLite advances the year magnitude before applying the minus sign.
            return (year > -4713).then(|| days_from_civil(year - 1, 1, 1) * 86_400_000);
        }
        Some(midnight_ms + clock_ms)
    };

    if let Some(offset_start) = clock.find(['+', '-']) {
        let (clock, offset) = clock.split_at(offset_start);
        let clock = clock.trim_end_matches(is_date_time_space);
        let (clock_ms, raw_seconds) = parse_hms_ms(clock, true)?;
        let local_ms = local_clock_ms(clock_ms)?;
        // A 24:00 clock advances the local date before applying its offset.
        if local_ms >= YEAR_10000_MS {
            return None;
        }
        let offset_ms = parse_numeric_time_offset_ms(offset)?;
        let utc_ms = local_ms - offset_ms;
        let signed_hour24 = signed && clock_ms == 86_400_000;
        let bare_hour24 = (!has_date && clock_ms == 86_400_000).then(|| BareHour24 {
            offset_ms,
            legacy_accepted: clock[3..]
                .bytes()
                .all(|byte| matches!(byte, b'0' | b':' | b'.')),
        });
        return (utc_ms < YEAR_10000_MS).then_some(ParsedDateTime {
            unix_ms: utc_ms,
            raw_seconds: if offset_ms == 0 { raw_seconds } else { None },
            is_utc: bare_hour24.is_none() || offset_ms == 0,
            signed_hour24,
            calendar_retained: signed_hour24 && offset_ms == 0,
            bare_hour24,
        });
    }

    let without_z = clock.strip_suffix(['Z', 'z']);
    let clock = without_z
        .unwrap_or(clock)
        .trim_end_matches(is_date_time_space);
    let hour24 = clock.starts_with("24:");
    if has_date && clock.is_empty() {
        if without_z.is_some() {
            return None;
        }
        return Some(ParsedDateTime::from_unix_ms(midnight_ms));
    }
    let (clock_ms, raw_seconds) = parse_hms_ms(clock, hour24)?;
    let unix_ms = local_clock_ms(clock_ms)?;
    let signed_hour24 = signed && hour24;
    if hour24 && unix_ms >= YEAR_10000_MS {
        return None;
    }
    Some(ParsedDateTime {
        unix_ms,
        raw_seconds,
        is_utc: without_z.is_some(),
        signed_hour24,
        calendar_retained: signed_hour24,
        bare_hour24: (!has_date && clock_ms == 86_400_000).then_some(BareHour24 {
            offset_ms: 0,
            legacy_accepted: false,
        }),
    })
}

fn is_date_time_space(c: char) -> bool {
    matches!(c, ' ' | '\x09'..='\x0d')
}

fn parse_two_time_digits(s: &str) -> Option<u32> {
    let [first, second] = s.as_bytes() else {
        return None;
    };
    if !first.is_ascii_digit() || !second.is_ascii_digit() {
        return None;
    }
    Some(u32::from(first - b'0') * 10 + u32::from(second - b'0'))
}

fn parse_numeric_time_offset_ms(s: &str) -> Option<i64> {
    let (hours, minutes) = s.get(1..)?.split_once(':')?;
    let hours = parse_two_time_digits(hours)?;
    let minutes = parse_two_time_digits(minutes)?;
    if hours > 14 || minutes > 59 {
        return None;
    }
    let ms = i64::from(hours * 60 + minutes) * 60_000;
    Some(if s.starts_with('-') { -ms } else { ms })
}

fn parse_hms_ms(s: &str, allow_hour24: bool) -> Option<(i64, Option<f64>)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    let hour24 = allow_hour24 && parts[0] == "24";
    let hh = parse_two_time_digits(parts[0])?;
    let mm = parse_two_time_digits(parts[1])?;
    let (ss, ms, raw_seconds) = if parts.len() == 3 {
        if let Some((sec_part, frac_part)) = parts[2].split_once('.') {
            let sec = parse_two_time_digits(sec_part)?;
            if frac_part.is_empty() || !frac_part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            if frac_part.len() > 3 {
                let (millis, seconds) = parse_long_clock_fraction(sec, frac_part)?;
                (sec, millis, Some(seconds))
            } else {
                let mut frac_digits: String = frac_part.chars().take(3).collect();
                while frac_digits.len() < 3 {
                    frac_digits.push('0');
                }
                (sec, frac_digits.parse().ok()?, None)
            }
        } else {
            (parse_two_time_digits(parts[2])?, 0, None)
        }
    } else {
        (0, 0, None)
    };
    if hh > 23 {
        let midnight = hour24
            && mm == 0
            && ss == 0
            && ms == 0
            && raw_seconds.is_none_or(|seconds| seconds == 0.0);
        if !midnight {
            return None;
        }
    }
    if mm > 59 || ss > 59 {
        return None;
    }
    let clock_ms =
        (i64::from(hh) * 3_600 + i64::from(mm) * 60 + i64::from(ss)) * 1_000 + i64::from(ms);
    Some((clock_ms, raw_seconds))
}

fn parse_long_clock_fraction(second: u32, digits: &str) -> Option<(u32, f64)> {
    let mut numerator = 0.0;
    let mut scale = 1.0;
    for digit in digits.bytes() {
        numerator = numerator * 10.0 + f64::from(digit) - f64::from(b'0');
        scale *= 10.0;
    }
    let fraction = numerator / scale;
    if fraction.is_nan() {
        return None;
    }
    // SQLite rounds after adding the whole second to the capped fraction.
    let seconds = f64::from(second) + fraction.min(0.999);
    let millis = (seconds * 1_000.0 + 0.5) as u32 - second * 1_000;
    Some((millis, seconds))
}

#[derive(Clone, Copy)]
struct NumericDateTimeModifier<'a> {
    amount: &'a str,
    unit: &'a str,
    elapsed: Option<(f64, f32)>,
}

impl NumericDateTimeModifier<'_> {
    fn whole_calendar_unit(&self) -> Option<(i64, i64)> {
        let digits = self.amount.strip_prefix(['+', '-']).unwrap_or(self.amount);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if self.unit.eq_ignore_ascii_case("month") {
            Some((1, 176_546))
        } else if self.unit.eq_ignore_ascii_case("year") {
            Some((12, 14_713))
        } else {
            None
        }
    }

    fn is_signed_integer_calendar_shift(&self) -> bool {
        self.amount.starts_with(['+', '-'])
            && self.amount.parse::<i32>().is_ok_and(|amount| {
                let magnitude = amount.unsigned_abs();
                (self.unit.eq_ignore_ascii_case("month") && magnitude < 176_546)
                    || (self.unit.eq_ignore_ascii_case("year") && magnitude < 14_713)
            })
    }
}

fn split_numeric_datetime_modifier(modifier: &str) -> Option<NumericDateTimeModifier<'_>> {
    let (amount, unit) = modifier.split_at(modifier.find(is_date_time_space)?);
    if !matches!(
        amount.as_bytes().first().copied()?,
        b'+' | b'-' | b'0'..=b'9'
    ) {
        return None;
    }
    let unit = unit.trim_start_matches(is_date_time_space);
    let unit = unit.strip_suffix(['s', 'S']).unwrap_or(unit);
    let elapsed = match unit {
        unit if unit.eq_ignore_ascii_case("second") => Some((1.0, 4.6427e14)),
        unit if unit.eq_ignore_ascii_case("minute") => Some((60.0, 7.7379e12)),
        unit if unit.eq_ignore_ascii_case("hour") => Some((3_600.0, 1.2897e11)),
        unit if unit.eq_ignore_ascii_case("day") => Some((86_400.0, 5_373_485.0)),
        _ => None,
    };
    Some(NumericDateTimeModifier {
        amount,
        unit,
        elapsed,
    })
}

#[derive(Clone, Copy)]
struct PackedCalendarShift {
    direction: i64,
    months: i64,
    days: i64,
}

fn parse_packed_calendar_shift(text: &str) -> Option<PackedCalendarShift> {
    let (sign, text) = text.split_at_checked(1)?;
    let direction = match sign {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let (year_text, tail) = text.split_once('-')?;
    let (month_text, day_text) = tail.split_once('-')?;
    if !matches!(year_text.len(), 4 | 5) || !year_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let years = year_text.parse::<i64>().ok()?;
    let months = i64::from(parse_two_time_digits(month_text)?);
    let days = i64::from(parse_two_time_digits(day_text)?);
    if years > 14_712 || (year_text.len() == 5 && years < 10_000) || months > 11 || days > 30 {
        return None;
    }
    Some(PackedCalendarShift {
        direction,
        months: years * 12 + months,
        days,
    })
}

fn parse_packed_hms_modifier(text: &str) -> Option<(PackedCalendarShift, i64)> {
    let (date, clock) = text.split_once(' ')?;
    if !matches!(
        clock.as_bytes(),
        [_, _, b':', _, _, b':', _, _, b'.', _, _, _]
    ) {
        return None;
    }
    let shift = parse_packed_calendar_shift(date)?;
    let (clock_ms, _) = parse_hms_ms(clock, false)?;
    Some((shift, clock_ms))
}

fn has_raw_calendar_overflow(text: &str) -> bool {
    let Some(date) = text.get(..10) else {
        return false;
    };
    // A positive date can retain a day past the end of the month in SQLite.
    if !matches!(date.as_bytes(), [_, _, _, _, b'-', _, _, b'-', _, _]) {
        return false;
    }
    let Some(raw_day) = date.get(8..).and_then(parse_two_time_digits) else {
        return false;
    };
    parse_iso_datetime(date, false)
        .is_some_and(|local| utc_ymdhms_from_unix_ms(local.unix_ms).2 != raw_day)
}

fn is_normal_iso_calendar(stamp: &str) -> bool {
    let Some((date, clock)) = stamp.split_at_checked(10) else {
        return false;
    };
    let canonical_clock = match clock.as_bytes() {
        [] => true,
        [b' ' | b'T', _, _, b':', ..] => clock
            .get(1..3)
            .and_then(parse_two_time_digits)
            .is_some_and(|hour| hour < 24),
        _ => false,
    };
    matches!(
        date.as_bytes(),
        [b'0'..=b'9', _, _, _, b'-', _, _, b'-', _, _]
    ) && canonical_clock
        && !has_raw_calendar_overflow(stamp)
}

fn end_of_iso_period_ms(unix_ms: i64, stamp: &str, modifier: &str) -> Option<i64> {
    if !is_normal_iso_calendar(stamp) || !valid_julian_unix_ms(unix_ms) {
        return None;
    }

    let (year, month, day, _, _, _) = utc_ymdhms_from_unix_ms(unix_ms);
    let year = i64::from(year);
    let next_day = match modifier.to_ascii_lowercase().as_str() {
        "end of day" => days_from_civil(year, month, day) + 1,
        // SQLite computes the following month first, including its civil year check.
        "end of month" if (year, month) != (9999, 12) => {
            days_from_civil(year + i64::from(month == 12), month % 12 + 1, 1)
        }
        "end of year" => days_from_civil(year, 12, 31) + 1,
        _ => return None,
    };
    Some(next_day * 86_400_000 - 1)
}

fn shift_weekday_ms(unix_ms: i64, target_day: i64, backward: bool) -> i64 {
    let day = unix_ms.div_euclid(86_400_000);
    let current_day = (day + 4).rem_euclid(7);
    let mut days = (target_day - current_day).rem_euclid(7);
    if backward && days != 0 {
        days -= 7;
    }
    unix_ms.saturating_add(days * 86_400_000)
}

fn apply_packed_calendar_shift(
    unix_ms: i64,
    original_date: &str,
    modifier: &str,
    floor: bool,
) -> Option<i64> {
    if modifier.len() != 11 {
        return None;
    }
    let shift = parse_packed_calendar_shift(modifier)?;
    if utc_ymdhms_from_unix_ms(unix_ms).2 != parse_two_time_digits(original_date.get(8..)?)? {
        return None;
    }
    apply_calendar_shift(
        unix_ms,
        shift.direction * shift.months,
        shift.direction * shift.days,
        floor,
    )
}

fn apply_calendar_shift(unix_ms: i64, months: i64, days: i64, floor: bool) -> Option<i64> {
    let (year, month, day, _, _, _) = utc_ymdhms_from_unix_ms(unix_ms);
    let shifted_month = i64::from(year) * 12 + i64::from(month) - 1 + months;
    let year = shifted_month.div_euclid(12);
    if !(-4713..=9999).contains(&year) {
        return None;
    }
    let month = shifted_month.rem_euclid(12) as u32 + 1;
    // Normalize the original day in the target month before applying the day shift.
    let date_days = days_from_civil(year, month, day);
    let overflow_days = if floor {
        let next_month_start = days_from_civil(year + i64::from(month == 12), month % 12 + 1, 1);
        (date_days - next_month_start + 1).max(0)
    } else {
        0
    };
    let shifted_days = date_days + days - overflow_days;
    Some(shifted_days * 86_400_000 + unix_ms.rem_euclid(86_400_000))
}

fn apply_datetime_modifier_ms<'a>(
    time: &mut ParsedDateTime,
    modifier: &str,
    single_typed_modifier_days: Option<f64>,
    typed_elapsed_chain: bool,
    numeric_modifier: impl FnOnce() -> Option<NumericDateTimeModifier<'a>>,
) -> Option<i64> {
    let mut ms = time.unix_ms;
    let bare_hour24 = time.bare_hour24.take();
    let signed_hour24 = time.signed_hour24;
    if signed_hour24 && !modifier.is_ascii() {
        return None;
    }
    let calendar_in_range = !signed_hour24 || valid_julian_unix_ms(ms);
    let calendar_input = (calendar_in_range || time.calendar_retained).then_some(());
    let calendar_output =
        |result| (!signed_hour24 || valid_julian_unix_ms(result)).then_some(result);
    let lower = modifier.to_ascii_lowercase();
    match lower.as_str() {
        "start of day" | "start of month" | "start of year" if bare_hour24.is_some() => {
            return Some(days_from_civil(2000, 1, 1) * 86_400_000);
        }
        "start of day" => {
            calendar_input?;
            let (y, mo, d, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            time.calendar_retained = signed_hour24;
            return Some(days_from_civil(i64::from(y), mo, d) * 86_400_000);
        }
        "start of month" => {
            calendar_input?;
            let (y, mo, _, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            time.calendar_retained = signed_hour24;
            return Some(days_from_civil(i64::from(y), mo, 1) * 86_400_000);
        }
        "start of year" => {
            calendar_input?;
            let (y, _, _, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            time.calendar_retained = signed_hour24;
            return Some(days_from_civil(i64::from(y), 1, 1) * 86_400_000);
        }
        _ => {}
    }
    if let Some(rest) = lower.strip_prefix("weekday ") {
        calendar_input?;
        let wf: f64 = if calendar_in_range {
            rest.trim_matches(is_date_time_space).parse().ok()?
        } else {
            if rest.contains('-') {
                return None;
            }
            let (value, status) = sqliters_types::sqlite_atof(rest.as_bytes());
            (status > 0).then_some(value)?
        };
        if !wf.is_finite() || wf.fract() != 0.0 || !(0.0..7.0).contains(&wf) {
            return None;
        }
        if let Some(clock) = bare_hour24 {
            // C's weekday calculation discards the offset of a bare clock.
            ms += clock.offset_ms;
        }
        let target_dow = wf as i64;
        time.calendar_retained = false;
        if !calendar_in_range {
            // SQLite uses signed division and remainder for a retained date
            // below the Julian origin.
            let dow = (ms + UNIX_EPOCH_JULIAN_MS + 129_600_000) / 86_400_000 % 7;
            let days = if dow > target_dow {
                target_dow - dow + 7
            } else {
                target_dow - dow
            };
            return Some(ms + days * 86_400_000);
        }
        return calendar_output(shift_weekday_ms(ms, target_dow, false));
    }

    let NumericDateTimeModifier {
        amount,
        unit,
        elapsed,
    } = numeric_modifier()?;
    let elapsed_julian_days = single_typed_modifier_days.filter(|_| elapsed.is_some());
    let use_c_number = signed_hour24 || typed_elapsed_chain || elapsed_julian_days.is_some();
    let num: f64 = if use_c_number {
        let (value, status) = sqliters_types::sqlite_atof(amount.as_bytes());
        (status > 0).then_some(value)?
    } else {
        amount.parse().ok()?
    };
    if !num.is_finite() {
        return None;
    }
    if bare_hour24.is_some_and(|clock| clock.offset_ms != 0) {
        time.is_utc = true;
    }
    time.calendar_retained = false;
    if let Some((seconds, limit)) = elapsed {
        if use_c_number {
            // SQLite's duration table uses an exclusive binary32 limit and casts
            // after adding the sign-dependent half millisecond.
            if num.abs() >= f64::from(limit) {
                return None;
            }
            let rounder = if num < 0.0 { -0.5 } else { 0.5 };
            let delta = (num * 1_000.0 * seconds + rounder) as i64;
            if let Some(days) = elapsed_julian_days {
                // Only output precision modifiers can follow this duration.
                let result = julian_days_to_ms(days)?.checked_add(delta)?;
                return valid_julian_unix_ms(result).then_some(result);
            }
            return ms.checked_add(delta);
        }
        return Some(ms.saturating_add((num * (seconds * 1_000.0)).round() as i64));
    }
    match unit {
        unit if unit.eq_ignore_ascii_case("month") => {
            calendar_input?;
            if !calendar_in_range && (num.fract() != 0.0 || num.abs() >= 176_546.0) {
                return None;
            }
            let whole_months = num.trunc() as i64;
            let frac_months = num - (whole_months as f64);
            let (y, mo, d, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            let tod_ms = ms.rem_euclid(86_400_000);
            let total_m0 = i64::from(y) * 12 + (i64::from(mo) - 1) + whole_months;
            let new_y = total_m0.div_euclid(12);
            let new_m = (total_m0.rem_euclid(12) + 1) as u32;
            let base_days = days_from_civil(new_y, new_m, 1) + i64::from(d) - 1;
            let frac_ms = (frac_months * 30.0 * 86_400_000.0).round() as i64;
            calendar_output(
                base_days
                    .saturating_mul(86_400_000)
                    .saturating_add(tod_ms)
                    .saturating_add(frac_ms),
            )
        }
        unit if unit.eq_ignore_ascii_case("year") => {
            calendar_input?;
            if !calendar_in_range && (num.fract() != 0.0 || num.abs() >= 14_713.0) {
                return None;
            }
            let whole_years = num.trunc() as i64;
            let frac_years = num - (whole_years as f64);
            let (y, mo, d, _, _, _) = utc_ymdhms_from_unix_ms(ms);
            let tod_ms = ms.rem_euclid(86_400_000);
            let new_y = i64::from(y) + whole_years;
            let base_days = days_from_civil(new_y, mo, 1) + i64::from(d) - 1;
            let frac_ms = (frac_years * 365.0 * 86_400_000.0).round() as i64;
            calendar_output(
                base_days
                    .saturating_mul(86_400_000)
                    .saturating_add(tod_ms)
                    .saturating_add(frac_ms),
            )
        }
        _ => None,
    }
}

fn valid_julian_day(unix_ms: i64) -> Option<f64> {
    unix_ms
        .checked_add(UNIX_EPOCH_JULIAN_MS)
        .filter(|ms| (0..JULIAN_MS_LIMIT).contains(ms))
        .map(|julian_ms| julian_ms as f64 / 86_400_000.0)
}

fn eval_datetime_function(
    upper_name: &str,
    args: &[Value],
    ctx: &VmRuntimeContext,
) -> Result<Value> {
    let (unix_ms, use_subsec) = if args.is_empty() {
        (ctx.effective_timestamp_ms(), false)
    } else {
        let Some((time, use_subsec)) = parse_datetime_value(&args[0], &args[1..], ctx) else {
            return Ok(Value::Null);
        };
        (time.unix_ms, use_subsec)
    };

    match upper_name {
        "DATE" => Ok(Value::Text(format_utc_date(unix_ms))),
        "TIME" | "DATETIME" => {
            let mut text = if upper_name == "TIME" {
                format_utc_time(unix_ms)
            } else {
                format_utc_timestamp(unix_ms)
            };
            if use_subsec {
                text.push_str(&format!(".{:03}", unix_ms.rem_euclid(1_000)));
            }
            Ok(Value::Text(text))
        }
        "UNIXEPOCH" if use_subsec => Ok(Value::real(unix_ms as f64 / 1_000.0)),
        "UNIXEPOCH" => Ok(Value::Integer(unix_ms.div_euclid(1_000))),
        "JULIANDAY" => {
            let day = valid_julian_day(unix_ms)
                .unwrap_or_else(|| (unix_ms as f64) / 86_400_000.0 + 2_440_587.5);
            Ok(Value::real(day))
        }
        _ => unreachable!(),
    }
}

fn eval_timediff(first: &Value, second: &Value, ctx: &VmRuntimeContext) -> Value {
    const DAY_MS: i64 = 86_400_000;

    let Some((first, _)) = parse_datetime_value(first, &[], ctx) else {
        return Value::Null;
    };
    let Some((second, _)) = parse_datetime_value(second, &[], ctx) else {
        return Value::Null;
    };

    let (first_year, first_month, _, _, _, _) = utc_ymdhms_from_unix_ms(first.unix_ms);
    let (second_year, second_month, second_day, _, _, _) = utc_ymdhms_from_unix_ms(second.unix_ms);
    let (sign, direction) = if first.unix_ms >= second.unix_ms {
        ('+', 1)
    } else {
        ('-', -1)
    };
    let month_number = |year, month| i64::from(year) * 12 + i64::from(month) - 1;
    let mut anchor_month = month_number(first_year, first_month);
    let mut months = (anchor_month - month_number(second_year, second_month)) * direction;
    let second_clock = second.unix_ms.rem_euclid(DAY_MS);
    let anchor_ms = |month: i64| {
        let year = month.div_euclid(12);
        let month = month.rem_euclid(12) as u32 + 1;
        days_from_civil(year, month, second_day) * DAY_MS + second_clock
    };

    // Keep the second date's day and clock when testing each calendar month.
    let mut remainder = (first.unix_ms - anchor_ms(anchor_month)) * direction;
    while remainder < 0 {
        months -= 1;
        anchor_month -= direction;
        remainder = (first.unix_ms - anchor_ms(anchor_month)) * direction;
    }

    let years = months / 12;
    let months = months % 12;
    // SQLite formats the remainder relative to January 1 of year zero.
    let remainder_date = days_from_civil(0, 1, 1) * DAY_MS + remainder;
    let (_, _, day, hour, minute, second) = utc_ymdhms_from_unix_ms(remainder_date);
    let days = day - 1;
    let milliseconds = remainder % 1_000;
    Value::Text(format!(
        "{sign}{years:04}-{months:02}-{days:02} {hour:02}:{minute:02}:{second:02}.{milliseconds:03}"
    ))
}

fn eval_strftime_function(args: &[Value], ctx: &VmRuntimeContext) -> Result<Value> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    if args[0].is_null() {
        return Ok(Value::Null);
    }
    let fmt = args[0].to_text_lossy();
    let (time, use_subsec) = if args.len() == 1 {
        (
            ParsedDateTime::from_unix_ms(ctx.effective_timestamp_ms()),
            false,
        )
    } else {
        let Some(parsed) = parse_datetime_value(&args[1], &args[2..], ctx) else {
            return Ok(Value::Null);
        };
        parsed
    };
    let unix_ms = time.unix_ms;
    let (y, m, d, hh, mm, ss) = utc_ymdhms_from_unix_ms(unix_ms);
    let ms_rem = unix_ms.rem_euclid(1_000) as u32;
    let total_secs = unix_ms.div_euclid(1_000);
    let days = total_secs.div_euclid(86_400);
    let dow = (days + 4).rem_euclid(7); // 1970-01-01 was Thursday (4); Sunday = 0
    let jan1_days = days_from_civil(i64::from(y), 1, 1);
    let doy_1_based = (days - jan1_days) + 1;

    let is_fixed_text_format = || {
        matches!(args, [Value::Text(_), _])
            && !fmt.contains('\0')
            && !matches!(&args[1], Value::Text(text) if text.trim().eq_ignore_ascii_case("now"))
    };
    let mut out = String::with_capacity(fmt.len() + 16);
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(spec) = chars.next() else {
            return Ok(Value::Null);
        };
        match spec {
            '%' => out.push('%'),
            'Y' => out.push_str(&format!("{y:04}")),
            'm' => out.push_str(&format!("{m:02}")),
            'd' => out.push_str(&format!("{d:02}")),
            'e' => out.push_str(&format!("{d:2}")),
            'H' => out.push_str(&format!("{hh:02}")),
            'k' => out.push_str(&format!("{hh:2}")),
            'I' | 'l' if spec == 'I' || is_fixed_text_format() => {
                let h12 = match hh % 12 {
                    0 => 12,
                    v => v,
                };
                if spec == 'I' {
                    out.push_str(&format!("{h12:02}"));
                } else {
                    out.push_str(&format!("{h12:2}"));
                }
            }
            'M' => out.push_str(&format!("{mm:02}")),
            'S' => out.push_str(&format!("{ss:02}")),
            'f' => {
                if let Some(seconds) = time.raw_seconds {
                    let bytes = crate::printf::sqlite_printf(b"%06.3f", &[Value::real(seconds)])
                        .map_err(|_| {
                            SqliteError::new(ErrorCode::TooBig, "string or blob too big")
                        })?;
                    out.push_str(&String::from_utf8_lossy(&bytes));
                } else {
                    out.push_str(&format!("{ss:02}.{ms_rem:03}"));
                }
            }
            'F' => out.push_str(&format!("{y:04}-{m:02}-{d:02}")),
            'T' => out.push_str(&format!("{hh:02}:{mm:02}:{ss:02}")),
            'R' => out.push_str(&format!("{hh:02}:{mm:02}")),
            'j' => out.push_str(&format!("{doy_1_based:03}")),
            'J' => {
                if let Some(day) = valid_julian_day(unix_ms) {
                    let bytes = crate::printf::sqlite_printf(b"%.16g", &[Value::real(day)])
                        .map_err(|_| {
                            SqliteError::new(ErrorCode::TooBig, "string or blob too big")
                        })?;
                    out.push_str(&String::from_utf8_lossy(&bytes));
                } else {
                    let jd = (unix_ms as f64) / 86_400_000.0 + 2_440_587.5;
                    out.push_str(&format!("{jd:.9}"));
                }
            }
            's' if use_subsec => {
                let sign = if unix_ms < 0 { "-" } else { "" };
                let magnitude = unix_ms.unsigned_abs();
                let seconds = magnitude / 1_000;
                let fraction = magnitude % 1_000;
                out.push_str(&format!("{sign}{seconds}.{fraction:03}"));
            }
            's' => out.push_str(&format!("{total_secs}")),
            'w' => out.push_str(&format!("{dow}")),
            'u' => out.push_str(&format!("{}", if dow == 0 { 7 } else { dow })),
            'W' | 'U' if is_fixed_text_format() => {
                let weekday_index = if spec == 'W' { (dow + 6) % 7 } else { dow };
                let week = (doy_1_based + 6 - weekday_index) / 7;
                out.push_str(&format!("{week:02}"));
            }
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
    ContainsAsciiBmh {
        needle: Box<[u8]>,
        skip: Box<[usize; 256]>,
    },
    General(Box<[LikeToken]>),
}

/// Pre-compiled SQLite `LIKE` pattern with zero-allocation byte-slice matching and a pure
/// safe-Rust Boyer-Moore-Horspool `[usize; 256]` skip table for `%<ascii_literal>%`.
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
            let mut skip = Box::new([m; 256]);
            for i in 0..(m - 1) {
                let b = needle[i];
                let shift = m - 1 - i;
                skip[b as usize] = shift;
                if !case_sensitive && b.is_ascii_lowercase() {
                    skip[b.to_ascii_uppercase() as usize] = shift;
                }
            }
            return Self {
                kind: CompiledLikeKind::ContainsAsciiBmh { needle, skip },
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
        if hay.len() != lower_needle.len() {
            return false;
        }
        if case_sensitive {
            hay == lower_needle
        } else {
            for i in 0..lower_needle.len() {
                if hay[i].to_ascii_lowercase() != lower_needle[i] {
                    return false;
                }
            }
            true
        }
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
                if cs || !t.is_ascii_lowercase() {
                    haystack.contains(&t)
                } else {
                    let upper = t.to_ascii_uppercase();
                    for &b in haystack {
                        if b == t || b == upper {
                            return true;
                        }
                    }
                    false
                }
            }
            CompiledLikeKind::ContainsAsciiBmh { needle, skip } => {
                let m = needle.len();
                let n = haystack.len();
                if n < m {
                    return false;
                }
                let last_idx = m - 1;
                let last_b = needle[last_idx];
                let limit = n - m;
                let mut pos = 0usize;
                if !cs {
                    while pos <= limit {
                        let win = &haystack[pos..pos + m];
                        let hb = win[last_idx];
                        if hb.to_ascii_lowercase() == last_b {
                            let mut ok = true;
                            for k in 0..last_idx {
                                if win[k].to_ascii_lowercase() != needle[k] {
                                    ok = false;
                                    break;
                                }
                            }
                            if ok {
                                return true;
                            }
                        }
                        pos += skip[hb as usize];
                    }
                } else {
                    while pos <= limit {
                        let win = &haystack[pos..pos + m];
                        let hb = win[last_idx];
                        if hb == last_b && win[..last_idx] == needle[..last_idx] {
                            return true;
                        }
                        pos += skip[hb as usize];
                    }
                }
                false
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
    fn classify_numeric(v: &Value, encoding: TextEncoding) -> Option<SumNumeric> {
        match v {
            Value::Null => None,
            Value::Integer(i) => Some(SumNumeric::Int(*i)),
            Value::Real(r) => Some(SumNumeric::Float(*r)),
            Value::Text(s) => match sqliters_types::parse_numeric_text(s) {
                Some(Value::Integer(i)) => Some(SumNumeric::Int(i)),
                Some(Value::Real(r)) => Some(SumNumeric::Float(r)),
                _ => Some(SumNumeric::Float(v.to_real_lossy())),
            },
            // sqlite3_value_numeric_type leaves BLOB intact. sumStep calls
            // sqlite3_value_double with this cell's encoding in its REAL arm.
            Value::Blob(bytes) => Some(SumNumeric::Float(
                sqliters_types::fp::sqlite_atof_with_encoding(bytes, encoding).0,
            )),
        }
    }

    fn step_val(&mut self, v: &Value, encoding: TextEncoding) {
        let Some(num) = Self::classify_numeric(v, encoding) else {
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

    fn inverse_val(&mut self, v: &Value, encoding: TextEncoding) {
        let Some(num) = Self::classify_numeric(v, encoding) else {
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
    encoding: TextEncoding,
}

impl FastSumAccum {
    #[inline]
    pub const fn new() -> Self {
        Self {
            count: 0,
            encoding: TextEncoding::Utf8,
            ctx: SumCtx {
                r_sum: 0.0,
                r_err: 0.0,
                i_sum: 0,
                approx: false,
                ovrfl: false,
            },
        }
    }

    #[inline]
    pub fn with_encoding(mut self, encoding: TextEncoding) -> Self {
        self.encoding = encoding;
        self
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
        self.ctx.step_val(v, self.encoding);
    }

    #[inline]
    pub fn inverse_value(&mut self, v: &Value) {
        if v.is_null() {
            return;
        }
        self.count -= 1;
        self.ctx.inverse_val(v, self.encoding);
    }

    #[inline]
    pub fn step_raw_field(&mut self, field: sqliters_types::RawField<'_>, enc: TextEncoding) {
        match field {
            sqliters_types::RawField::Null => {}
            sqliters_types::RawField::Int(i) => self.step_i64(i),
            sqliters_types::RawField::Float(f) => self.step_f64(f),
            sqliters_types::RawField::Blob(bytes) => {
                self.step_f64(sqliters_types::fp::sqlite_atof_with_encoding(bytes, enc).0);
            }
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
                Err(SqliteError::new(ErrorCode::Error, "integer overflow"))
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

/// Key under which `agg(DISTINCT x)` remembers a value with the BINARY collation. C keeps the
/// values in an index, where an INTEGER and a REAL of the same numeric value are one key
/// (`sqlite3VdbeRecordCompare`), so `1` and `1.0` are one distinct value.
fn distinct_binary_key(v: &Value) -> Vec<u8> {
    if let Value::Real(r) = v
        && *r >= -9_223_372_036_854_775_808.0
        && *r < 9_223_372_036_854_775_808.0
    {
        let i = *r as i64;
        if (i as f64) == *r && sqliters_types::cmp_int_float(i, *r).is_eq() {
            return sqliters_types::encode_record(&[Value::Integer(i)]);
        }
    }
    sqliters_types::encode_record(std::slice::from_ref(v))
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
        let enc = self.encoding;
        self.step_rt_with_cmp(args, |a, b| Ok(a.cmp_sqlite_with_encoding(b, coll, enc)))
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
                    let key = distinct_binary_key(first);
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
        let enc = self.encoding;
        self.step_with_cmp(args, |a, b| Ok(a.cmp_sqlite_with_encoding(b, coll, enc)))
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
                let key = distinct_binary_key(first);
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
                self.sum_ctx.step_val(first, self.encoding);
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
                    self.sum_ctx.inverse_val(first, self.encoding);
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
                        Err(SqliteError::new(ErrorCode::Error, "integer overflow"))
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
