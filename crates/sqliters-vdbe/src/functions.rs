//! Built-in SQLite scalar functions, `LIKE` / `GLOB` pattern matcher, and aggregate accumulators
//! ported from `sqlite/src/func.c`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::time::{SystemTime, UNIX_EPOCH};

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

use sqliters_types::{
    Collation, ErrorCode, Result, SQLITE_SOURCE_ID, SQLITE_VERSION, SqliteError, Value,
    format_sqlite_real,
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

/// Evaluates a built-in SQLite scalar function (`func.c`).
pub fn eval_scalar_function(
    name: &str,
    args: &[Value],
    ctx: &VmRuntimeContext,
) -> Result<Value> {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "TYPEOF" => {
            check_arity(name, args, 1, 1)?;
            Ok(Value::Text(args[0].storage_class().typeof_str().to_owned()))
        }
        "ABS" => {
            check_arity(name, args, 1, 1)?;
            match &args[0] {
                Value::Null => Ok(Value::Null),
                Value::Integer(i) => i.checked_abs().map(Value::Integer).ok_or_else(|| {
                    SqliteError::new(ErrorCode::Mismatch, "integer overflow")
                }),
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
            let min = if upper == "IFNULL" { 2 } else { 1 };
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
        "IIF" => {
            check_arity(name, args, 3, 3)?;
            if args[0].truthiness() == Some(true) {
                Ok(args[1].clone())
            } else {
                Ok(args[2].clone())
            }
        }
        "LENGTH" => {
            check_arity(name, args, 1, 1)?;
            match &args[0] {
                Value::Null => Ok(Value::Null),
                Value::Blob(b) => Ok(Value::Integer(b.len() as i64)),
                other => Ok(Value::Integer(other.to_text_lossy().chars().count() as i64)),
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
            let trimmed = match upper.as_str() {
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
                ((raw_start - 1) as i64, raw_len)
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
            let bytes: Vec<u8> = match &args[0] {
                Value::Blob(b) => b.clone(),
                other => other.to_text_lossy().into_bytes(),
            };
            let mut out = String::with_capacity(bytes.len() * 2);
            for b in bytes {
                out.push_str(&format!("{b:02X}"));
            }
            Ok(Value::Text(out))
        }
        "UNHEX" => {
            check_arity(name, args, 1, 1)?;
            if args[0].is_null() {
                return Ok(Value::Null);
            }
            let s = args[0].to_text_lossy();
            if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(Value::Null);
            }
            let mut out = Vec::with_capacity(s.len() / 2);
            for pair in s.as_bytes().chunks_exact(2) {
                let byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("00"), 16)
                    .unwrap_or(0);
                out.push(byte);
            }
            Ok(Value::Blob(out))
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
                Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
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
            let n = args[0].to_integer_lossy().max(0) as usize;
            Ok(Value::Blob(vec![0u8; n]))
        }
        "RANDOM" => {
            check_arity(name, args, 0, 0)?;
            Ok(Value::Integer(next_prng_u64() as i64))
        }
        "RANDOMBLOB" => {
            check_arity(name, args, 1, 1)?;
            let n = args[0].to_integer_lossy().max(1) as usize;
            let mut buf = vec![0u8; n];
            for chunk in buf.chunks_mut(8) {
                let bytes = next_prng_u64().to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
            Ok(Value::Blob(buf))
        }
        "ROUND" => {
            check_arity(name, args, 1, 2)?;
            if args[0].is_null() || (args.len() == 2 && args[1].is_null()) {
                return Ok(Value::Null);
            }
            let x = args[0].to_real_lossy();
            let digits = args.get(1).map_or(0, Value::to_integer_lossy).clamp(-30, 30);
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
            let matched = sqlite_like_match(&args[0].to_text_lossy(), &args[1].to_text_lossy(), esc);
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
            eval_datetime_function(&upper, args, ctx)
        }
        "STRFTIME" => eval_strftime_function(args, ctx),
        _ => Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such function: {name}"),
        )),
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

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + i64::from(doe) - 719_468
}

fn parse_time_value_to_unix_ms(val: &Value, modifiers: &[Value], ctx: &VmRuntimeContext) -> Option<i64> {
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
        if mt.eq_ignore_ascii_case("unixepoch") || mt.eq_ignore_ascii_case("utc") {
            continue;
        }
        if let Some(delta_ms) = parse_relative_modifier_ms(mt) {
            ms = ms.saturating_add(delta_ms);
        } else {
            return None;
        }
    }
    Some(ms)
}

fn parse_iso_datetime_ms(s: &str) -> Option<i64> {
    // Supports "YYYY-MM-DD", "YYYY-MM-DD HH:MM[:SS]", "YYYY-MM-DDTHH:MM[:SS][Z]", or "HH:MM[:SS]"
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix('z')).unwrap_or(s);
    if s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let y: i64 = s[0..4].parse().ok()?;
        let m: u32 = s[5..7].parse().ok()?;
        let d: u32 = s[8..10].parse().ok()?;
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let days = days_from_civil(y, m, d);
        let rest = s[10..].trim_start_matches([' ', 'T', 't']);
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
            let mut frac_digits = String::from(frac_part);
            frac_digits.truncate(3);
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

fn parse_relative_modifier_ms(s: &str) -> Option<i64> {
    let mut parts = s.split_whitespace();
    let num: f64 = parts.next()?.parse().ok()?;
    let unit = parts.next()?.to_ascii_lowercase();
    if parts.next().is_some() {
        return None;
    }
    let mult_ms = match unit.trim_end_matches('s') {
        "second" => 1_000.0,
        "minute" => 60_000.0,
        "hour" => 3_600_000.0,
        "day" => 86_400_000.0,
        _ => return None,
    };
    Some((num * mult_ms).round() as i64)
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

/// SQLite `LIKE` matcher (`%` = 0+ chars, `_` = 1 char, ASCII case-insensitive).
pub fn sqlite_like_match(pattern: &str, text: &str, escape: Option<char>) -> bool {
    let pchars: Vec<char> = pattern.chars().collect();
    let tchars: Vec<char> = text.chars().collect();
    like_rec(&pchars, &tchars, escape)
}

fn like_rec(pat: &[char], txt: &[char], escape: Option<char>) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    while pi < pat.len() {
        let pc = pat[pi];
        if Some(pc) == escape {
            pi += 1;
            if pi >= pat.len() || ti >= txt.len() || !ascii_eq_nocase(pat[pi], txt[ti]) {
                return false;
            }
            pi += 1;
            ti += 1;
        } else if pc == '%' {
            while pi < pat.len() && pat[pi] == '%' {
                pi += 1;
            }
            if pi == pat.len() {
                return true;
            }
            for k in ti..=txt.len() {
                if like_rec(&pat[pi..], &txt[k..], escape) {
                    return true;
                }
            }
            return false;
        } else if pc == '_' {
            if ti >= txt.len() {
                return false;
            }
            pi += 1;
            ti += 1;
        } else {
            if ti >= txt.len() || !ascii_eq_nocase(pc, txt[ti]) {
                return false;
            }
            pi += 1;
            ti += 1;
        }
    }
    ti == txt.len()
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
                let invert = pi < pat.len() && (pat[pi] == '^' || pat[pi] == '!');
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
        if i < bytes.len() && bytes[i] == b'0' {
            zero_pad = true;
            i += 1;
        }
        let mut width = 0usize;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            width = width * 10 + (bytes[i] - b'0') as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            let mut p = 0usize;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                p = p * 10 + (bytes[i] - b'0') as usize;
                i += 1;
            }
            prec = Some(p);
        }
        if i >= bytes.len() {
            break;
        }
        let spec = bytes[i] as char;
        i += 1;
        let val = args.get(arg_idx).cloned().unwrap_or(Value::Null);
        arg_idx += 1;

        let formatted = match spec {
            'd' | 'i' => format!("{}", val.to_integer_lossy()),
            'x' => format!("{:x}", val.to_integer_lossy()),
            'X' => format!("{:X}", val.to_integer_lossy()),
            'f' => {
                let p = prec.unwrap_or(6);
                format!("{:.*}", p, val.to_real_lossy())
            }
            's' => val.to_text_lossy(),
            _ => val.to_text_lossy(),
        };
        if formatted.len() < width {
            let pad = width - formatted.len();
            let pad_ch = if zero_pad { '0' } else { ' ' };
            for _ in 0..pad {
                out.push(pad_ch);
            }
        }
        out.push_str(&formatted);
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
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AggregateAccumulator {
    kind: AggregateKind,
    distinct: bool,
    seen: HashSet<Vec<u8>>,
    count: i64,
    int_sum: i128,
    float_sum: f64,
    has_float: bool,
    best: Option<Value>,
    concat_buf: Option<String>,
}

impl AggregateAccumulator {
    pub fn new(kind: AggregateKind, distinct: bool) -> Self {
        Self {
            kind,
            distinct,
            seen: HashSet::new(),
            count: 0,
            int_sum: 0,
            float_sum: 0.0,
            has_float: false,
            best: None,
            concat_buf: None,
        }
    }

    pub fn step(&mut self, args: &[Value]) -> Result<()> {
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
            let key = sqliters_types::encode_record(&[first.clone()]);
            if !self.seen.insert(key) {
                return Ok(());
            }
        }

        match self.kind {
            AggregateKind::CountStar => unreachable!(),
            AggregateKind::Count => {
                self.count += 1;
            }
            AggregateKind::Sum | AggregateKind::Total | AggregateKind::Avg => {
                self.count += 1;
                match first {
                    Value::Integer(i) => {
                        self.int_sum += i128::from(*i);
                    }
                    Value::Real(r) => {
                        self.has_float = true;
                        self.float_sum += *r;
                    }
                    other => {
                        if let Some(Value::Integer(i)) =
                            sqliters_types::parse_numeric_text(&other.to_text_lossy())
                        {
                            self.int_sum += i128::from(i);
                        } else {
                            self.has_float = true;
                            self.float_sum += other.to_real_lossy();
                        }
                    }
                }
            }
            AggregateKind::Min => {
                let replace = match &self.best {
                    None => true,
                    Some(cur) => first.cmp_sqlite(cur, Collation::Binary).is_lt(),
                };
                if replace {
                    self.best = Some(first.clone());
                }
            }
            AggregateKind::Max => {
                let replace = match &self.best {
                    None => true,
                    Some(cur) => first.cmp_sqlite(cur, Collation::Binary).is_gt(),
                };
                if replace {
                    self.best = Some(first.clone());
                }
            }
            AggregateKind::GroupConcat => {
                let sep = args
                    .get(1)
                    .filter(|v| !v.is_null())
                    .map_or_else(|| ",".to_owned(), Value::to_text_lossy);
                if args.get(1).is_some_and(Value::is_null) {
                    return Ok(());
                }
                let text = first.to_text_lossy();
                match &mut self.concat_buf {
                    None => self.concat_buf = Some(text),
                    Some(buf) => {
                        buf.push_str(&sep);
                        buf.push_str(&text);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn finalize(&self) -> Result<Value> {
        match self.kind {
            AggregateKind::CountStar | AggregateKind::Count => Ok(Value::Integer(self.count)),
            AggregateKind::Sum => {
                if self.count == 0 {
                    return Ok(Value::Null);
                }
                if self.has_float {
                    Ok(Value::real((self.int_sum as f64) + self.float_sum))
                } else {
                    i64::try_from(self.int_sum).map(Value::Integer).map_err(|_| {
                        SqliteError::new(ErrorCode::Mismatch, "integer overflow")
                    })
                }
            }
            AggregateKind::Total => {
                // In SQLite, TOTAL() of empty set returns 0.0 (never NULL!)
                Ok(Value::real((self.int_sum as f64) + self.float_sum))
            }
            AggregateKind::Avg => {
                if self.count == 0 {
                    Ok(Value::Null)
                } else {
                    let sum = (self.int_sum as f64) + self.float_sum;
                    Ok(Value::real(sum / (self.count as f64)))
                }
            }
            AggregateKind::Min | AggregateKind::Max => {
                Ok(self.best.clone().unwrap_or(Value::Null))
            }
            AggregateKind::GroupConcat => Ok(self
                .concat_buf
                .clone()
                .map_or(Value::Null, Value::Text)),
        }
    }
}
