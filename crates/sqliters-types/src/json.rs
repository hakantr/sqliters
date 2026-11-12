//! Pure safe-Rust SQLite 3.54.1 JSON / JSON5 / JSONB codec, path engine,
//! scalar evaluators, window-capable group accumulators, and tree walkers
//! (`sqlite/src/json.c`).
#![forbid(unsafe_code)]

use std::collections::VecDeque;

use crate::{ErrorCode, Result, RuntimeValue, SqliteError, Value, format_sqlite_real};

/// Ephemeral subtype `'J'` (`74`) attached to SQL `TEXT` values carrying JSON.
pub const JSON_SUBTYPE: u32 = 74;

/// Maximum allowed nesting depth for JSON text and JSONB containers (`json.c`).
pub const JSON_MAX_DEPTH: usize = 1000;

/// JSONB element type nibbles (`low 4 bits` of header byte 0).
pub const JSONB_NULL: u8 = 0;
pub const JSONB_TRUE: u8 = 1;
pub const JSONB_FALSE: u8 = 2;
pub const JSONB_INT: u8 = 3;
pub const JSONB_INT5: u8 = 4;
pub const JSONB_FLOAT: u8 = 5;
pub const JSONB_FLOAT5: u8 = 6;
pub const JSONB_TEXT: u8 = 7;
pub const JSONB_TEXTJ: u8 = 8;
pub const JSONB_TEXT5: u8 = 9;
pub const JSONB_TEXTRAW: u8 = 10;
pub const JSONB_ARRAY: u8 = 11;
pub const JSONB_OBJECT: u8 = 12;

/// `P3v2` Binary `JSONB` 4-byte envelope header (`[0x0f, 0x01, total_len_lo, total_len_hi]`)
/// and position-independent node tags (`0x00..=0x0e`).
pub const P3V2_JSONB_MAGIC0: u8 = 0x0f;
pub const P3V2_JSONB_VER1: u8 = 0x01;

pub const P3J_NULL: u8 = 0x00;
pub const P3J_FALSE: u8 = 0x01;
pub const P3J_TRUE: u8 = 0x02;
pub const P3J_INT_I8: u8 = 0x03;
pub const P3J_INT_I16: u8 = 0x04;
pub const P3J_INT_I32: u8 = 0x05;
pub const P3J_INT_I64: u8 = 0x06;
pub const P3J_FLOAT_RAW: u8 = 0x07;
pub const P3J_STR_PLAIN: u8 = 0x08;
pub const P3J_STR_ESCAPED: u8 = 0x09;
pub const P3J_STR_RAW: u8 = 0x0a;
pub const P3J_EMPTY_ARRAY: u8 = 0x0b;
pub const P3J_OBJECT: u8 = 0x0c;
pub const P3J_ARRAY: u8 = 0x0d;
pub const P3J_EMPTY_OBJECT: u8 = 0x0e;
/// Compact object (`1 <= n_keys <= 31`, all keys plain `0..=63 B`, `dir_rel <= 255`):
/// `[0x0f, n_keys, dir_rel_u8]` followed by `key/value` body and `2 * n_keys` directory bytes
/// `[kinfo_u8, val_rel_u8]`, where `klen = kinfo_u8 >> 2` and `key_pos = pos + val_rel - 1 - klen`.
pub const P3J_OBJECT_COMPACT: u8 = 0x0f;
/// Short plain UTF-8 string (`0..=127 B`): single header byte `0x80 | (len as u8)` followed by `len` bytes.
pub const P3J_STR_SHORT_BASE: u8 = 0x80;

const P3V2_KEY_TAG_ESCAPED: u32 = 0x80;

/// Returns `true` when `blob` carries the `P3v2` Binary `JSONB` envelope header.
#[inline(always)]
pub fn is_p3v2_jsonb(blob: &[u8]) -> bool {
    blob.len() >= 5
        && blob[0] == P3V2_JSONB_MAGIC0
        && blob[1] == P3V2_JSONB_VER1
        && usize::from(u16::from_le_bytes([blob[2], blob[3]])) == blob.len()
}

#[inline(always)]
fn p3v2_key_tag(bytes: &[u8], raw_key: bool) -> u32 {
    if raw_key && bytes.len() <= 127 {
        u32::from_le_bytes([
            bytes.len() as u8,
            bytes.first().copied().unwrap_or(0),
            bytes.get(1).copied().unwrap_or(0),
            bytes.get(2).copied().unwrap_or(0),
        ])
    } else {
        P3V2_KEY_TAG_ESCAPED
    }
}

#[inline(always)]
fn p3v2_compact_kinfo(bytes: &[u8], raw_key: bool) -> Option<u8> {
    if raw_key && bytes.len() <= 63 {
        Some(((bytes.len() as u8) << 2) | (bytes.first().copied().unwrap_or(0) & 0x03))
    } else {
        None
    }
}

/// Decodes a `P3v2` string header at `blob[pos]` into `(kind_tag, hdr_len, str_len)` where
/// `kind_tag` is normalized to `P3J_STR_PLAIN`, `P3J_STR_ESCAPED`, or `P3J_STR_RAW`.
#[inline(always)]
fn p3v2_str_header(blob: &[u8], pos: usize) -> Option<(u8, usize, usize)> {
    let tag = *blob.get(pos)?;
    if tag >= P3J_STR_SHORT_BASE {
        let slen = usize::from(tag & 0x7f);
        (pos.checked_add(1)?.checked_add(slen)? <= blob.len()).then_some((P3J_STR_PLAIN, 1, slen))
    } else if matches!(tag, P3J_STR_PLAIN | P3J_STR_ESCAPED | P3J_STR_RAW) {
        let slen = usize::from(u16::from_le_bytes([
            *blob.get(pos.checked_add(1)?)?,
            *blob.get(pos.checked_add(2)?)?,
        ]));
        (pos.checked_add(3)?.checked_add(slen)? <= blob.len()).then_some((tag, 3, slen))
    } else {
        None
    }
}

#[inline(always)]
fn p3v2_node_len(blob: &[u8], pos: usize) -> Option<usize> {
    let tag = *blob.get(pos)?;
    if tag >= P3J_STR_SHORT_BASE {
        let len = 1usize + usize::from(tag & 0x7f);
        return (pos.checked_add(len)? <= blob.len()).then_some(len);
    }
    let len = match tag {
        P3J_NULL | P3J_FALSE | P3J_TRUE | P3J_EMPTY_ARRAY | P3J_EMPTY_OBJECT => 1usize,
        P3J_INT_I8 => 2usize,
        P3J_INT_I16 => 3usize,
        P3J_INT_I32 => 5usize,
        P3J_INT_I64 => 9usize,
        P3J_FLOAT_RAW => {
            let b9 = *blob.get(pos.checked_add(9)?)?;
            let b10 = *blob.get(pos.checked_add(10)?)?;
            11usize.checked_add(usize::from(u16::from_le_bytes([b9, b10])))?
        }
        P3J_STR_PLAIN | P3J_STR_ESCAPED | P3J_STR_RAW => {
            let b1 = *blob.get(pos.checked_add(1)?)?;
            let b2 = *blob.get(pos.checked_add(2)?)?;
            3usize.checked_add(usize::from(u16::from_le_bytes([b1, b2])))?
        }
        P3J_OBJECT_COMPACT => {
            let n_keys = usize::from(*blob.get(pos.checked_add(1)?)?);
            let dir_rel = usize::from(*blob.get(pos.checked_add(2)?)?);
            if n_keys == 0 || n_keys > 31 || dir_rel <= 4 {
                return None;
            }
            dir_rel + 2 * n_keys
        }
        P3J_OBJECT => {
            let n_keys = usize::from(*blob.get(pos.checked_add(1)?)?);
            if n_keys == 0 {
                return None;
            }
            let dir_rel = usize::from(u16::from_le_bytes([
                *blob.get(pos + 2)?,
                *blob.get(pos + 3)?,
            ]));
            let end_rel = usize::from(u16::from_le_bytes([
                *blob.get(pos + 4)?,
                *blob.get(pos + 5)?,
            ]));
            if dir_rel <= 6 || end_rel != dir_rel + 8 * n_keys {
                return None;
            }
            end_rel
        }
        P3J_ARRAY => {
            let n_elems = usize::from(u16::from_le_bytes([
                *blob.get(pos + 1)?,
                *blob.get(pos + 2)?,
            ]));
            if n_elems == 0 {
                return None;
            }
            let dir_rel = usize::from(u16::from_le_bytes([
                *blob.get(pos + 3)?,
                *blob.get(pos + 4)?,
            ]));
            let end_rel = usize::from(u16::from_le_bytes([
                *blob.get(pos + 5)?,
                *blob.get(pos + 6)?,
            ]));
            if dir_rel <= 7 || end_rel != dir_rel + 2 * n_elems {
                return None;
            }
            end_rel
        }
        _ => return None,
    };
    (pos.checked_add(len)? <= blob.len()).then_some(len)
}

#[inline(always)]
fn p3v2_read_int(blob: &[u8], pos: usize) -> Option<i64> {
    match *blob.get(pos)? {
        P3J_INT_I8 => Some(i64::from(*blob.get(pos + 1)? as i8)),
        P3J_INT_I16 => {
            let s = blob.get(pos + 1..pos + 3)?;
            Some(i64::from(i16::from_le_bytes([s[0], s[1]])))
        }
        P3J_INT_I32 => {
            let s = blob.get(pos + 1..pos + 5)?;
            Some(i64::from(i32::from_le_bytes([s[0], s[1], s[2], s[3]])))
        }
        P3J_INT_I64 => {
            let s = blob.get(pos + 1..pos + 9)?;
            Some(i64::from_le_bytes([
                s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
            ]))
        }
        _ => None,
    }
}

#[inline]
fn p3v2_append_int_node(out: &mut Vec<u8>, v: i64) {
    if let Ok(v8) = i8::try_from(v) {
        out.extend_from_slice(&[P3J_INT_I8, v8 as u8]);
    } else if let Ok(v16) = i16::try_from(v) {
        let b = v16.to_le_bytes();
        out.extend_from_slice(&[P3J_INT_I16, b[0], b[1]]);
    } else if let Ok(v32) = i32::try_from(v) {
        let b = v32.to_le_bytes();
        out.extend_from_slice(&[P3J_INT_I32, b[0], b[1], b[2], b[3]]);
    } else {
        let b = v.to_le_bytes();
        out.extend_from_slice(&[P3J_INT_I64, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    }
}

const JSON_INVALID_CHAR: u32 = 0x0011_0000;

#[cold]
#[inline(never)]
fn err_malformed() -> SqliteError {
    SqliteError::new(ErrorCode::Error, "malformed JSON")
}

#[cold]
#[inline(never)]
fn err_too_deep() -> SqliteError {
    SqliteError::new(ErrorCode::Error, "JSON nested too deep")
}

#[cold]
#[inline(never)]
fn err_bad_path(z_path: &str) -> SqliteError {
    SqliteError::new(
        ErrorCode::Error,
        format!("bad JSON path: '{}'", z_path.replace('\'', "''")),
    )
}

#[cold]
#[inline(never)]
fn err_not_array_element(z_path: &str) -> SqliteError {
    SqliteError::new(
        ErrorCode::Error,
        format!("not an array element: '{}'", z_path.replace('\'', "''")),
    )
}

/// Formats a finite or infinite `f64` using SQLite `%!0.17g` semantics (`json.c:814`).
fn format_json_real(f: f64) -> &'static str {
    if f.is_sign_positive() {
        "9.0e+999"
    } else {
        "-9.0e+999"
    }
}

fn format_finite_json_real(f: f64) -> String {
    debug_assert!(f.is_finite());
    if f == 0.0 {
        return "0.0".to_owned();
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    let abs_f = f.abs();
    // Shortest round-trip scientific form (matching Dragonbox / `%!0.17g` precision).
    let sci = format!("{abs_f:e}");
    let (mantissa, exp_str) = sci.split_once('e').expect("scientific format has 'e'");
    let exp: i32 = exp_str.parse().expect("valid exponent");
    let digits: Vec<u8> = mantissa.bytes().filter(|&b| b != b'.').collect();
    // Trim trailing zeros down to at least 1 significant digit.
    let mut sig_len = digits.len();
    while sig_len > 1 && digits[sig_len - 1] == b'0' {
        sig_len -= 1;
    }
    let sig = &digits[..sig_len];

    if (-4..17).contains(&exp) {
        let mut out = String::with_capacity(24);
        out.push_str(sign);
        if exp < 0 {
            out.push_str("0.");
            for _ in 0..(-exp - 1) {
                out.push('0');
            }
            for &d in sig {
                out.push(d as char);
            }
        } else {
            let int_digits = (exp as usize) + 1;
            if int_digits >= sig.len() {
                for &d in sig {
                    out.push(d as char);
                }
                for _ in 0..(int_digits - sig.len()) {
                    out.push('0');
                }
                out.push_str(".0");
            } else {
                for &d in &sig[..int_digits] {
                    out.push(d as char);
                }
                out.push('.');
                for &d in &sig[int_digits..] {
                    out.push(d as char);
                }
            }
        }
        out
    } else {
        let mut out = String::with_capacity(28);
        out.push_str(sign);
        out.push(sig[0] as char);
        out.push('.');
        if sig.len() == 1 {
            out.push('0');
        } else {
            for &d in &sig[1..] {
                out.push(d as char);
            }
        }
        out.push('e');
        if exp >= 0 {
            out.push('+');
        } else {
            out.push('-');
        }
        let abs_exp = exp.unsigned_abs();
        if abs_exp < 10 {
            out.push('0');
        }
        out.push_str(&abs_exp.to_string());
        out
    }
}

#[inline]
fn err_blob_forbidden() -> SqliteError {
    SqliteError::new(ErrorCode::Error, "JSON cannot hold BLOB values")
}

#[inline]
fn is_json_plain_ok_byte(b: u8) -> bool {
    // Printable ASCII or UTF-8 multi-byte byte except `"`, `\`, `'`, and control chars <= 0x1f.
    b >= 0x20 && b != b'"' && b != b'\\' && b != b'\''
}

#[inline]
fn hex_val(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some(u32::from(b - b'0')),
        b'a'..=b'f' => Some(u32::from(b - b'a' + 10)),
        b'A'..=b'F' => Some(u32::from(b - b'A' + 10)),
        _ => None,
    }
}

#[inline]
fn parse_4hex(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    let d0 = hex_val(bytes[0])?;
    let d1 = hex_val(bytes[1])?;
    let d2 = hex_val(bytes[2])?;
    let d3 = hex_val(bytes[3])?;
    Some((d0 << 12) | (d1 << 8) | (d2 << 4) | d3)
}

#[inline]
fn parse_2hex(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 2 {
        return None;
    }
    let d0 = hex_val(bytes[0])?;
    let d1 = hex_val(bytes[1])?;
    Some((d0 << 4) | d1)
}

/// Decodes the JSONB header at `blob[offset]`, returning `Some((header_len, payload_size))`
/// if and only if the header and payload fit completely within `blob`.
#[inline]
pub fn jsonb_payload_size(blob: &[u8], offset: usize) -> Option<(usize, usize)> {
    let first = *blob.get(offset)?;
    let sz_code = first >> 4;
    if sz_code < 12 {
        let sz = usize::from(sz_code);
        let end = offset + 1 + sz;
        return (end <= blob.len()).then_some((1, sz));
    }
    let (hdr_len, sz) = match sz_code {
        12 => {
            let b1 = *blob.get(offset.checked_add(1)?)?;
            (2usize, usize::from(b1))
        }
        13 => {
            let b1 = *blob.get(offset.checked_add(1)?)?;
            let b2 = *blob.get(offset.checked_add(2)?)?;
            (3usize, usize::from(u16::from_be_bytes([b1, b2])))
        }
        14 => {
            let end = offset.checked_add(5)?;
            let slice = blob.get(offset + 1..end)?;
            let v = u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
            let sz = usize::try_from(v).ok()?;
            (5usize, sz)
        }
        15 => {
            let end = offset.checked_add(9)?;
            let slice = blob.get(offset + 1..end)?;
            if slice[0] != 0 || slice[1] != 0 || slice[2] != 0 || slice[3] != 0 {
                return None;
            }
            let v = u32::from_be_bytes([slice[4], slice[5], slice[6], slice[7]]);
            let sz = usize::try_from(v).ok()?;
            (9usize, sz)
        }
        _ => unreachable!(),
    };
    let end = offset.checked_add(hdr_len)?.checked_add(sz)?;
    if end > blob.len() {
        return None;
    }
    Some((hdr_len, sz))
}

/// Appends a canonical JSONB node header (`etype` in `0..=12` and minimal size encoding).
pub fn jsonb_append_header(out: &mut Vec<u8>, etype: u8, sz: usize) {
    let et = etype & 0x0f;
    if sz <= 11 {
        out.push(et | ((sz as u8) << 4));
    } else if sz <= 0xff {
        out.push(et | 0xc0);
        out.push(sz as u8);
    } else if sz <= 0xffff {
        out.push(et | 0xd0);
        out.extend_from_slice(&(sz as u16).to_be_bytes());
    } else if let Ok(sz32) = u32::try_from(sz) {
        out.push(et | 0xe0);
        out.extend_from_slice(&sz32.to_be_bytes());
    } else {
        out.push(et | 0xf0);
        out.extend_from_slice(&(sz as u64).to_be_bytes());
    }
}

/// Appends a complete canonical JSONB leaf or pre-built container node to `out`.
pub fn jsonb_append_node(out: &mut Vec<u8>, etype: u8, payload: &[u8]) {
    jsonb_append_header(out, etype, payload.len());
    out.extend_from_slice(payload);
}

fn json_bytes_to_bypass(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'\\' {
            return i;
        }
        if bytes[i + 1] == b'\n' {
            i += 2;
            continue;
        }
        if bytes[i + 1] == b'\r' {
            if i + 2 < bytes.len() && bytes[i + 2] == b'\n' {
                i += 3;
            } else {
                i += 2;
            }
            continue;
        }
        if bytes[i + 1] == 0xe2
            && i + 3 < bytes.len()
            && bytes[i + 2] == 0x80
            && (bytes[i + 3] == 0xa8 || bytes[i + 3] == 0xa9)
        {
            i += 4;
            continue;
        }
        break;
    }
    i
}

/// Decodes a single escape sequence starting at `bytes[0] == b'\\'`.
/// Returns `(consumed_bytes, codepoint_or_JSON_INVALID_CHAR)`.
fn json_unescape_one_char(bytes: &[u8]) -> (usize, u32) {
    if bytes.len() < 2 || bytes[0] != b'\\' {
        return (bytes.len(), JSON_INVALID_CHAR);
    }
    match bytes[1] {
        b'u' => {
            let Some(v) = bytes.get(2..6).and_then(parse_4hex) else {
                return (bytes.len(), JSON_INVALID_CHAR);
            };
            if (v & 0xfc00) == 0xd800 {
                if bytes.len() >= 12
                    && bytes[6] == b'\\'
                    && bytes[7] == b'u'
                    && let Some(vlo) = parse_4hex(&bytes[8..12])
                    && (vlo & 0xfc00) == 0xdc00
                {
                    let cp = ((v & 0x03ff) << 10) + (vlo & 0x03ff) + 0x10000;
                    return (12, cp);
                }
                return (6, 0xfffd);
            }
            if (v & 0xfc00) == 0xdc00 {
                return (6, 0xfffd);
            }
            (6, v)
        }
        b'x' => {
            let Some(v) = bytes.get(2..4).and_then(parse_2hex) else {
                return (bytes.len(), JSON_INVALID_CHAR);
            };
            (4, v)
        }
        b'"' => (2, u32::from(b'"')),
        b'\'' => (2, u32::from(b'\'')),
        b'\\' => (2, u32::from(b'\\')),
        b'/' => (2, u32::from(b'/')),
        b'b' => (2, 0x08),
        b'f' => (2, 0x0c),
        b'n' => (2, 0x0a),
        b'r' => (2, 0x0d),
        b't' => (2, 0x09),
        b'v' => (2, 0x0b),
        b'0' => {
            if bytes.get(2).is_some_and(u8::is_ascii_digit) {
                (2, JSON_INVALID_CHAR)
            } else {
                (2, 0)
            }
        }
        b'\n' | b'\r' | 0xe2 => {
            let skip = json_bytes_to_bypass(bytes);
            if skip == 0 {
                (2, JSON_INVALID_CHAR)
            } else {
                // Signals line continuation (0 characters emitted).
                (skip, u32::MAX)
            }
        }
        _ => (2, JSON_INVALID_CHAR),
    }
}

fn p3v2_node_validity_check(blob: &[u8], pos: usize, depth: usize) -> (usize, usize) {
    if depth > JSON_MAX_DEPTH || pos >= blob.len() {
        return (pos + 1, 0);
    }
    let Some(node_len) = p3v2_node_len(blob, pos) else {
        return (pos + 1, 0);
    };
    let end = pos + node_len;
    match blob[pos] {
        P3J_NULL | P3J_FALSE | P3J_TRUE | P3J_EMPTY_ARRAY | P3J_EMPTY_OBJECT => (0, end),
        P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => (0, end),
        P3J_FLOAT_RAW => {
            let raw_start = pos + 11;
            let raw = &blob[raw_start..end];
            if raw.len() < 2 || std::str::from_utf8(raw).is_err() {
                return (raw_start + 1, 0);
            }
            if !raw
                .iter()
                .all(|&c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E'))
            {
                return (raw_start + 1, 0);
            }
            (0, end)
        }
        t if t >= P3J_STR_SHORT_BASE || t == P3J_STR_PLAIN => {
            let s_start = pos + if t >= P3J_STR_SHORT_BASE { 1 } else { 3 };
            let payload = &blob[s_start..end];
            if std::str::from_utf8(payload).is_err() {
                return (s_start + 1, 0);
            }
            let mut j = s_start + sqliters_simd::scan_json_plain_ascii_string(payload);
            while j < end {
                if !is_json_plain_ok_byte(blob[j]) && blob[j] != b'\'' {
                    return (j + 1, 0);
                }
                j += 1;
            }
            (0, end)
        }
        P3J_STR_ESCAPED => {
            let s_start = pos + 3;
            let payload = &blob[s_start..end];
            if std::str::from_utf8(payload).is_err() {
                return (s_start + 1, 0);
            }
            let mut j = s_start;
            while j < end {
                let b = blob[j];
                if !is_json_plain_ok_byte(b) && b != b'\'' {
                    if b == b'"' || b <= 0x1f || b != b'\\' || j + 1 >= end {
                        return (j + 1, 0);
                    }
                    if matches!(
                        blob[j + 1],
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't'
                    ) {
                        j += 1;
                    } else if blob[j + 1] == b'u' {
                        if j + 5 >= end || parse_4hex(&blob[j + 2..j + 6]).is_none() {
                            return (j + 1, 0);
                        }
                        j += 5;
                    } else {
                        return (j + 1, 0);
                    }
                }
                j += 1;
            }
            (0, end)
        }
        P3J_STR_RAW => {
            let s_start = pos + 3;
            if std::str::from_utf8(&blob[s_start..end]).is_err() {
                return (s_start + 1, 0);
            }
            (0, end)
        }
        P3J_OBJECT_COMPACT => {
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(blob[pos + 2]);
            let dir_abs = pos + dir_rel;
            let mut cursor = pos + 3;
            for m in 0..n_keys {
                let s = dir_abs + 2 * m;
                let kinfo = blob[s];
                let val_rel = usize::from(blob[s + 1]);
                let klen = usize::from(kinfo >> 2);
                let val_pos = pos + val_rel;
                if val_pos < cursor + 1 + klen || val_pos >= dir_abs {
                    return (s + 1, 0);
                }
                let key_pos = val_pos - 1 - klen;
                if key_pos != cursor || blob[key_pos] != (P3J_STR_SHORT_BASE | (klen as u8)) {
                    return (s + 1, 0);
                }
                let (kerr, kend) = p3v2_node_validity_check(blob, key_pos, depth + 1);
                if kerr != 0 {
                    return (kerr, 0);
                }
                if kend != val_pos {
                    return (s + 2, 0);
                }
                let kbytes = &blob[key_pos + 1..val_pos];
                if p3v2_compact_kinfo(kbytes, true) != Some(kinfo) {
                    return (s + 1, 0);
                }
                let (verr, vend) = p3v2_node_validity_check(blob, val_pos, depth + 1);
                if verr != 0 {
                    return (verr, 0);
                }
                cursor = vend;
            }
            if cursor != dir_abs {
                return (dir_abs + 1, 0);
            }
            (0, end)
        }
        P3J_OBJECT => {
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 2], blob[pos + 3]]));
            let dir_abs = pos + dir_rel;
            let mut cursor = pos + 6;
            for m in 0..n_keys {
                let s = dir_abs + 8 * m;
                let slot_tag = u32::from_le_bytes([blob[s], blob[s + 1], blob[s + 2], blob[s + 3]]);
                let key_rel = usize::from(u16::from_le_bytes([blob[s + 4], blob[s + 5]]));
                let val_rel = usize::from(u16::from_le_bytes([blob[s + 6], blob[s + 7]]));
                let key_pos = pos + key_rel;
                let val_pos = pos + val_rel;
                if key_pos != cursor || key_pos >= dir_abs {
                    return (s + 1, 0);
                }
                let Some((ktag, khdr_len, _)) = p3v2_str_header(blob, key_pos) else {
                    return (key_pos + 1, 0);
                };
                let (kerr, kend) = p3v2_node_validity_check(blob, key_pos, depth + 1);
                if kerr != 0 {
                    return (kerr, 0);
                }
                if kend != val_pos {
                    return (s + 5, 0);
                }
                let kbytes = &blob[key_pos + khdr_len..kend];
                let expected_tag = p3v2_key_tag(kbytes, ktag != P3J_STR_ESCAPED);
                if slot_tag != expected_tag {
                    return (s + 1, 0);
                }
                let (verr, vend) = p3v2_node_validity_check(blob, val_pos, depth + 1);
                if verr != 0 {
                    return (verr, 0);
                }
                cursor = vend;
            }
            if cursor != dir_abs {
                return (dir_abs + 1, 0);
            }
            (0, end)
        }
        P3J_ARRAY => {
            let n_elems = usize::from(u16::from_le_bytes([blob[pos + 1], blob[pos + 2]]));
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 3], blob[pos + 4]]));
            let dir_abs = pos + dir_rel;
            let mut cursor = pos + 7;
            for m in 0..n_elems {
                let s = dir_abs + 2 * m;
                let elem_rel = usize::from(u16::from_le_bytes([blob[s], blob[s + 1]]));
                let elem_pos = pos + elem_rel;
                if elem_pos != cursor || elem_pos >= dir_abs {
                    return (s + 1, 0);
                }
                let (cerr, cend) = p3v2_node_validity_check(blob, elem_pos, depth + 1);
                if cerr != 0 {
                    return (cerr, 0);
                }
                cursor = cend;
            }
            if cursor != dir_abs {
                return (dir_abs + 1, 0);
            }
            (0, end)
        }
        _ => (pos + 1, 0),
    }
}

fn p3v2_jsonb_validity_check(blob: &[u8], depth: usize) -> usize {
    if !is_p3v2_jsonb(blob) || depth > JSON_MAX_DEPTH {
        return 1;
    }
    let (err, end) = p3v2_node_validity_check(blob, 4, depth);
    if err != 0 {
        err
    } else if end != blob.len() {
        end.min(blob.len())
    } else {
        0
    }
}

/// Deep JSONB validity check (`jsonbValidityCheck` in `json.c`).
/// Returns `0` if valid, or the 1-based byte offset of the first invalid element/byte.
pub fn jsonb_validity_check(blob: &[u8], i: usize, i_end: usize, depth: usize) -> usize {
    if i == 0 && i_end == blob.len() && is_p3v2_jsonb(blob) {
        return p3v2_jsonb_validity_check(blob, depth);
    }
    if depth > JSON_MAX_DEPTH || i >= blob.len() || i_end > blob.len() || i >= i_end {
        return i + 1;
    }
    let Some((n, sz)) = jsonb_payload_size(blob, i) else {
        return i + 1;
    };
    if i + n + sz != i_end {
        return i + 1;
    }
    let payload_start = i + n;
    let payload_end = payload_start + sz;
    let payload = &blob[payload_start..payload_end];
    if matches!(
        blob[i] & 0x0f,
        JSONB_TEXT | JSONB_TEXTJ | JSONB_TEXT5 | JSONB_TEXTRAW
    ) && std::str::from_utf8(payload).is_err()
    {
        return payload_start + 1;
    }
    let x = blob[i] & 0x0f;
    match x {
        JSONB_NULL | JSONB_TRUE | JSONB_FALSE => {
            if n + sz == 1 {
                0
            } else {
                i + 1
            }
        }
        JSONB_INT => {
            if sz < 1 {
                return i + 1;
            }
            let mut j = payload_start;
            if blob[j] == b'-' {
                j += 1;
                if sz < 2 {
                    return i + 1;
                }
            }
            while j < payload_end {
                if blob[j].is_ascii_digit() {
                    j += 1;
                } else {
                    return j + 1;
                }
            }
            0
        }
        JSONB_INT5 => {
            if sz < 3 {
                return i + 1;
            }
            let mut j = payload_start;
            if blob[j] == b'-' {
                if sz < 4 {
                    return i + 1;
                }
                j += 1;
            }
            if blob[j] != b'0' {
                return i + 1;
            }
            if blob[j + 1] != b'x' && blob[j + 1] != b'X' {
                return j + 2;
            }
            j += 2;
            while j < payload_end {
                if blob[j].is_ascii_hexdigit() {
                    j += 1;
                } else {
                    return j + 1;
                }
            }
            0
        }
        JSONB_FLOAT | JSONB_FLOAT5 => {
            let mut seen = 0u8;
            if sz < 2 {
                return i + 1;
            }
            let mut j = payload_start;
            if blob[j] == b'-' {
                j += 1;
                if sz < 3 {
                    return i + 1;
                }
            }
            if blob[j] == b'.' {
                if x == JSONB_FLOAT {
                    return j + 1;
                }
                if !blob.get(j + 1).is_some_and(u8::is_ascii_digit) {
                    return j + 1;
                }
                j += 2;
                seen = 1;
            } else if blob[j] == b'0' && x == JSONB_FLOAT {
                if j + 3 > payload_end {
                    return j + 1;
                }
                if blob[j + 1] != b'.' && blob[j + 1] != b'e' && blob[j + 1] != b'E' {
                    return j + 1;
                }
                j += 1;
            }
            while j < payload_end {
                if blob[j].is_ascii_digit() {
                    j += 1;
                    continue;
                }
                if blob[j] == b'.' {
                    if seen > 0 {
                        return j + 1;
                    }
                    if x == JSONB_FLOAT && (j + 1 == payload_end || !blob[j + 1].is_ascii_digit()) {
                        return j + 1;
                    }
                    seen = 1;
                    j += 1;
                    continue;
                }
                if blob[j] == b'e' || blob[j] == b'E' {
                    if seen == 2 || j + 1 == payload_end {
                        return j + 1;
                    }
                    if blob[j + 1] == b'+' || blob[j + 1] == b'-' {
                        j += 1;
                        if j + 1 == payload_end {
                            return j + 1;
                        }
                    }
                    seen = 2;
                    j += 1;
                    continue;
                }
                return j + 1;
            }
            if seen == 0 { i + 1 } else { 0 }
        }
        JSONB_TEXT => {
            let mut j = payload_start + sqliters_simd::scan_json_plain_ascii_string(payload);
            while j < payload_end {
                if !is_json_plain_ok_byte(blob[j]) && blob[j] != b'\'' {
                    return j + 1;
                }
                j += 1;
            }
            0
        }
        JSONB_TEXTJ | JSONB_TEXT5 => {
            let mut j = payload_start;
            while j < payload_end {
                let b = blob[j];
                if !is_json_plain_ok_byte(b) && b != b'\'' {
                    if b == b'"' {
                        if x == JSONB_TEXTJ {
                            return j + 1;
                        }
                    } else if b <= 0x1f {
                        if x == JSONB_TEXTJ || b == 0 {
                            return j + 1;
                        }
                    } else if b != b'\\' || j + 1 >= payload_end {
                        return j + 1;
                    } else if matches!(
                        blob[j + 1],
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't'
                    ) {
                        j += 1;
                    } else if blob[j + 1] == b'u' {
                        if j + 5 >= payload_end || parse_4hex(&blob[j + 2..j + 6]).is_none() {
                            return j + 1;
                        }
                        j += 5;
                    } else if x != JSONB_TEXT5 {
                        return j + 1;
                    } else {
                        let (sz_c, cp) = json_unescape_one_char(&blob[j..payload_end]);
                        if cp == JSON_INVALID_CHAR || sz_c == 0 {
                            return j + 1;
                        }
                        j += sz_c - 1;
                    }
                }
                j += 1;
            }
            0
        }
        JSONB_TEXTRAW => 0,
        JSONB_ARRAY => {
            let mut j = payload_start;
            while j < payload_end {
                let Some((cn, csz)) = jsonb_payload_size(&blob[..payload_end], j) else {
                    return j + 1;
                };
                let child_end = j + cn + csz;
                let sub = jsonb_validity_check(blob, j, child_end, depth + 1);
                if sub != 0 {
                    return sub;
                }
                j = child_end;
            }
            0
        }
        JSONB_OBJECT => {
            let mut cnt = 0usize;
            let mut j = payload_start;
            while j < payload_end {
                let Some((cn, csz)) = jsonb_payload_size(&blob[..payload_end], j) else {
                    return j + 1;
                };
                if (cnt & 1) == 0 {
                    let kx = blob[j] & 0x0f;
                    if !(JSONB_TEXT..=JSONB_TEXTRAW).contains(&kx) {
                        return j + 1;
                    }
                }
                let child_end = j + cn + csz;
                let sub = jsonb_validity_check(blob, j, child_end, depth + 1);
                if sub != 0 {
                    return sub;
                }
                cnt += 1;
                j = child_end;
            }
            if (cnt & 1) != 0 { j + 1 } else { 0 }
        }
        _ => i + 1,
    }
}

/// Superficial + short-text-disambiguation JSONB recognition (`jsonArgIsJsonb` in `json.c`).
pub fn blob_is_jsonb(blob: &[u8]) -> bool {
    if blob.is_empty() {
        return false;
    }
    if is_p3v2_jsonb(blob) {
        return true;
    }
    let c = blob[0];
    let etype = c & 0x0f;
    if etype > JSONB_OBJECT {
        return false;
    }
    let Some((n, sz)) = jsonb_payload_size(blob, 0) else {
        return false;
    };
    if n + sz != blob.len() {
        return false;
    }
    if etype <= JSONB_FALSE && sz != 0 {
        return false;
    }
    if sz <= 7 && (c == b'{' || c == b'[' || c.is_ascii_digit()) {
        return jsonb_validity_check(blob, 0, blob.len(), 1) == 0;
    }
    true
}

#[derive(Debug, Clone)]
pub struct JsonTextParseOutcome {
    pub jsonb: Vec<u8>,
    pub has_nonstd: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct JsonTextParseError {
    pub byte_offset: usize,
    pub too_deep: bool,
}

struct TextParser<'a, 'b> {
    src: &'a str,
    bytes: &'a [u8],
    out: &'b mut Vec<u8>,
    depth: usize,
    has_nonstd: bool,
}

#[inline]
fn finish_container_in_place(out: &mut Vec<u8>, hdr_pos: usize, etype: u8) {
    let children_start = hdr_pos + 2;
    let sz = out.len() - children_start;
    let et = etype & 0x0f;
    if (12..=0xff).contains(&sz) {
        out[hdr_pos] = et | 0xc0;
        out[hdr_pos + 1] = sz as u8;
    } else if sz <= 11 {
        out[hdr_pos] = et | ((sz as u8) << 4);
        out.copy_within(children_start..children_start + sz, hdr_pos + 1);
        out.truncate(hdr_pos + 1 + sz);
    } else if sz <= 0xffff {
        out.push(0);
        out.copy_within(children_start..children_start + sz, hdr_pos + 3);
        let be = (sz as u16).to_be_bytes();
        out[hdr_pos] = et | 0xd0;
        out[hdr_pos + 1] = be[0];
        out[hdr_pos + 2] = be[1];
    } else if let Ok(sz32) = u32::try_from(sz) {
        out.extend_from_slice(&[0, 0, 0]);
        out.copy_within(children_start..children_start + sz, hdr_pos + 5);
        let be = sz32.to_be_bytes();
        out[hdr_pos] = et | 0xe0;
        out[hdr_pos + 1..hdr_pos + 5].copy_from_slice(&be);
    } else {
        out.extend_from_slice(&[0; 7]);
        out.copy_within(children_start..children_start + sz, hdr_pos + 9);
        let be = (sz as u64).to_be_bytes();
        out[hdr_pos] = et | 0xf0;
        out[hdr_pos + 1..hdr_pos + 9].copy_from_slice(&be);
    }
}

impl<'a, 'b> TextParser<'a, 'b> {
    #[inline]
    fn new(src: &'a str, out: &'b mut Vec<u8>) -> Self {
        out.clear();
        out.reserve(src.len().saturating_add(16));
        Self {
            src,
            bytes: src.as_bytes(),
            out,
            depth: 0,
            has_nonstd: false,
        }
    }

    fn err(&self, pos: usize) -> JsonTextParseError {
        JsonTextParseError {
            byte_offset: pos.min(self.bytes.len()),
            too_deep: false,
        }
    }

    #[inline(always)]
    fn skip_ws_and_comments(&mut self, i: usize) -> std::result::Result<usize, JsonTextParseError> {
        let b = self.bytes;
        if i < b.len() {
            let c = b[i];
            if c > b' ' && c != b'/' && c < 0x80 {
                return Ok(i);
            }
            self.skip_ws_and_comments_slow(i)
        } else {
            Ok(i)
        }
    }

    #[inline(never)]
    fn skip_ws_and_comments_slow(
        &mut self,
        mut i: usize,
    ) -> std::result::Result<usize, JsonTextParseError> {
        let b = self.bytes;
        while i < b.len() {
            match b[i] {
                b' ' | b'\t' | b'\n' | b'\r' => i += 1,
                0x0b | 0x0c => {
                    self.has_nonstd = true;
                    i += 1;
                }
                b'/' => {
                    if i + 1 >= b.len() {
                        return Err(self.err(i));
                    }
                    if b[i + 1] == b'/' {
                        self.has_nonstd = true;
                        i += 2;
                        while i < b.len() {
                            if b[i] == b'\n' || b[i] == b'\r' {
                                i += 1;
                                break;
                            }
                            if b[i] == 0xe2
                                && i + 2 < b.len()
                                && b[i + 1] == 0x80
                                && (b[i + 2] == 0xa8 || b[i + 2] == 0xa9)
                            {
                                i += 3;
                                break;
                            }
                            i += 1;
                        }
                    } else if b[i + 1] == b'*' {
                        self.has_nonstd = true;
                        i += 2;
                        let mut closed = false;
                        while i + 1 < b.len() {
                            if b[i] == b'*' && b[i + 1] == b'/' {
                                i += 2;
                                closed = true;
                                break;
                            }
                            i += 1;
                        }
                        if !closed {
                            return Err(self.err(b.len()));
                        }
                    } else {
                        break;
                    }
                }
                x if x >= 0x80 => {
                    if let Some(ch) = self.src[i..].chars().next()
                        && (ch == '\u{00a0}'
                            || ch == '\u{feff}'
                            || ch == '\u{2028}'
                            || ch == '\u{2029}'
                            || ch.is_whitespace())
                    {
                        self.has_nonstd = true;
                        i += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(i)
    }

    fn parse_string(&mut self, i: usize) -> std::result::Result<usize, JsonTextParseError> {
        let bytes = self.bytes;
        let delim = bytes[i];
        let mut j = i + 1;
        if delim == b'"' {
            j += sqliters_simd::scan_json_plain_ascii_string(&bytes[j..]);
            if j < bytes.len() && bytes[j] == b'"' {
                jsonb_append_node(self.out, JSONB_TEXT, &bytes[i + 1..j]);
                return Ok(j + 1);
            }
        } else if delim == b'\'' {
            self.has_nonstd = true;
        }
        let mut opcode = JSONB_TEXT;
        while j < bytes.len() {
            let c = bytes[j];
            if c == delim {
                jsonb_append_node(self.out, opcode, &bytes[i + 1..j]);
                return Ok(j + 1);
            } else if c == b'\\' {
                j += 1;
                if j >= bytes.len() {
                    return Err(self.err(j));
                }
                let esc = bytes[j];
                if matches!(esc, b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') {
                    if opcode == JSONB_TEXT {
                        opcode = JSONB_TEXTJ;
                    }
                    j += 1;
                } else if esc == b'u' {
                    if j + 4 >= bytes.len() || parse_4hex(&bytes[j + 1..j + 5]).is_none() {
                        return Err(self.err(j));
                    }
                    if opcode == JSONB_TEXT {
                        opcode = JSONB_TEXTJ;
                    }
                    j += 5;
                } else if esc == b'\''
                    || esc == b'v'
                    || esc == b'\n'
                    || (esc == b'0' && !bytes.get(j + 1).is_some_and(u8::is_ascii_digit))
                {
                    opcode = JSONB_TEXT5;
                    self.has_nonstd = true;
                    j += 1;
                } else if esc == 0xe2
                    && j + 2 < bytes.len()
                    && bytes[j + 1] == 0x80
                    && (bytes[j + 2] == 0xa8 || bytes[j + 2] == 0xa9)
                {
                    opcode = JSONB_TEXT5;
                    self.has_nonstd = true;
                    j += 3;
                } else if esc == b'x' {
                    if j + 2 >= bytes.len() || parse_2hex(&bytes[j + 1..j + 3]).is_none() {
                        return Err(self.err(j));
                    }
                    opcode = JSONB_TEXT5;
                    self.has_nonstd = true;
                    j += 3;
                } else if esc == b'\r' {
                    if bytes.get(j + 1) == Some(&b'\n') {
                        j += 1;
                    }
                    opcode = JSONB_TEXT5;
                    self.has_nonstd = true;
                    j += 1;
                } else {
                    return Err(self.err(j));
                }
            } else if c <= 0x1f {
                if c == 0 {
                    return Err(self.err(j));
                }
                opcode = JSONB_TEXT5;
                self.has_nonstd = true;
                j += 1;
            } else if c < 0x80 {
                if c == b'"' {
                    opcode = JSONB_TEXT5;
                }
                j += 1;
            } else {
                // Advance full UTF-8 character safely
                let ch_len = self.src[j..].chars().next().map_or(1, char::len_utf8);
                j += ch_len;
            }
        }
        Err(self.err(j))
    }

    fn parse_unquoted_key(&mut self, i: usize) -> std::result::Result<usize, JsonTextParseError> {
        let mut j = i;
        let mut is_first = true;
        let mut has_escape = false;
        while j < self.bytes.len() {
            let b = self.bytes[j];
            if b == b'\\' {
                if j + 5 < self.bytes.len()
                    && self.bytes[j + 1] == b'u'
                    && let Some(cp) = parse_4hex(&self.bytes[j + 2..j + 6])
                    && let Some(ch) = char::from_u32(cp)
                    && is_json5_ident_char(ch, is_first)
                {
                    has_escape = true;
                    is_first = false;
                    j += 6;
                    continue;
                }
                return Err(self.err(j));
            }
            let Some(ch) = self.src[j..].chars().next() else {
                break;
            };
            if is_json5_ident_char(ch, is_first) {
                is_first = false;
                j += ch.len_utf8();
            } else {
                break;
            }
        }
        if j == i {
            return Err(self.err(i));
        }
        self.has_nonstd = true;
        let opcode = if has_escape { JSONB_TEXTJ } else { JSONB_TEXT };
        jsonb_append_node(self.out, opcode, &self.bytes[i..j]);
        Ok(j)
    }

    fn parse_value(&mut self, start: usize) -> std::result::Result<usize, JsonTextParseError> {
        let i = self.skip_ws_and_comments(start)?;
        if i >= self.bytes.len() {
            return Err(self.err(i));
        }
        match self.bytes[i] {
            b'{' => {
                self.depth += 1;
                if self.depth > JSON_MAX_DEPTH {
                    return Err(JsonTextParseError {
                        byte_offset: i,
                        too_deep: true,
                    });
                }
                let mut j = self.skip_ws_and_comments(i + 1)?;
                if j < self.bytes.len() && self.bytes[j] == b'}' {
                    self.depth -= 1;
                    jsonb_append_node(self.out, JSONB_OBJECT, &[]);
                    return Ok(j + 1);
                }
                let hdr_pos = self.out.len();
                self.out.extend_from_slice(&[0, 0]);
                let res = (|| -> std::result::Result<usize, JsonTextParseError> {
                    loop {
                        if j >= self.bytes.len() {
                            return Err(self.err(j));
                        }
                        if self.bytes[j] == b'"' || self.bytes[j] == b'\'' {
                            j = self.parse_string(j)?;
                        } else {
                            j = self.parse_unquoted_key(j)?;
                        }
                        j = self.skip_ws_and_comments(j)?;
                        if j >= self.bytes.len() || self.bytes[j] != b':' {
                            return Err(self.err(j));
                        }
                        j = self.parse_value(j + 1)?;
                        j = self.skip_ws_and_comments(j)?;
                        if j >= self.bytes.len() {
                            return Err(self.err(j));
                        }
                        if self.bytes[j] == b',' {
                            let after_comma = self.skip_ws_and_comments(j + 1)?;
                            if after_comma < self.bytes.len() && self.bytes[after_comma] == b'}' {
                                self.has_nonstd = true;
                                return Ok(after_comma + 1);
                            }
                            j = after_comma;
                        } else if self.bytes[j] == b'}' {
                            return Ok(j + 1);
                        } else {
                            return Err(self.err(j));
                        }
                    }
                })();
                self.depth -= 1;
                match res {
                    Ok(next_pos) => {
                        finish_container_in_place(self.out, hdr_pos, JSONB_OBJECT);
                        Ok(next_pos)
                    }
                    Err(e) => {
                        self.out.truncate(hdr_pos);
                        Err(e)
                    }
                }
            }
            b'[' => {
                self.depth += 1;
                if self.depth > JSON_MAX_DEPTH {
                    return Err(JsonTextParseError {
                        byte_offset: i,
                        too_deep: true,
                    });
                }
                let mut j = self.skip_ws_and_comments(i + 1)?;
                if j < self.bytes.len() && self.bytes[j] == b']' {
                    self.depth -= 1;
                    jsonb_append_node(self.out, JSONB_ARRAY, &[]);
                    return Ok(j + 1);
                }
                let hdr_pos = self.out.len();
                self.out.extend_from_slice(&[0, 0]);
                let res = (|| -> std::result::Result<usize, JsonTextParseError> {
                    loop {
                        j = self.parse_value(j)?;
                        j = self.skip_ws_and_comments(j)?;
                        if j >= self.bytes.len() {
                            return Err(self.err(j));
                        }
                        if self.bytes[j] == b',' {
                            let after_comma = self.skip_ws_and_comments(j + 1)?;
                            if after_comma < self.bytes.len() && self.bytes[after_comma] == b']' {
                                self.has_nonstd = true;
                                return Ok(after_comma + 1);
                            }
                            j = after_comma;
                        } else if self.bytes[j] == b']' {
                            return Ok(j + 1);
                        } else {
                            return Err(self.err(j));
                        }
                    }
                })();
                self.depth -= 1;
                match res {
                    Ok(next_pos) => {
                        finish_container_in_place(self.out, hdr_pos, JSONB_ARRAY);
                        Ok(next_pos)
                    }
                    Err(e) => {
                        self.out.truncate(hdr_pos);
                        Err(e)
                    }
                }
            }
            b'"' | b'\'' => self.parse_string(i),
            b't' => {
                if self.bytes[i..].starts_with(b"true")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                {
                    jsonb_append_node(self.out, JSONB_TRUE, &[]);
                    Ok(i + 4)
                } else {
                    Err(self.err(i))
                }
            }
            b'f' => {
                if self.bytes[i..].starts_with(b"false")
                    && !self.bytes.get(i + 5).is_some_and(u8::is_ascii_alphanumeric)
                {
                    jsonb_append_node(self.out, JSONB_FALSE, &[]);
                    Ok(i + 5)
                } else {
                    Err(self.err(i))
                }
            }
            b'n' | b'N' => {
                if self.bytes[i..].starts_with(b"null")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                {
                    jsonb_append_node(self.out, JSONB_NULL, &[]);
                    Ok(i + 4)
                } else if self.bytes.len() >= i + 3
                    && self.bytes[i..i + 3].eq_ignore_ascii_case(b"nan")
                    && !self.bytes.get(i + 3).is_some_and(u8::is_ascii_alphanumeric)
                {
                    self.has_nonstd = true;
                    jsonb_append_node(self.out, JSONB_NULL, &[]);
                    Ok(i + 3)
                } else {
                    Err(self.err(i))
                }
            }
            b'i' | b'I' => {
                if let Some(len) = match_inf(&self.bytes[i..]) {
                    self.has_nonstd = true;
                    jsonb_append_node(self.out, JSONB_FLOAT, b"9e999");
                    Ok(i + len)
                } else {
                    Err(self.err(i))
                }
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.parse_number(i),
            _ => Err(self.err(i)),
        }
    }

    fn parse_number(&mut self, i: usize) -> std::result::Result<usize, JsonTextParseError> {
        let b = self.bytes;
        let mut p = i;
        let mut neg = false;
        if b[p] == b'+' {
            self.has_nonstd = true;
            p += 1;
            if p >= b.len() {
                return Err(self.err(i));
            }
            if let Some(len) = match_inf(&b[p..]) {
                jsonb_append_node(self.out, JSONB_FLOAT, b"9e999");
                return Ok(p + len);
            }
        } else if b[p] == b'-' {
            neg = true;
            p += 1;
            if p >= b.len() {
                return Err(self.err(i));
            }
            if let Some(len) = match_inf(&b[p..]) {
                self.has_nonstd = true;
                jsonb_append_node(self.out, JSONB_FLOAT, b"-9e999");
                return Ok(p + len);
            }
        }

        let num_digits_start = p;
        // Hex integer (`0x...` / `0X...`)
        if b[p] == b'0' && p + 1 < b.len() && (b[p + 1] == b'x' || b[p + 1] == b'X') {
            self.has_nonstd = true;
            let mut j = p + 2;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j == p + 2
                || b.get(j)
                    .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'.')
            {
                return Err(self.err(j));
            }
            let slice_start = if neg { i } else { p };
            jsonb_append_node(self.out, JSONB_INT5, &b[slice_start..j]);
            return Ok(j);
        }

        let mut is_float = false;
        let mut is_float5 = false;
        let mut j = p;

        if b[j] == b'.' {
            self.has_nonstd = true;
            is_float = true;
            is_float5 = true;
            j += 1;
            if j >= b.len() || !b[j].is_ascii_digit() {
                return Err(self.err(j.min(b.len())));
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        } else if b[j] == b'0' {
            j += 1;
            if j < b.len() && b[j].is_ascii_digit() {
                return Err(self.err(j));
            }
        } else if b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        } else {
            return Err(self.err(j));
        }

        if !is_float5 && j < b.len() && b[j] == b'.' {
            is_float = true;
            j += 1;
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            } else {
                self.has_nonstd = true;
                is_float5 = true;
            }
        }

        if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
            is_float = true;
            j += 1;
            if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                j += 1;
            }
            if j >= b.len() || !b[j].is_ascii_digit() {
                return Err(self.err(j));
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        }

        if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
            return Err(self.err(j));
        }

        let etype = if is_float5 {
            JSONB_FLOAT5
        } else if is_float {
            JSONB_FLOAT
        } else {
            JSONB_INT
        };
        let slice_start = if neg { i } else { num_digits_start };
        jsonb_append_node(self.out, etype, &b[slice_start..j]);
        Ok(j)
    }
}

fn match_inf(bytes: &[u8]) -> Option<usize> {
    if bytes.len() >= 8
        && bytes[..8].eq_ignore_ascii_case(b"infinity")
        && !bytes.get(8).is_some_and(u8::is_ascii_alphanumeric)
    {
        Some(8)
    } else if bytes.len() >= 3
        && bytes[..3].eq_ignore_ascii_case(b"inf")
        && !bytes.get(3).is_some_and(u8::is_ascii_alphanumeric)
    {
        Some(3)
    } else {
        None
    }
}

fn is_json5_ident_char(ch: char, is_first: bool) -> bool {
    if ch == '$' || ch == '_' || ch.is_ascii_alphabetic() {
        return true;
    }
    if !is_first && (ch.is_ascii_digit() || ch == '\u{200c}' || ch == '\u{200d}') {
        return true;
    }
    if !ch.is_ascii() {
        if ch.is_alphabetic() {
            return true;
        }
        if !is_first && ch.is_numeric() {
            return true;
        }
    }
    false
}

struct Stage1JsonParser<'a, 'b> {
    bytes: &'a [u8],
    out: &'b mut Vec<u8>,
    win_base: usize,
    win_end: usize,
    masks: sqliters_simd::JsonStage1Masks,
    depth: usize,
}

impl<'a, 'b> Stage1JsonParser<'a, 'b> {
    #[inline(always)]
    fn ensure_at(&mut self, i: usize) -> Option<usize> {
        if i < self.win_end {
            return Some(i - self.win_base);
        }
        let (base, end, masks) = sqliters_simd::classify_json_stage1_at(self.bytes, i)?;
        self.win_base = base;
        self.win_end = end;
        self.masks = masks;
        Some(i - base)
    }

    #[inline(always)]
    fn skip_spaces(&mut self, mut i: usize) -> Option<usize> {
        if i < self.win_end {
            let rem = self.masks.non_space_mask >> (i - self.win_base);
            if rem != 0 {
                return Some(i + rem.trailing_zeros() as usize);
            }
            i = self.win_end;
        }
        while i < self.bytes.len() {
            let (base, end, masks) = sqliters_simd::classify_json_stage1_at(self.bytes, i)?;
            self.win_base = base;
            self.win_end = end;
            self.masks = masks;
            let rem = masks.non_space_mask >> (i - base);
            if rem != 0 {
                return Some(i + rem.trailing_zeros() as usize);
            }
            i = end;
        }
        Some(i)
    }

    #[inline(always)]
    fn parse_string(&mut self, i: usize) -> Option<usize> {
        let start = i + 1;
        let mut cur = start;
        while cur < self.bytes.len() {
            let rel = self.ensure_at(cur)?;
            let rem_q = self.masks.quote_mask >> rel;
            if rem_q != 0 {
                let end_q = cur + rem_q.trailing_zeros() as usize;
                jsonb_append_node(self.out, JSONB_TEXT, &self.bytes[start..end_q]);
                return Some(end_q + 1);
            }
            cur = self.win_end;
        }
        None
    }

    #[inline(always)]
    fn skip_digits(&mut self, mut cur: usize) -> Option<usize> {
        while cur < self.bytes.len() {
            let rel = self.ensure_at(cur)?;
            let run = (self.masks.digit_mask >> rel).trailing_ones() as usize;
            cur += run;
            if cur < self.win_end || cur >= self.bytes.len() {
                break;
            }
        }
        Some(cur)
    }

    #[inline(always)]
    fn parse_number(&mut self, i: usize) -> Option<usize> {
        let b = self.bytes;
        let p = if b[i] == b'-' { i + 1 } else { i };
        let &first = b.get(p)?;
        let mut j = if first == b'0' {
            if b.get(p + 1).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            p + 1
        } else if first.is_ascii_digit() {
            self.skip_digits(p)?
        } else {
            return None;
        };

        let mut is_float = false;
        if b.get(j) == Some(&b'.') {
            if !b.get(j + 1).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            is_float = true;
            j = self.skip_digits(j + 1)?;
        }
        if matches!(b.get(j), Some(b'e' | b'E')) {
            is_float = true;
            j += 1;
            if matches!(b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if !b.get(j).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            j = self.skip_digits(j)?;
        }
        if b.get(j)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_' || *c == b'.')
        {
            return None;
        }
        let etype = if is_float { JSONB_FLOAT } else { JSONB_INT };
        jsonb_append_node(self.out, etype, &b[i..j]);
        Some(j)
    }

    fn parse_value(&mut self, start: usize) -> Option<usize> {
        let i = self.skip_spaces(start)?;
        let &c = self.bytes.get(i)?;
        match c {
            b'{' => {
                self.depth += 1;
                if self.depth > JSON_MAX_DEPTH {
                    return None;
                }
                let mut j = self.skip_spaces(i + 1)?;
                if self.bytes.get(j) == Some(&b'}') {
                    self.depth -= 1;
                    jsonb_append_node(self.out, JSONB_OBJECT, &[]);
                    return Some(j + 1);
                }
                let hdr_pos = self.out.len();
                self.out.extend_from_slice(&[0, 0]);
                loop {
                    if self.bytes.get(j) != Some(&b'"') {
                        return None;
                    }
                    j = self.parse_string(j)?;
                    j = self.skip_spaces(j)?;
                    if self.bytes.get(j) != Some(&b':') {
                        return None;
                    }
                    j = self.parse_value(j + 1)?;
                    j = self.skip_spaces(j)?;
                    match self.bytes.get(j) {
                        Some(&b',') => {
                            j = self.skip_spaces(j + 1)?;
                            if self.bytes.get(j) == Some(&b'}') {
                                return None;
                            }
                        }
                        Some(&b'}') => {
                            self.depth -= 1;
                            finish_container_in_place(self.out, hdr_pos, JSONB_OBJECT);
                            return Some(j + 1);
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.depth += 1;
                if self.depth > JSON_MAX_DEPTH {
                    return None;
                }
                let mut j = self.skip_spaces(i + 1)?;
                if self.bytes.get(j) == Some(&b']') {
                    self.depth -= 1;
                    jsonb_append_node(self.out, JSONB_ARRAY, &[]);
                    return Some(j + 1);
                }
                let hdr_pos = self.out.len();
                self.out.extend_from_slice(&[0, 0]);
                loop {
                    j = self.parse_value(j)?;
                    j = self.skip_spaces(j)?;
                    match self.bytes.get(j) {
                        Some(&b',') => {
                            j = self.skip_spaces(j + 1)?;
                            if self.bytes.get(j) == Some(&b']') {
                                return None;
                            }
                        }
                        Some(&b']') => {
                            self.depth -= 1;
                            finish_container_in_place(self.out, hdr_pos, JSONB_ARRAY);
                            return Some(j + 1);
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self.parse_string(i),
            b't' => {
                if self.bytes[i..].starts_with(b"true")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                {
                    jsonb_append_node(self.out, JSONB_TRUE, &[]);
                    Some(i + 4)
                } else {
                    None
                }
            }
            b'f' => {
                if self.bytes[i..].starts_with(b"false")
                    && !self.bytes.get(i + 5).is_some_and(u8::is_ascii_alphanumeric)
                {
                    jsonb_append_node(self.out, JSONB_FALSE, &[]);
                    Some(i + 5)
                } else {
                    None
                }
            }
            b'n' => {
                if self.bytes[i..].starts_with(b"null")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                {
                    jsonb_append_node(self.out, JSONB_NULL, &[]);
                    Some(i + 4)
                } else {
                    None
                }
            }
            b'-' | b'0'..=b'9' => self.parse_number(i),
            _ => None,
        }
    }
}

#[inline]
fn try_parse_json_stage1_simd(src: &str, out: &mut Vec<u8>) -> bool {
    let bytes = src.as_bytes();
    let Some((win_base, win_end, masks)) = sqliters_simd::classify_json_stage1_at(bytes, 0) else {
        return false;
    };
    out.clear();
    out.reserve(bytes.len().saturating_add(16));
    let mut fast = Stage1JsonParser {
        bytes,
        out,
        win_base,
        win_end,
        masks,
        depth: 0,
    };
    if let Some(end) = fast.parse_value(0)
        && let Some(tail) = fast.skip_spaces(end)
        && tail == bytes.len()
    {
        return true;
    }
    out.clear();
    false
}

#[inline(always)]
fn skip_rfc8259_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        let c = bytes[i];
        if c > b' ' {
            break;
        }
        if matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        } else {
            break;
        }
    }
    i
}

struct P3SinglePassEncoder<'a, 'b> {
    src: &'a str,
    bytes: &'a [u8],
    out: &'b mut Vec<u8>,
    obj_slots_stack: [u64; 16],
    obj_slots_len: usize,
    obj_slots_heap: Vec<u64>,
    arr_slots_stack: [u16; 16],
    arr_slots_len: usize,
    arr_slots_heap: Vec<u16>,
    depth: usize,
}

impl<'a, 'b> P3SinglePassEncoder<'a, 'b> {
    #[inline(always)]
    fn push_obj_slot(&mut self, ktag: u32, key_rel: u16, val_rel: u16) {
        let packed = u64::from(ktag) | (u64::from(key_rel) << 32) | (u64::from(val_rel) << 48);
        if self.obj_slots_len < 16 {
            self.obj_slots_stack[self.obj_slots_len] = packed;
        } else {
            self.obj_slots_heap.push(packed);
        }
        self.obj_slots_len += 1;
    }

    #[inline(always)]
    fn flush_obj_slots_compact(&mut self, mark: usize) {
        let end = self.obj_slots_len;
        let stack_start = mark.min(16);
        let stack_end = end.min(16);
        for &packed in &self.obj_slots_stack[stack_start..stack_end] {
            let kinfo = (((packed & 0x3f) as u8) << 2) | (((packed >> 8) as u8) & 0x03);
            let val_rel = (packed >> 48) as u8;
            self.out.extend_from_slice(&[kinfo, val_rel]);
        }
        if end > 16 {
            let heap_start = mark.saturating_sub(16);
            let heap_end = end - 16;
            for &packed in &self.obj_slots_heap[heap_start..heap_end] {
                let kinfo = (((packed & 0x3f) as u8) << 2) | (((packed >> 8) as u8) & 0x03);
                let val_rel = (packed >> 48) as u8;
                self.out.extend_from_slice(&[kinfo, val_rel]);
            }
            self.obj_slots_heap.truncate(heap_start);
        }
        self.obj_slots_len = mark;
    }

    #[inline(never)]
    fn flush_obj_slots_wide(&mut self, mark: usize, rel_delta: u16) {
        let end = self.obj_slots_len;
        let stack_start = mark.min(16);
        let stack_end = end.min(16);
        for &packed in &self.obj_slots_stack[stack_start..stack_end] {
            let tb = (packed as u32).to_le_bytes();
            let kb = (((packed >> 32) as u16) + rel_delta).to_le_bytes();
            let vb = (((packed >> 48) as u16) + rel_delta).to_le_bytes();
            self.out
                .extend_from_slice(&[tb[0], tb[1], tb[2], tb[3], kb[0], kb[1], vb[0], vb[1]]);
        }
        if end > 16 {
            let heap_start = mark.saturating_sub(16);
            let heap_end = end - 16;
            for &packed in &self.obj_slots_heap[heap_start..heap_end] {
                let tb = (packed as u32).to_le_bytes();
                let kb = (((packed >> 32) as u16) + rel_delta).to_le_bytes();
                let vb = (((packed >> 48) as u16) + rel_delta).to_le_bytes();
                self.out
                    .extend_from_slice(&[tb[0], tb[1], tb[2], tb[3], kb[0], kb[1], vb[0], vb[1]]);
            }
            self.obj_slots_heap.truncate(heap_start);
        }
        self.obj_slots_len = mark;
    }

    #[inline(always)]
    fn push_arr_slot(&mut self, rel: u16) {
        if self.arr_slots_len < 16 {
            self.arr_slots_stack[self.arr_slots_len] = rel;
        } else {
            self.arr_slots_heap.push(rel);
        }
        self.arr_slots_len += 1;
    }

    #[inline(always)]
    fn flush_arr_slots(&mut self, mark: usize) {
        let end = self.arr_slots_len;
        let stack_start = mark.min(16);
        let stack_end = end.min(16);
        for &rel in &self.arr_slots_stack[stack_start..stack_end] {
            self.out.extend_from_slice(&rel.to_le_bytes());
        }
        if end > 16 {
            let heap_start = mark.saturating_sub(16);
            let heap_end = end - 16;
            for &rel in &self.arr_slots_heap[heap_start..heap_end] {
                self.out.extend_from_slice(&rel.to_le_bytes());
            }
            self.arr_slots_heap.truncate(heap_start);
        }
        self.arr_slots_len = mark;
    }

    #[inline(always)]
    fn parse_string_with_tag(&mut self, i: usize, compute_tag: bool) -> Option<(usize, u32)> {
        let bytes = self.bytes;
        let start = i + 1;
        let run = sqliters_simd::scan_json_plain_ascii_string(&bytes[start..]);
        let end_q = start + run;
        if end_q < bytes.len() && bytes[end_q] == b'"' {
            if run > 56_000 || self.out.len() + 3 + run > 60_000 {
                return None;
            }
            let slice = &bytes[start..end_q];
            if run <= 127 {
                self.out.push(P3J_STR_SHORT_BASE | (run as u8));
            } else {
                let len_b = (run as u16).to_le_bytes();
                self.out
                    .extend_from_slice(&[P3J_STR_PLAIN, len_b[0], len_b[1]]);
            }
            self.out.extend_from_slice(slice);
            let tag = if compute_tag {
                p3v2_key_tag(slice, true)
            } else {
                0
            };
            return Some((end_q + 1, tag));
        }
        self.parse_string_slow(start, end_q, compute_tag)
    }

    #[inline(never)]
    fn parse_string_slow(
        &mut self,
        start: usize,
        mut j: usize,
        compute_tag: bool,
    ) -> Option<(usize, u32)> {
        let bytes = self.bytes;
        let mut has_escape = false;
        while j < bytes.len() {
            let c = bytes[j];
            if c == b'"' {
                let slice = &bytes[start..j];
                if slice.len() > 56_000 || self.out.len() + 3 + slice.len() > 60_000 {
                    return None;
                }
                if !has_escape && slice.len() <= 127 {
                    self.out.push(P3J_STR_SHORT_BASE | (slice.len() as u8));
                } else {
                    let ntag = if has_escape {
                        P3J_STR_ESCAPED
                    } else {
                        P3J_STR_PLAIN
                    };
                    let len_b = (slice.len() as u16).to_le_bytes();
                    self.out.extend_from_slice(&[ntag, len_b[0], len_b[1]]);
                }
                self.out.extend_from_slice(slice);
                let ktag = if compute_tag {
                    p3v2_key_tag(slice, !has_escape)
                } else {
                    0
                };
                return Some((j + 1, ktag));
            } else if c == b'\\' {
                has_escape = true;
                j += 1;
                let &esc = bytes.get(j)?;
                if matches!(esc, b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') {
                    j += 1;
                } else if esc == b'u' {
                    if j + 4 >= bytes.len() || parse_4hex(&bytes[j + 1..j + 5]).is_none() {
                        return None;
                    }
                    j += 5;
                } else {
                    return None;
                }
            } else if c <= 0x1f {
                return None;
            } else if c < 0x80 {
                j += 1;
            } else {
                let ch_len = self.src[j..].chars().next().map_or(1, char::len_utf8);
                j += ch_len;
            }
        }
        None
    }

    #[inline(always)]
    fn parse_number(&mut self, i: usize) -> Option<usize> {
        let b = self.bytes;
        let neg = b[i] == b'-';
        let p = if neg { i + 1 } else { i };
        let &first = b.get(p)?;
        let mut acc = 0u64;
        let mut int_overflow = false;
        let mut j = if first == b'0' {
            if b.get(p + 1).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            p + 1
        } else if first.is_ascii_digit() {
            acc = u64::from(first - b'0');
            let mut cur = p + 1;
            while cur < b.len() && b[cur].is_ascii_digit() {
                let d = u64::from(b[cur] - b'0');
                if !int_overflow {
                    match acc.checked_mul(10).and_then(|v| v.checked_add(d)) {
                        Some(next) => acc = next,
                        None => int_overflow = true,
                    }
                }
                cur += 1;
            }
            cur
        } else {
            return None;
        };

        let mut is_float = false;
        if b.get(j) == Some(&b'.') {
            j += 1;
            if !b.get(j).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            is_float = true;
            j += 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        }
        if matches!(b.get(j), Some(b'e' | b'E')) {
            is_float = true;
            j += 1;
            if matches!(b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if !b.get(j).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            j += 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        }
        if b.get(j)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_' || *c == b'.')
        {
            return None;
        }
        if !is_float {
            if int_overflow || (neg && acc == 0) {
                return None;
            }
            let val: i64 = if !neg {
                if acc > i64::MAX as u64 {
                    return None;
                }
                acc as i64
            } else if acc == (i64::MAX as u64) + 1 {
                i64::MIN
            } else if acc <= i64::MAX as u64 {
                -(acc as i64)
            } else {
                return None;
            };
            if self.out.len() + 9 > 60_000 {
                return None;
            }
            p3v2_append_int_node(self.out, val);
            return Some(j);
        }

        let raw = &b[i..j];
        if raw.len() > 4096 || self.out.len() + 11 + raw.len() > 60_000 {
            return None;
        }
        let fv = self.src[i..j].parse::<f64>().unwrap_or(if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
        let len_b = (raw.len() as u16).to_le_bytes();
        self.out.push(P3J_FLOAT_RAW);
        self.out.extend_from_slice(&fv.to_le_bytes());
        self.out.extend_from_slice(&len_b);
        self.out.extend_from_slice(raw);
        Some(j)
    }

    fn parse_value(&mut self, start: usize) -> Option<usize> {
        let i = skip_rfc8259_ws(self.bytes, start);
        let &c = self.bytes.get(i)?;
        match c {
            b'{' => {
                self.depth += 1;
                if self.depth > JSON_MAX_DEPTH {
                    return None;
                }
                let mut j = skip_rfc8259_ws(self.bytes, i + 1);
                if self.bytes.get(j) == Some(&b'}') {
                    self.depth -= 1;
                    if self.out.len() + 1 > 60_000 {
                        return None;
                    }
                    self.out.push(P3J_EMPTY_OBJECT);
                    return Some(j + 1);
                }
                if self.out.len() + 6 > 60_000 {
                    return None;
                }
                let obj_pos = self.out.len();
                self.out.extend_from_slice(&[P3J_OBJECT_COMPACT, 0, 0]);
                let slot_mark = self.obj_slots_len;
                let mut n_keys = 0usize;
                let mut all_compact_keys = true;
                loop {
                    if n_keys >= 255 || self.bytes.get(j) != Some(&b'"') {
                        return None;
                    }
                    let key_rel = self.out.len() - obj_pos;
                    if key_rel > 59_990 {
                        return None;
                    }
                    let (after_k, ktag) = self.parse_string_with_tag(j, true)?;
                    if (ktag & 0xff) > 63 {
                        all_compact_keys = false;
                    }
                    j = skip_rfc8259_ws(self.bytes, after_k);
                    if self.bytes.get(j) != Some(&b':') {
                        return None;
                    }
                    let val_rel = self.out.len() - obj_pos;
                    if val_rel > 59_990 {
                        return None;
                    }
                    j = self.parse_value(j + 1)?;
                    self.push_obj_slot(ktag, key_rel as u16, val_rel as u16);
                    n_keys += 1;

                    j = skip_rfc8259_ws(self.bytes, j);
                    match self.bytes.get(j) {
                        Some(&b',') => {
                            j = skip_rfc8259_ws(self.bytes, j + 1);
                            if self.bytes.get(j) == Some(&b'}') {
                                return None;
                            }
                        }
                        Some(&b'}') => {
                            self.depth -= 1;
                            let body_rel = self.out.len() - obj_pos;
                            if n_keys <= 31 && all_compact_keys && body_rel <= 255 {
                                if self.out.len() + 2 * n_keys > 60_000 {
                                    return None;
                                }
                                let dir_rel = body_rel as u8;
                                self.flush_obj_slots_compact(slot_mark);
                                self.out[obj_pos + 1] = n_keys as u8;
                                self.out[obj_pos + 2] = dir_rel;
                                return Some(j + 1);
                            }
                            if self.out.len() + 3 + 8 * n_keys > 60_000 {
                                return None;
                            }
                            let body_end = self.out.len();
                            self.out.extend_from_slice(&[0, 0, 0]);
                            self.out.copy_within(obj_pos + 3..body_end, obj_pos + 6);
                            let dir_rel = (self.out.len() - obj_pos) as u16;
                            self.flush_obj_slots_wide(slot_mark, 3);
                            let end_rel = (self.out.len() - obj_pos) as u16;
                            let db = dir_rel.to_le_bytes();
                            let eb = end_rel.to_le_bytes();
                            self.out[obj_pos] = P3J_OBJECT;
                            self.out[obj_pos + 1] = n_keys as u8;
                            self.out[obj_pos + 2] = db[0];
                            self.out[obj_pos + 3] = db[1];
                            self.out[obj_pos + 4] = eb[0];
                            self.out[obj_pos + 5] = eb[1];
                            return Some(j + 1);
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.depth += 1;
                if self.depth > JSON_MAX_DEPTH {
                    return None;
                }
                let mut j = skip_rfc8259_ws(self.bytes, i + 1);
                if self.bytes.get(j) == Some(&b']') {
                    self.depth -= 1;
                    if self.out.len() + 1 > 60_000 {
                        return None;
                    }
                    self.out.push(P3J_EMPTY_ARRAY);
                    return Some(j + 1);
                }
                if self.out.len() + 7 > 60_000 {
                    return None;
                }
                let arr_pos = self.out.len();
                self.out.extend_from_slice(&[P3J_ARRAY, 0, 0, 0, 0, 0, 0]);
                let slot_mark = self.arr_slots_len;
                let mut n_elems = 0usize;
                loop {
                    if n_elems >= 16_384 {
                        return None;
                    }
                    let elem_rel = self.out.len() - arr_pos;
                    if elem_rel > 60_000 {
                        return None;
                    }
                    j = self.parse_value(j)?;
                    self.push_arr_slot(elem_rel as u16);
                    n_elems += 1;

                    j = skip_rfc8259_ws(self.bytes, j);
                    match self.bytes.get(j) {
                        Some(&b',') => {
                            j = skip_rfc8259_ws(self.bytes, j + 1);
                            if self.bytes.get(j) == Some(&b']') {
                                return None;
                            }
                        }
                        Some(&b']') => {
                            self.depth -= 1;
                            if self.out.len() + 2 * n_elems > 60_000 {
                                return None;
                            }
                            let dir_rel = (self.out.len() - arr_pos) as u16;
                            self.flush_arr_slots(slot_mark);
                            let end_rel = (self.out.len() - arr_pos) as u16;
                            let nb = (n_elems as u16).to_le_bytes();
                            let db = dir_rel.to_le_bytes();
                            let eb = end_rel.to_le_bytes();
                            self.out[arr_pos + 1] = nb[0];
                            self.out[arr_pos + 2] = nb[1];
                            self.out[arr_pos + 3] = db[0];
                            self.out[arr_pos + 4] = db[1];
                            self.out[arr_pos + 5] = eb[0];
                            self.out[arr_pos + 6] = eb[1];
                            return Some(j + 1);
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self
                .parse_string_with_tag(i, false)
                .map(|(next_i, _)| next_i),
            b't' => {
                if self.bytes[i..].starts_with(b"true")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                    && self.out.len() < 60_000
                {
                    self.out.push(P3J_TRUE);
                    Some(i + 4)
                } else {
                    None
                }
            }
            b'f' => {
                if self.bytes[i..].starts_with(b"false")
                    && !self.bytes.get(i + 5).is_some_and(u8::is_ascii_alphanumeric)
                    && self.out.len() < 60_000
                {
                    self.out.push(P3J_FALSE);
                    Some(i + 5)
                } else {
                    None
                }
            }
            b'n' => {
                if self.bytes[i..].starts_with(b"null")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                    && self.out.len() < 60_000
                {
                    self.out.push(P3J_NULL);
                    Some(i + 4)
                } else {
                    None
                }
            }
            b'-' | b'0'..=b'9' => self.parse_number(i),
            _ => None,
        }
    }
}

struct P3ShortSliceEncoder<'a, 'b> {
    src: &'a str,
    bytes: &'a [u8],
    buf: &'b mut [u8],
    pos: usize,
    obj_slots: [u16; 24],
    obj_len: usize,
    depth: u8,
}

impl<'a, 'b> P3ShortSliceEncoder<'a, 'b> {
    #[inline(always)]
    fn parse_plain_string(&mut self, i: usize, is_key: bool) -> Option<(usize, u8)> {
        let bytes = self.bytes;
        let start = i + 1;
        let run = sqliters_simd::scan_json_plain_ascii_string(&bytes[start..]);
        let end_q = start + run;
        if end_q >= bytes.len() || bytes[end_q] != b'"' {
            return None;
        }
        let max_len = if is_key { 63 } else { 127 };
        if run > max_len || self.pos + 1 + run > self.buf.len() {
            return None;
        }
        let slice = &bytes[start..end_q];
        self.buf[self.pos] = P3J_STR_SHORT_BASE | (run as u8);
        self.buf[self.pos + 1..self.pos + 1 + run].copy_from_slice(slice);
        self.pos += 1 + run;
        let kinfo = if is_key {
            ((run as u8) << 2) | (slice.first().copied().unwrap_or(0) & 0x03)
        } else {
            0
        };
        Some((end_q + 1, kinfo))
    }

    #[inline(always)]
    fn parse_number(&mut self, i: usize) -> Option<usize> {
        let b = self.bytes;
        let neg = b[i] == b'-';
        let p = if neg { i + 1 } else { i };
        let &first = b.get(p)?;
        let mut acc = 0u64;
        let mut int_overflow = false;
        let mut j = if first == b'0' {
            if b.get(p + 1).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            p + 1
        } else if first.is_ascii_digit() {
            acc = u64::from(first - b'0');
            let mut cur = p + 1;
            while cur < b.len() && b[cur].is_ascii_digit() {
                let d = u64::from(b[cur] - b'0');
                if !int_overflow {
                    match acc.checked_mul(10).and_then(|v| v.checked_add(d)) {
                        Some(next) => acc = next,
                        None => int_overflow = true,
                    }
                }
                cur += 1;
            }
            cur
        } else {
            return None;
        };

        let mut is_float = false;
        if b.get(j) == Some(&b'.') {
            j += 1;
            if !b.get(j).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            is_float = true;
            j += 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        }
        if matches!(b.get(j), Some(b'e' | b'E')) {
            is_float = true;
            j += 1;
            if matches!(b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if !b.get(j).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            j += 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
        }
        if b.get(j)
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_' || *c == b'.')
        {
            return None;
        }
        if !is_float {
            if int_overflow || (neg && acc == 0) {
                return None;
            }
            let val: i64 = if !neg {
                if acc > i64::MAX as u64 {
                    return None;
                }
                acc as i64
            } else if acc == (i64::MAX as u64) + 1 {
                i64::MIN
            } else if acc <= i64::MAX as u64 {
                -(acc as i64)
            } else {
                return None;
            };
            if self.pos + 9 > self.buf.len() {
                return None;
            }
            if let Ok(v8) = i8::try_from(val) {
                self.buf[self.pos] = P3J_INT_I8;
                self.buf[self.pos + 1] = v8 as u8;
                self.pos += 2;
            } else if let Ok(v16) = i16::try_from(val) {
                let le = v16.to_le_bytes();
                self.buf[self.pos] = P3J_INT_I16;
                self.buf[self.pos + 1] = le[0];
                self.buf[self.pos + 2] = le[1];
                self.pos += 3;
            } else if let Ok(v32) = i32::try_from(val) {
                let le = v32.to_le_bytes();
                self.buf[self.pos] = P3J_INT_I32;
                self.buf[self.pos + 1..self.pos + 5].copy_from_slice(&le);
                self.pos += 5;
            } else {
                self.buf[self.pos] = P3J_INT_I64;
                self.buf[self.pos + 1..self.pos + 9].copy_from_slice(&val.to_le_bytes());
                self.pos += 9;
            }
            return Some(j);
        }

        let raw = &b[i..j];
        if self.pos + 11 + raw.len() > self.buf.len() {
            return None;
        }
        let fv = self.src[i..j].parse::<f64>().unwrap_or(if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
        let len_b = (raw.len() as u16).to_le_bytes();
        self.buf[self.pos] = P3J_FLOAT_RAW;
        self.buf[self.pos + 1..self.pos + 9].copy_from_slice(&fv.to_le_bytes());
        self.buf[self.pos + 9] = len_b[0];
        self.buf[self.pos + 10] = len_b[1];
        self.buf[self.pos + 11..self.pos + 11 + raw.len()].copy_from_slice(raw);
        self.pos += 11 + raw.len();
        Some(j)
    }

    fn parse_value(&mut self, start: usize) -> Option<usize> {
        let i = skip_rfc8259_ws(self.bytes, start);
        let &c = self.bytes.get(i)?;
        match c {
            b'{' => {
                self.depth += 1;
                if self.depth > 12 {
                    return None;
                }
                let mut j = skip_rfc8259_ws(self.bytes, i + 1);
                if self.bytes.get(j) == Some(&b'}') {
                    self.depth -= 1;
                    if self.pos + 1 > self.buf.len() {
                        return None;
                    }
                    self.buf[self.pos] = P3J_EMPTY_OBJECT;
                    self.pos += 1;
                    return Some(j + 1);
                }
                if self.pos + 3 > self.buf.len() {
                    return None;
                }
                let obj_pos = self.pos;
                self.buf[obj_pos] = P3J_OBJECT_COMPACT;
                self.pos += 3;
                let slot_mark = self.obj_len;
                let mut n_keys = 0usize;
                loop {
                    if n_keys >= 31 || self.obj_len >= 24 || self.bytes.get(j) != Some(&b'"') {
                        return None;
                    }
                    let (after_k, kinfo) = self.parse_plain_string(j, true)?;
                    j = skip_rfc8259_ws(self.bytes, after_k);
                    if self.bytes.get(j) != Some(&b':') {
                        return None;
                    }
                    let val_rel = self.pos - obj_pos;
                    if val_rel > 255 {
                        return None;
                    }
                    self.obj_slots[self.obj_len] = u16::from(kinfo) | ((val_rel as u16) << 8);
                    self.obj_len += 1;
                    j = self.parse_value(j + 1)?;
                    n_keys += 1;

                    j = skip_rfc8259_ws(self.bytes, j);
                    match self.bytes.get(j) {
                        Some(&b',') => {
                            j = skip_rfc8259_ws(self.bytes, j + 1);
                            if self.bytes.get(j) == Some(&b'}') {
                                return None;
                            }
                        }
                        Some(&b'}') => {
                            self.depth -= 1;
                            let body_rel = self.pos - obj_pos;
                            if body_rel > 255 || self.pos + 2 * n_keys > self.buf.len() {
                                return None;
                            }
                            self.buf[obj_pos + 1] = n_keys as u8;
                            self.buf[obj_pos + 2] = body_rel as u8;
                            for k in slot_mark..self.obj_len {
                                let sb = self.obj_slots[k].to_le_bytes();
                                self.buf[self.pos] = sb[0];
                                self.buf[self.pos + 1] = sb[1];
                                self.pos += 2;
                            }
                            self.obj_len = slot_mark;
                            return Some(j + 1);
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self.parse_plain_string(i, false).map(|(next_i, _)| next_i),
            b't' => {
                if self.bytes[i..].starts_with(b"true")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                    && self.pos < self.buf.len()
                {
                    self.buf[self.pos] = P3J_TRUE;
                    self.pos += 1;
                    Some(i + 4)
                } else {
                    None
                }
            }
            b'f' => {
                if self.bytes[i..].starts_with(b"false")
                    && !self.bytes.get(i + 5).is_some_and(u8::is_ascii_alphanumeric)
                    && self.pos < self.buf.len()
                {
                    self.buf[self.pos] = P3J_FALSE;
                    self.pos += 1;
                    Some(i + 5)
                } else {
                    None
                }
            }
            b'n' => {
                if self.bytes[i..].starts_with(b"null")
                    && !self.bytes.get(i + 4).is_some_and(u8::is_ascii_alphanumeric)
                    && self.pos < self.buf.len()
                {
                    self.buf[self.pos] = P3J_NULL;
                    self.pos += 1;
                    Some(i + 4)
                } else {
                    None
                }
            }
            b'-' | b'0'..=b'9' => self.parse_number(i),
            _ => None,
        }
    }
}

#[inline]
fn try_encode_p3v2_jsonb_single_pass(src: &str, out: &mut Vec<u8>) -> bool {
    if crate::legacy_format3_mode() {
        return false;
    }
    let bytes = src.as_bytes();
    if bytes.len() < 2 || bytes.len() > 54_000 {
        return false;
    }
    let i0 = skip_rfc8259_ws(bytes, 0);
    if i0 >= bytes.len() || (bytes[i0] != b'{' && bytes[i0] != b'[') {
        return false;
    }
    let i1 = skip_rfc8259_ws(bytes, i0 + 1);
    if i1 >= bytes.len()
        || (bytes[i0] == b'{' && bytes[i1] == b'}')
        || (bytes[i0] == b'[' && bytes[i1] == b']')
    {
        return false;
    }
    if bytes[i0] == b'{' && bytes.len() <= 384 {
        let cap = bytes.len() * 4 + 32;
        if out.len() < cap {
            out.resize(cap, 0);
        }
        let mut short_enc = P3ShortSliceEncoder {
            src,
            bytes,
            buf: &mut out[..cap],
            pos: 4,
            obj_slots: [0u16; 24],
            obj_len: 0,
            depth: 0,
        };
        if let Some(end) = short_enc.parse_value(i0) {
            let tail = skip_rfc8259_ws(bytes, end);
            let final_len = short_enc.pos;
            if tail == bytes.len() {
                let tb = (final_len as u16).to_le_bytes();
                out[0] = P3V2_JSONB_MAGIC0;
                out[1] = P3V2_JSONB_VER1;
                out[2] = tb[0];
                out[3] = tb[1];
                out.truncate(final_len);
                return true;
            }
        }
    }
    out.clear();
    out.reserve(bytes.len().saturating_add(16));
    out.extend_from_slice(&[P3V2_JSONB_MAGIC0, P3V2_JSONB_VER1, 0, 0]);
    let mut enc = P3SinglePassEncoder {
        src,
        bytes,
        out,
        obj_slots_stack: [0u64; 16],
        obj_slots_len: 0,
        obj_slots_heap: Vec::new(),
        arr_slots_stack: [0u16; 16],
        arr_slots_len: 0,
        arr_slots_heap: Vec::new(),
        depth: 0,
    };
    if let Some(end) = enc.parse_value(i0) {
        let tail = skip_rfc8259_ws(bytes, end);
        if tail == bytes.len() && enc.out.len() <= 60_000 {
            let tb = (enc.out.len() as u16).to_le_bytes();
            enc.out[2] = tb[0];
            enc.out[3] = tb[1];
            return true;
        }
    }
    out.clear();
    false
}

/// Transcodes a `P3v2` Binary `JSONB` node at `blob[pos]` into canonical Format-3 `JSONB` bytes.
fn transcode_p3v2_node_to_format3(
    blob: &[u8],
    pos: usize,
    depth: usize,
    out: &mut Vec<u8>,
) -> Result<usize> {
    if depth > JSON_MAX_DEPTH {
        return Err(err_too_deep());
    }
    let Some(node_len) = p3v2_node_len(blob, pos) else {
        return Err(err_malformed());
    };
    let end = pos + node_len;
    match blob[pos] {
        P3J_NULL => jsonb_append_node(out, JSONB_NULL, &[]),
        P3J_FALSE => jsonb_append_node(out, JSONB_FALSE, &[]),
        P3J_TRUE => jsonb_append_node(out, JSONB_TRUE, &[]),
        P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => {
            let i = p3v2_read_int(blob, pos).ok_or_else(err_malformed)?;
            let mut buf = [0u8; 24];
            let s = format_i64_json(i, &mut buf);
            jsonb_append_node(out, JSONB_INT, s.as_bytes());
        }
        P3J_FLOAT_RAW => {
            if p3v2_node_validity_check(blob, pos, depth).0 != 0 {
                return Err(err_malformed());
            }
            jsonb_append_node(out, JSONB_FLOAT, &blob[pos + 11..end]);
        }
        t if t >= P3J_STR_SHORT_BASE || t == P3J_STR_PLAIN => {
            if p3v2_node_validity_check(blob, pos, depth).0 != 0 {
                return Err(err_malformed());
            }
            let hlen = if t >= P3J_STR_SHORT_BASE { 1 } else { 3 };
            jsonb_append_node(out, JSONB_TEXT, &blob[pos + hlen..end]);
        }
        P3J_STR_ESCAPED => {
            if p3v2_node_validity_check(blob, pos, depth).0 != 0 {
                return Err(err_malformed());
            }
            jsonb_append_node(out, JSONB_TEXTJ, &blob[pos + 3..end]);
        }
        P3J_STR_RAW => {
            if p3v2_node_validity_check(blob, pos, depth).0 != 0 {
                return Err(err_malformed());
            }
            jsonb_append_node(out, JSONB_TEXTRAW, &blob[pos + 3..end]);
        }
        P3J_EMPTY_ARRAY => jsonb_append_node(out, JSONB_ARRAY, &[]),
        P3J_EMPTY_OBJECT => jsonb_append_node(out, JSONB_OBJECT, &[]),
        P3J_OBJECT_COMPACT => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(blob[pos + 2]);
            let dir_abs = pos + dir_rel;
            let hdr_pos = out.len();
            out.extend_from_slice(&[0, 0]);
            let mut cursor = pos + 3;
            for m in 0..n_keys {
                let s = dir_abs + 2 * m;
                let kinfo = blob[s];
                let val_rel = usize::from(blob[s + 1]);
                let klen = usize::from(kinfo >> 2);
                let val_pos = pos + val_rel;
                if val_pos < cursor + 1 + klen || val_pos >= dir_abs {
                    return Err(err_malformed());
                }
                let key_pos = val_pos - 1 - klen;
                if key_pos != cursor || blob[key_pos] != (P3J_STR_SHORT_BASE | (klen as u8)) {
                    return Err(err_malformed());
                }
                let kend = transcode_p3v2_node_to_format3(blob, key_pos, depth + 1, out)?;
                if kend != val_pos {
                    return Err(err_malformed());
                }
                cursor = transcode_p3v2_node_to_format3(blob, val_pos, depth + 1, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            finish_container_in_place(out, hdr_pos, JSONB_OBJECT);
        }
        P3J_OBJECT => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 2], blob[pos + 3]]));
            let dir_abs = pos + dir_rel;
            let hdr_pos = out.len();
            out.extend_from_slice(&[0, 0]);
            let mut cursor = pos + 6;
            for m in 0..n_keys {
                let s = dir_abs + 8 * m;
                let key_rel = usize::from(u16::from_le_bytes([blob[s + 4], blob[s + 5]]));
                let val_rel = usize::from(u16::from_le_bytes([blob[s + 6], blob[s + 7]]));
                if pos + key_rel != cursor {
                    return Err(err_malformed());
                }
                if p3v2_str_header(blob, pos + key_rel).is_none() {
                    return Err(err_malformed());
                }
                let kend = transcode_p3v2_node_to_format3(blob, pos + key_rel, depth + 1, out)?;
                if kend != pos + val_rel {
                    return Err(err_malformed());
                }
                cursor = transcode_p3v2_node_to_format3(blob, pos + val_rel, depth + 1, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            finish_container_in_place(out, hdr_pos, JSONB_OBJECT);
        }
        P3J_ARRAY => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_elems = usize::from(u16::from_le_bytes([blob[pos + 1], blob[pos + 2]]));
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 3], blob[pos + 4]]));
            let dir_abs = pos + dir_rel;
            let hdr_pos = out.len();
            out.extend_from_slice(&[0, 0]);
            let mut cursor = pos + 7;
            for m in 0..n_elems {
                let s = dir_abs + 2 * m;
                let elem_rel = usize::from(u16::from_le_bytes([blob[s], blob[s + 1]]));
                if pos + elem_rel != cursor {
                    return Err(err_malformed());
                }
                cursor = transcode_p3v2_node_to_format3(blob, pos + elem_rel, depth + 1, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            finish_container_in_place(out, hdr_pos, JSONB_ARRAY);
        }
        _ => return Err(err_malformed()),
    }
    Ok(end)
}

fn transcode_p3v2_jsonb_to_format3(blob: &[u8], out: &mut Vec<u8>) -> Result<()> {
    if !is_p3v2_jsonb(blob) {
        return Err(err_malformed());
    }
    out.reserve(blob.len());
    let end = transcode_p3v2_node_to_format3(blob, 4, 0, out)?;
    if end != blob.len() {
        return Err(err_malformed());
    }
    Ok(())
}

/// Converts a validated non-empty Format-3 `JSONB` root container (`len <= 48_000`) into `P3v2`
/// Binary `JSONB` when all containers fit `P3v2` directory bounds; returns `None` to keep `f3` as-is.
fn try_transcode_format3_to_p3v2(f3: &[u8]) -> Option<Vec<u8>> {
    if crate::legacy_format3_mode() || f3.len() <= 1 || f3.len() > 48_000 {
        return None;
    }
    if !matches!(f3[0] & 0x0f, JSONB_OBJECT | JSONB_ARRAY) {
        return None;
    }
    fn convert_node(f3: &[u8], offset: usize, depth: usize, out: &mut Vec<u8>) -> Option<usize> {
        if depth > JSON_MAX_DEPTH {
            return None;
        }
        let (n, sz) = jsonb_payload_size(f3, offset)?;
        let end = offset + n + sz;
        let payload = &f3[offset + n..end];
        match f3[offset] & 0x0f {
            JSONB_NULL => {
                if sz != 0 || out.len() + 1 > 60_000 {
                    return None;
                }
                out.push(P3J_NULL);
            }
            JSONB_TRUE => {
                if sz != 0 || out.len() + 1 > 60_000 {
                    return None;
                }
                out.push(P3J_TRUE);
            }
            JSONB_FALSE => {
                if sz != 0 || out.len() + 1 > 60_000 {
                    return None;
                }
                out.push(P3J_FALSE);
            }
            JSONB_INT => {
                if payload == b"-0" || out.len() + 9 > 60_000 {
                    return None;
                }
                let val = parse_jsonb_int_i64(payload).ok()??;
                p3v2_append_int_node(out, val);
            }
            JSONB_FLOAT => {
                if payload.len() > 4096 || out.len() + 11 + payload.len() > 60_000 {
                    return None;
                }
                let s = std::str::from_utf8(payload).ok()?;
                let fv = s.parse::<f64>().unwrap_or_else(|_| {
                    if s.starts_with('-') {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    }
                });
                let lb = (payload.len() as u16).to_le_bytes();
                out.push(P3J_FLOAT_RAW);
                out.extend_from_slice(&fv.to_le_bytes());
                out.extend_from_slice(&lb);
                out.extend_from_slice(payload);
            }
            JSONB_TEXT | JSONB_TEXTJ | JSONB_TEXTRAW => {
                if payload.len() > 56_000 || out.len() + 3 + payload.len() > 60_000 {
                    return None;
                }
                let ptag = match f3[offset] & 0x0f {
                    JSONB_TEXT => P3J_STR_PLAIN,
                    JSONB_TEXTJ => P3J_STR_ESCAPED,
                    _ => P3J_STR_RAW,
                };
                let lb = (payload.len() as u16).to_le_bytes();
                out.extend_from_slice(&[ptag, lb[0], lb[1]]);
                out.extend_from_slice(payload);
            }
            JSONB_ARRAY => {
                if sz == 0 {
                    if out.len() + 1 > 60_000 {
                        return None;
                    }
                    out.push(P3J_EMPTY_ARRAY);
                } else {
                    if out.len() + 7 > 60_000 {
                        return None;
                    }
                    let arr_pos = out.len();
                    out.extend_from_slice(&[P3J_ARRAY, 0, 0, 0, 0, 0, 0]);
                    let mut rels = Vec::new();
                    let mut j = offset + n;
                    while j < end {
                        if rels.len() >= 16_384 {
                            return None;
                        }
                        let elem_rel = out.len() - arr_pos;
                        if elem_rel > 60_000 {
                            return None;
                        }
                        j = convert_node(&f3[..end], j, depth + 1, out)?;
                        rels.push(elem_rel as u16);
                    }
                    if out.len() + 2 * rels.len() > 60_000 {
                        return None;
                    }
                    let dir_rel = (out.len() - arr_pos) as u16;
                    for r in &rels {
                        out.extend_from_slice(&r.to_le_bytes());
                    }
                    let end_rel = (out.len() - arr_pos) as u16;
                    let nb = (rels.len() as u16).to_le_bytes();
                    let db = dir_rel.to_le_bytes();
                    let eb = end_rel.to_le_bytes();
                    out[arr_pos + 1] = nb[0];
                    out[arr_pos + 2] = nb[1];
                    out[arr_pos + 3] = db[0];
                    out[arr_pos + 4] = db[1];
                    out[arr_pos + 5] = eb[0];
                    out[arr_pos + 6] = eb[1];
                }
            }
            JSONB_OBJECT => {
                if sz == 0 {
                    if out.len() + 1 > 60_000 {
                        return None;
                    }
                    out.push(P3J_EMPTY_OBJECT);
                } else {
                    if out.len() + 6 > 60_000 {
                        return None;
                    }
                    let obj_pos = out.len();
                    out.extend_from_slice(&[P3J_OBJECT, 0, 0, 0, 0, 0]);
                    let mut slots: Vec<[u8; 8]> = Vec::new();
                    let mut j = offset + n;
                    while j < end {
                        if slots.len() >= 255 {
                            return None;
                        }
                        let ket = f3[j] & 0x0f;
                        if !matches!(ket, JSONB_TEXT | JSONB_TEXTJ | JSONB_TEXTRAW) {
                            return None;
                        }
                        let (kn, ksz) = jsonb_payload_size(&f3[..end], j)?;
                        let kbytes = &f3[j + kn..j + kn + ksz];
                        let ktag = p3v2_key_tag(kbytes, ket != JSONB_TEXTJ);
                        let key_rel = out.len() - obj_pos;
                        if key_rel > 60_000 {
                            return None;
                        }
                        let after_k = convert_node(&f3[..end], j, depth + 1, out)?;
                        if after_k >= end {
                            return None;
                        }
                        let val_rel = out.len() - obj_pos;
                        if val_rel > 60_000 {
                            return None;
                        }
                        j = convert_node(&f3[..end], after_k, depth + 1, out)?;
                        let tb = ktag.to_le_bytes();
                        let kb = (key_rel as u16).to_le_bytes();
                        let vb = (val_rel as u16).to_le_bytes();
                        slots.push([tb[0], tb[1], tb[2], tb[3], kb[0], kb[1], vb[0], vb[1]]);
                    }
                    if out.len() + 8 * slots.len() > 60_000 {
                        return None;
                    }
                    let dir_rel = (out.len() - obj_pos) as u16;
                    for slot in &slots {
                        out.extend_from_slice(slot);
                    }
                    let end_rel = (out.len() - obj_pos) as u16;
                    let db = dir_rel.to_le_bytes();
                    let eb = end_rel.to_le_bytes();
                    out[obj_pos + 1] = slots.len() as u8;
                    out[obj_pos + 2] = db[0];
                    out[obj_pos + 3] = db[1];
                    out[obj_pos + 4] = eb[0];
                    out[obj_pos + 5] = eb[1];
                }
            }
            _ => return None,
        }
        Some(end)
    }

    let mut out = Vec::with_capacity(f3.len().saturating_mul(2).saturating_add(16));
    out.extend_from_slice(&[P3V2_JSONB_MAGIC0, P3V2_JSONB_VER1, 0, 0]);
    let end = convert_node(f3, 0, 0, &mut out)?;
    if end != f3.len() || out.len() > 60_000 {
        return None;
    }
    let tb = (out.len() as u16).to_le_bytes();
    out[2] = tb[0];
    out[3] = tb[1];
    Some(out)
}

/// Parses JSON or JSON5 text into `out` as `P3v2` Binary `JSONB` (or Format-3 fallback for scalars,
/// empty containers, JSON5 non-standard syntax, or `>= 54 KiB` documents), returning `Ok(has_nonstd)`.
pub fn parse_json_text_to_jsonb_into(
    src: &str,
    out: &mut Vec<u8>,
) -> std::result::Result<bool, JsonTextParseError> {
    if try_encode_p3v2_jsonb_single_pass(src, out) {
        return Ok(false);
    }
    if try_parse_json_stage1_simd(src, out) {
        return Ok(false);
    }
    let mut parser = TextParser::new(src, out);
    let end = parser.parse_value(0)?;
    let tail = parser.skip_ws_and_comments(end)?;
    if tail != parser.bytes.len() {
        return Err(parser.err(tail));
    }
    let has_nonstd = parser.has_nonstd;
    if let Some(p3) = try_transcode_format3_to_p3v2(out) {
        *out = p3;
    }
    Ok(has_nonstd)
}

/// Parses JSON or JSON5 text into canonical SQLite 3.54.1 JSONB bytes.
pub fn parse_json_text_to_jsonb(
    src: &str,
) -> std::result::Result<JsonTextParseOutcome, JsonTextParseError> {
    let mut jsonb = Vec::with_capacity(src.len().saturating_add(16));
    let has_nonstd = parse_json_text_to_jsonb_into(src, &mut jsonb)?;
    Ok(JsonTextParseOutcome { jsonb, has_nonstd })
}

/// Evaluates SQLite `jsonb(val)` directly into `out`, returning `Ok(true)` when `out` holds the
/// validated JSONB bytes or `Ok(false)` when `val` is `Value::Null`.
pub fn eval_jsonb_value_into(val: &Value, out: &mut Vec<u8>) -> Result<bool> {
    out.clear();
    match val {
        Value::Null => Ok(false),
        Value::Text(s) => match parse_json_text_to_jsonb_into(s, out) {
            Ok(_) => Ok(true),
            Err(e) if e.too_deep => Err(err_too_deep()),
            Err(_) => Err(err_malformed()),
        },
        Value::Blob(b) => {
            if blob_is_jsonb(b) {
                validate_jsonb_root(b)?;
                out.extend_from_slice(b);
                Ok(true)
            } else {
                let Ok(s) = std::str::from_utf8(b) else {
                    return Err(err_malformed());
                };
                match parse_json_text_to_jsonb_into(s, out) {
                    Ok(_) => Ok(true),
                    Err(e) if e.too_deep => Err(err_too_deep()),
                    Err(_) => Err(err_malformed()),
                }
            }
        }
        Value::Integer(i) => {
            let mut buf = [0u8; 24];
            let mut pos = buf.len();
            let mut u = i.unsigned_abs();
            loop {
                pos -= 1;
                buf[pos] = b'0' + (u % 10) as u8;
                u /= 10;
                if u == 0 {
                    break;
                }
            }
            if *i < 0 {
                pos -= 1;
                buf[pos] = b'-';
            }
            jsonb_append_node(out, JSONB_INT, &buf[pos..]);
            Ok(true)
        }
        Value::Real(f) => {
            if f.is_nan() {
                out.push(JSONB_NULL);
            } else if f.is_infinite() {
                let s = if f.is_sign_positive() {
                    b"9e999".as_slice()
                } else {
                    b"-9e999".as_slice()
                };
                jsonb_append_node(out, JSONB_FLOAT, s);
            } else {
                let s = format_sqlite_real(*f);
                jsonb_append_node(out, JSONB_FLOAT, s.as_bytes());
            }
            Ok(true)
        }
    }
}

/// Converts a SQLite `Value` (either JSON/JSON5 `TEXT`, numeric coerced to text, or JSONB `BLOB`)
/// into a validated JSONB buffer. Returns `Ok(None)` when `val` is `Value::Null`.
pub fn parse_func_arg_to_jsonb(val: &Value) -> Result<Option<Vec<u8>>> {
    match val {
        Value::Null => Ok(None),
        Value::Blob(b) => {
            if blob_is_jsonb(b) {
                Ok(Some(b.clone()))
            } else {
                // `json.c` falls back to interpreting non-JSONB BLOBs as UTF-8 text if valid JSON!
                let Ok(s) = std::str::from_utf8(b) else {
                    return Err(err_malformed());
                };
                match parse_json_text_to_jsonb(s) {
                    Ok(out) => Ok(Some(out.jsonb)),
                    Err(e) if e.too_deep => Err(err_too_deep()),
                    Err(_) => Err(err_malformed()),
                }
            }
        }
        Value::Text(s) => match parse_json_text_to_jsonb(s) {
            Ok(out) => Ok(Some(out.jsonb)),
            Err(e) if e.too_deep => Err(err_too_deep()),
            Err(_) => Err(err_malformed()),
        },
        Value::Integer(i) => {
            let s = i.to_string();
            let mut out = Vec::with_capacity(s.len() + 2);
            jsonb_append_node(&mut out, JSONB_INT, s.as_bytes());
            Ok(Some(out))
        }
        Value::Real(f) => {
            if f.is_nan() {
                Ok(Some(vec![JSONB_NULL]))
            } else if f.is_infinite() {
                let s = if f.is_sign_positive() {
                    "9e999"
                } else {
                    "-9e999"
                };
                let mut out = Vec::with_capacity(s.len() + 2);
                jsonb_append_node(&mut out, JSONB_FLOAT, s.as_bytes());
                Ok(Some(out))
            } else {
                let s = format_sqlite_real(*f);
                let mut out = Vec::with_capacity(s.len() + 2);
                jsonb_append_node(&mut out, JSONB_FLOAT, s.as_bytes());
                Ok(Some(out))
            }
        }
    }
}

/// Appends a raw UTF-8 string as a canonical double-quoted RFC-8259 JSON string literal.
pub fn append_escaped_json_string(out: &mut String, s: &str) {
    out.push('"');
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let run = sqliters_simd::scan_json_plain_ascii_string(&bytes[pos..]);
        if run > 0 {
            out.push_str(&s[pos..pos + run]);
            pos += run;
            if pos >= bytes.len() {
                break;
            }
        }
        let ch = s[pos..].chars().next().expect("valid UTF-8 slice");
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) <= 0x1f => {
                let v = c as u32;
                out.push_str(&format!("\\u{v:04x}"));
            }
            c => out.push(c),
        }
        pos += ch.len_utf8();
    }
    out.push('"');
}

/// Decodes the string value of a `JSONB_TEXT*` node at `offset` into an unescaped Rust `String`.
pub fn jsonb_decode_string(blob: &[u8], offset: usize) -> Result<String> {
    let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
        return Err(err_malformed());
    };
    let etype = blob[offset] & 0x0f;
    let payload = &blob[offset + n..offset + n + sz];
    match etype {
        JSONB_TEXT | JSONB_TEXTRAW => std::str::from_utf8(payload)
            .map(str::to_owned)
            .map_err(|_| err_malformed()),
        JSONB_TEXTJ | JSONB_TEXT5 => {
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            let bytes = s.as_bytes();
            let mut out = String::with_capacity(sz);
            let mut k = 0;
            while k < bytes.len() {
                if bytes[k] == b'\\' {
                    let (consumed, cp) = json_unescape_one_char(&bytes[k..]);
                    if cp == JSON_INVALID_CHAR || consumed == 0 {
                        return Err(err_malformed());
                    }
                    if cp != u32::MAX
                        && let Some(ch) = char::from_u32(cp)
                    {
                        out.push(ch);
                    }
                    k += consumed;
                } else {
                    let ch = s[k..].chars().next().ok_or_else(err_malformed)?;
                    out.push(ch);
                    k += ch.len_utf8();
                }
            }
            Ok(out)
        }
        _ => Err(err_malformed()),
    }
}

fn p3v2_decode_string(blob: &[u8], pos: usize) -> Result<String> {
    let Some((ktag, hdr_len, slen)) = p3v2_str_header(blob, pos) else {
        return Err(err_malformed());
    };
    let payload = &blob[pos + hdr_len..pos + hdr_len + slen];
    match ktag {
        P3J_STR_PLAIN | P3J_STR_RAW => std::str::from_utf8(payload)
            .map(str::to_owned)
            .map_err(|_| err_malformed()),
        P3J_STR_ESCAPED => {
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            let bytes = s.as_bytes();
            let mut out = String::with_capacity(payload.len());
            let mut k = 0;
            while k < bytes.len() {
                if bytes[k] == b'\\' {
                    let (consumed, cp) = json_unescape_one_char(&bytes[k..]);
                    if cp == JSON_INVALID_CHAR || consumed == 0 {
                        return Err(err_malformed());
                    }
                    if cp != u32::MAX
                        && let Some(ch) = char::from_u32(cp)
                    {
                        out.push(ch);
                    }
                    k += consumed;
                } else {
                    let ch = s[k..].chars().next().ok_or_else(err_malformed)?;
                    out.push(ch);
                    k += ch.len_utf8();
                }
            }
            Ok(out)
        }
        _ => Err(err_malformed()),
    }
}

fn p3v2_node_to_text(blob: &[u8], pos: usize, depth: usize, out: &mut String) -> Result<usize> {
    if depth > JSON_MAX_DEPTH {
        return Err(err_too_deep());
    }
    let Some(node_len) = p3v2_node_len(blob, pos) else {
        return Err(err_malformed());
    };
    let end = pos + node_len;
    match blob[pos] {
        P3J_NULL => out.push_str("null"),
        P3J_FALSE => out.push_str("false"),
        P3J_TRUE => out.push_str("true"),
        P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => {
            let i = p3v2_read_int(blob, pos).ok_or_else(err_malformed)?;
            let mut buf = [0u8; 24];
            out.push_str(format_i64_json(i, &mut buf));
        }
        P3J_FLOAT_RAW => {
            if p3v2_node_validity_check(blob, pos, depth).0 != 0 {
                return Err(err_malformed());
            }
            let s = std::str::from_utf8(&blob[pos + 11..end]).map_err(|_| err_malformed())?;
            out.push_str(s);
        }
        t if t >= P3J_STR_SHORT_BASE || matches!(t, P3J_STR_PLAIN | P3J_STR_ESCAPED) => {
            if p3v2_node_validity_check(blob, pos, depth).0 != 0 {
                return Err(err_malformed());
            }
            let hlen = if t >= P3J_STR_SHORT_BASE { 1 } else { 3 };
            let s = std::str::from_utf8(&blob[pos + hlen..end]).map_err(|_| err_malformed())?;
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
        P3J_STR_RAW => {
            let s = std::str::from_utf8(&blob[pos + 3..end]).map_err(|_| err_malformed())?;
            append_escaped_json_string(out, s);
        }
        P3J_EMPTY_ARRAY => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            out.push_str("[]");
        }
        P3J_EMPTY_OBJECT => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            out.push_str("{}");
        }
        P3J_ARRAY => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_elems = usize::from(u16::from_le_bytes([blob[pos + 1], blob[pos + 2]]));
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 3], blob[pos + 4]]));
            let dir_abs = pos + dir_rel;
            out.push('[');
            let mut cursor = pos + 7;
            for m in 0..n_elems {
                if m > 0 {
                    out.push(',');
                }
                let s = dir_abs + 2 * m;
                let elem_rel = usize::from(u16::from_le_bytes([blob[s], blob[s + 1]]));
                if pos + elem_rel != cursor {
                    return Err(err_malformed());
                }
                cursor = p3v2_node_to_text(blob, pos + elem_rel, depth + 1, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            out.push(']');
        }
        P3J_OBJECT_COMPACT => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(blob[pos + 2]);
            let dir_abs = pos + dir_rel;
            out.push('{');
            let mut cursor = pos + 3;
            for m in 0..n_keys {
                if m > 0 {
                    out.push(',');
                }
                let s = dir_abs + 2 * m;
                let kinfo = blob[s];
                let val_rel = usize::from(blob[s + 1]);
                let klen = usize::from(kinfo >> 2);
                let val_pos = pos + val_rel;
                if val_pos < cursor + 1 + klen || val_pos >= dir_abs {
                    return Err(err_malformed());
                }
                let key_pos = val_pos - 1 - klen;
                if key_pos != cursor || blob[key_pos] != (P3J_STR_SHORT_BASE | (klen as u8)) {
                    return Err(err_malformed());
                }
                let kend = p3v2_node_to_text(blob, key_pos, depth + 1, out)?;
                if kend != val_pos {
                    return Err(err_malformed());
                }
                if p3v2_compact_kinfo(&blob[key_pos + 1..val_pos], true) != Some(kinfo) {
                    return Err(err_malformed());
                }
                out.push(':');
                cursor = p3v2_node_to_text(blob, val_pos, depth + 1, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            out.push('}');
        }
        P3J_OBJECT => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 2], blob[pos + 3]]));
            let dir_abs = pos + dir_rel;
            out.push('{');
            let mut cursor = pos + 6;
            for m in 0..n_keys {
                if m > 0 {
                    out.push(',');
                }
                let s = dir_abs + 8 * m;
                let slot_tag = u32::from_le_bytes([blob[s], blob[s + 1], blob[s + 2], blob[s + 3]]);
                let key_rel = usize::from(u16::from_le_bytes([blob[s + 4], blob[s + 5]]));
                let val_rel = usize::from(u16::from_le_bytes([blob[s + 6], blob[s + 7]]));
                let key_pos = pos + key_rel;
                let val_pos = pos + val_rel;
                if key_pos != cursor || key_pos >= dir_abs {
                    return Err(err_malformed());
                }
                let Some((ktag, khdr_len, _)) = p3v2_str_header(blob, key_pos) else {
                    return Err(err_malformed());
                };
                let kend = p3v2_node_to_text(blob, key_pos, depth + 1, out)?;
                if kend != val_pos {
                    return Err(err_malformed());
                }
                let expected_tag =
                    p3v2_key_tag(&blob[key_pos + khdr_len..kend], ktag != P3J_STR_ESCAPED);
                if slot_tag != expected_tag {
                    return Err(err_malformed());
                }
                out.push(':');
                cursor = p3v2_node_to_text(blob, val_pos, depth + 1, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            out.push('}');
        }
        _ => return Err(err_malformed()),
    }
    Ok(end)
}

fn p3v2_node_to_pretty(
    blob: &[u8],
    pos: usize,
    indent: &str,
    level: usize,
    out: &mut String,
) -> Result<usize> {
    if level >= JSON_MAX_DEPTH {
        return Err(err_too_deep());
    }
    let Some(node_len) = p3v2_node_len(blob, pos) else {
        return Err(err_malformed());
    };
    let end = pos + node_len;
    match blob[pos] {
        P3J_EMPTY_ARRAY => {
            out.push_str("[]");
            Ok(end)
        }
        P3J_EMPTY_OBJECT => {
            out.push_str("{}");
            Ok(end)
        }
        P3J_ARRAY => {
            let next_level = level + 1;
            if next_level >= JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_elems = usize::from(u16::from_le_bytes([blob[pos + 1], blob[pos + 2]]));
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 3], blob[pos + 4]]));
            let dir_abs = pos + dir_rel;
            out.push_str("[\n");
            let mut cursor = pos + 7;
            for m in 0..n_elems {
                if m > 0 {
                    out.push_str(",\n");
                }
                for _ in 0..next_level {
                    out.push_str(indent);
                }
                let s = dir_abs + 2 * m;
                let elem_rel = usize::from(u16::from_le_bytes([blob[s], blob[s + 1]]));
                if pos + elem_rel != cursor {
                    return Err(err_malformed());
                }
                cursor = p3v2_node_to_pretty(blob, pos + elem_rel, indent, next_level, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            out.push('\n');
            for _ in 0..level {
                out.push_str(indent);
            }
            out.push(']');
            Ok(end)
        }
        P3J_OBJECT_COMPACT => {
            let next_level = level + 1;
            if next_level >= JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(blob[pos + 2]);
            let dir_abs = pos + dir_rel;
            out.push_str("{\n");
            let mut cursor = pos + 3;
            for m in 0..n_keys {
                if m > 0 {
                    out.push_str(",\n");
                }
                for _ in 0..next_level {
                    out.push_str(indent);
                }
                let s = dir_abs + 2 * m;
                let kinfo = blob[s];
                let val_rel = usize::from(blob[s + 1]);
                let klen = usize::from(kinfo >> 2);
                let val_pos = pos + val_rel;
                if val_pos < cursor + 1 + klen || val_pos >= dir_abs {
                    return Err(err_malformed());
                }
                let key_pos = val_pos - 1 - klen;
                if key_pos != cursor || blob[key_pos] != (P3J_STR_SHORT_BASE | (klen as u8)) {
                    return Err(err_malformed());
                }
                let kend = p3v2_node_to_text(blob, key_pos, next_level, out)?;
                if kend != val_pos {
                    return Err(err_malformed());
                }
                out.push_str(": ");
                cursor = p3v2_node_to_pretty(blob, val_pos, indent, next_level, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            out.push('\n');
            for _ in 0..level {
                out.push_str(indent);
            }
            out.push('}');
            Ok(end)
        }
        P3J_OBJECT => {
            let next_level = level + 1;
            if next_level >= JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 2], blob[pos + 3]]));
            let dir_abs = pos + dir_rel;
            out.push_str("{\n");
            let mut cursor = pos + 6;
            for m in 0..n_keys {
                if m > 0 {
                    out.push_str(",\n");
                }
                for _ in 0..next_level {
                    out.push_str(indent);
                }
                let s = dir_abs + 8 * m;
                let key_rel = usize::from(u16::from_le_bytes([blob[s + 4], blob[s + 5]]));
                let val_rel = usize::from(u16::from_le_bytes([blob[s + 6], blob[s + 7]]));
                let key_pos = pos + key_rel;
                let val_pos = pos + val_rel;
                if key_pos != cursor || key_pos >= dir_abs {
                    return Err(err_malformed());
                }
                if p3v2_str_header(blob, key_pos).is_none() {
                    return Err(err_malformed());
                }
                let kend = p3v2_node_to_text(blob, key_pos, next_level, out)?;
                if kend != val_pos {
                    return Err(err_malformed());
                }
                out.push_str(": ");
                cursor = p3v2_node_to_pretty(blob, val_pos, indent, next_level, out)?;
            }
            if cursor != dir_abs {
                return Err(err_malformed());
            }
            out.push('\n');
            for _ in 0..level {
                out.push_str(indent);
            }
            out.push('}');
            Ok(end)
        }
        _ => p3v2_node_to_text(blob, pos, level, out),
    }
}

/// Renders the JSONB element starting at `blob[offset]` into canonical RFC-8259 JSON text,
/// returning the end byte offset (`offset + n + sz`).
pub fn jsonb_node_to_text(
    blob: &[u8],
    offset: usize,
    depth: usize,
    out: &mut String,
) -> Result<usize> {
    if is_p3v2_jsonb(blob) {
        let start = if offset == 0 { 4 } else { offset };
        let end = p3v2_node_to_text(blob, start, depth, out)?;
        if offset == 0 && end != blob.len() {
            return Err(err_malformed());
        }
        return Ok(end);
    }
    if depth > JSON_MAX_DEPTH {
        return Err(err_too_deep());
    }
    let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
        return Err(err_malformed());
    };
    let end = offset + n + sz;
    let payload = &blob[offset + n..end];
    let etype = blob[offset] & 0x0f;
    match etype {
        JSONB_NULL => {
            if sz != 0 {
                return Err(err_malformed());
            }
            out.push_str("null");
        }
        JSONB_TRUE => {
            if sz != 0 {
                return Err(err_malformed());
            }
            out.push_str("true");
        }
        JSONB_FALSE => {
            if sz != 0 {
                return Err(err_malformed());
            }
            out.push_str("false");
        }
        JSONB_INT | JSONB_FLOAT => {
            if sz == 0 {
                return Err(err_malformed());
            }
            // Validate number payload characters
            if jsonb_validity_check(blob, offset, end, depth) != 0 {
                return Err(err_malformed());
            }
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            out.push_str(s);
        }
        JSONB_INT5 => {
            if sz < 3 {
                return Err(err_malformed());
            }
            let mut k = 0usize;
            if payload[0] == b'-' {
                out.push('-');
                k = 1;
            } else if payload[0] == b'+' {
                k = 1;
            }
            if k + 2 >= sz
                || payload[k] != b'0'
                || (payload[k + 1] != b'x' && payload[k + 1] != b'X')
            {
                return Err(err_malformed());
            }
            k += 2;
            let mut u: u64 = 0;
            let mut overflow = false;
            for &b in &payload[k..] {
                let Some(hv) = hex_val(b) else {
                    return Err(err_malformed());
                };
                if (u >> 60) != 0 {
                    overflow = true;
                } else {
                    u = (u << 4) | u64::from(hv);
                }
            }
            if overflow {
                out.push_str("9.0e999");
            } else {
                out.push_str(&u.to_string());
            }
        }
        JSONB_FLOAT5 => {
            if jsonb_validity_check(blob, offset, end, depth) != 0 {
                return Err(err_malformed());
            }
            let mut k = 0usize;
            if payload[0] == b'-' {
                out.push('-');
                k = 1;
            }
            if payload[k] == b'.' {
                out.push('0');
            }
            while k < sz {
                out.push(payload[k] as char);
                if payload[k] == b'.' && (k + 1 == sz || !payload[k + 1].is_ascii_digit()) {
                    out.push('0');
                }
                k += 1;
            }
        }
        JSONB_TEXT | JSONB_TEXTJ => {
            if jsonb_validity_check(blob, offset, end, depth) != 0 {
                return Err(err_malformed());
            }
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            out.push('"');
            out.push_str(s);
            out.push('"');
        }
        JSONB_TEXT5 => {
            let decoded = jsonb_decode_string(blob, offset)?;
            append_escaped_json_string(out, &decoded);
        }
        JSONB_TEXTRAW => {
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            append_escaped_json_string(out, s);
        }
        JSONB_ARRAY => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            out.push('[');
            let mut j = offset + n;
            let mut first = true;
            while j < end {
                if !first {
                    out.push(',');
                }
                first = false;
                j = jsonb_node_to_text(blob, j, depth + 1, out)?;
                if j > end {
                    return Err(err_malformed());
                }
            }
            out.push(']');
        }
        JSONB_OBJECT => {
            if depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            out.push('{');
            let mut j = offset + n;
            let mut idx = 0usize;
            while j < end {
                if idx > 0 && idx.is_multiple_of(2) {
                    out.push(',');
                }
                if idx.is_multiple_of(2) {
                    let ket = blob.get(j).map(|b| b & 0x0f).unwrap_or(255);
                    if !(JSONB_TEXT..=JSONB_TEXTRAW).contains(&ket) {
                        return Err(err_malformed());
                    }
                }
                j = jsonb_node_to_text(blob, j, depth + 1, out)?;
                if j > end {
                    return Err(err_malformed());
                }
                if idx.is_multiple_of(2) {
                    out.push(':');
                }
                idx += 1;
            }
            if !idx.is_multiple_of(2) {
                return Err(err_malformed());
            }
            out.push('}');
        }
        _ => return Err(err_malformed()),
    }
    Ok(end)
}

/// Converts a complete JSONB root buffer to canonical RFC-8259 JSON text.
pub fn jsonb_to_text(blob: &[u8]) -> Result<String> {
    if blob.is_empty() {
        return Err(err_malformed());
    }
    let mut out = String::with_capacity(blob.len().saturating_add(8));
    let end = jsonb_node_to_text(blob, 0, 0, &mut out)?;
    if end != blob.len() {
        return Err(err_malformed());
    }
    Ok(out)
}

fn jsonb_node_to_pretty(
    blob: &[u8],
    offset: usize,
    indent: &str,
    level: usize,
    out: &mut String,
) -> Result<usize> {
    if level >= JSON_MAX_DEPTH {
        return Err(err_too_deep());
    }
    let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
        return Err(err_malformed());
    };
    let end = offset + n + sz;
    match blob[offset] & 0x0f {
        JSONB_ARRAY => {
            out.push('[');
            let mut j = offset + n;
            if j < end {
                out.push('\n');
                let next_level = level + 1;
                if next_level >= JSON_MAX_DEPTH {
                    return Err(err_too_deep());
                }
                loop {
                    for _ in 0..next_level {
                        out.push_str(indent);
                    }
                    j = jsonb_node_to_pretty(blob, j, indent, next_level, out)?;
                    if j > end {
                        return Err(err_malformed());
                    }
                    if j == end {
                        break;
                    }
                    out.push_str(",\n");
                }
                out.push('\n');
                for _ in 0..level {
                    out.push_str(indent);
                }
            }
            out.push(']');
            Ok(end)
        }
        JSONB_OBJECT => {
            out.push('{');
            let mut j = offset + n;
            if j < end {
                out.push('\n');
                let next_level = level + 1;
                if next_level >= JSON_MAX_DEPTH {
                    return Err(err_too_deep());
                }
                loop {
                    for _ in 0..next_level {
                        out.push_str(indent);
                    }
                    let ket = blob.get(j).map(|b| b & 0x0f).unwrap_or(255);
                    if !(JSONB_TEXT..=JSONB_TEXTRAW).contains(&ket) {
                        return Err(err_malformed());
                    }
                    j = jsonb_node_to_text(blob, j, next_level, out)?;
                    if j >= end {
                        return Err(err_malformed());
                    }
                    out.push_str(": ");
                    j = jsonb_node_to_pretty(blob, j, indent, next_level, out)?;
                    if j > end {
                        return Err(err_malformed());
                    }
                    if j == end {
                        break;
                    }
                    out.push_str(",\n");
                }
                out.push('\n');
                for _ in 0..level {
                    out.push_str(indent);
                }
            }
            out.push('}');
            Ok(end)
        }
        _ => jsonb_node_to_text(blob, offset, level, out),
    }
}

/// Converts a complete JSONB root buffer to pretty-printed JSON text.
pub fn jsonb_to_pretty_text(blob: &[u8], indent: &str) -> Result<String> {
    if blob.is_empty() {
        return Err(err_malformed());
    }
    if is_p3v2_jsonb(blob) {
        let mut out = String::with_capacity(blob.len().saturating_mul(2));
        let end = p3v2_node_to_pretty(blob, 4, indent, 0, &mut out)?;
        if end != blob.len() {
            return Err(err_malformed());
        }
        return Ok(out);
    }
    let mut out = String::with_capacity(blob.len().saturating_mul(2));
    let end = jsonb_node_to_pretty(blob, 0, indent, 0, &mut out)?;
    if end != blob.len() {
        return Err(err_malformed());
    }
    Ok(out)
}

/// Appends a SQL `RuntimeValue` as a JSONB element into `out` (`jsonAppendValue` semantics).
/// `TEXT` with `subtype == JSON_SUBTYPE` is parsed & embedded as JSON; `BLOB` passing
/// `blob_is_jsonb` is validated & embedded as JSONB; any other `BLOB` errors with
/// `"JSON cannot hold BLOB values"`.
pub fn append_sql_arg_as_jsonb(out: &mut Vec<u8>, arg: &RuntimeValue) -> Result<()> {
    match &arg.value {
        Value::Null => jsonb_append_node(out, JSONB_NULL, &[]),
        Value::Integer(i) => {
            let s = i.to_string();
            jsonb_append_node(out, JSONB_INT, s.as_bytes());
        }
        Value::Real(f) => {
            if f.is_nan() {
                jsonb_append_node(out, JSONB_NULL, &[]);
            } else if f.is_infinite() {
                let s = format_json_real(*f);
                jsonb_append_node(out, JSONB_FLOAT, s.as_bytes());
            } else {
                let s = format_finite_json_real(*f);
                jsonb_append_node(out, JSONB_FLOAT, s.as_bytes());
            }
        }
        Value::Text(s) => {
            if u32::from(arg.subtype) == JSON_SUBTYPE {
                match parse_json_text_to_jsonb(s) {
                    Ok(parsed) => {
                        if is_p3v2_jsonb(&parsed.jsonb) {
                            transcode_p3v2_jsonb_to_format3(&parsed.jsonb, out)?;
                        } else {
                            out.extend_from_slice(&parsed.jsonb);
                        }
                    }
                    Err(e) if e.too_deep => return Err(err_too_deep()),
                    Err(_) => return Err(err_malformed()),
                }
            } else {
                let sb = s.as_bytes();
                let plain_len = sqliters_simd::scan_json_plain_ascii_string(sb);
                let plain = plain_len == sb.len()
                    || sb[plain_len..]
                        .iter()
                        .all(|&b| is_json_plain_ok_byte(b) || b == b'\'');
                let etype = if plain { JSONB_TEXT } else { JSONB_TEXTRAW };
                jsonb_append_node(out, etype, sb);
            }
        }
        Value::Blob(b) => {
            if is_p3v2_jsonb(b) {
                transcode_p3v2_jsonb_to_format3(b, out)?;
            } else if blob_is_jsonb(b) {
                if jsonb_validity_check(b, 0, b.len(), 1) != 0 {
                    // Check whether failure was depth or malformed
                    let mut tmp = String::new();
                    jsonb_node_to_text(b, 0, 0, &mut tmp)?;
                }
                out.extend_from_slice(b);
            } else {
                return Err(err_blob_forbidden());
            }
        }
    }
    Ok(())
}

fn p3v2_node_to_sql_value(
    blob: &[u8],
    pos: usize,
    return_blob_containers: bool,
    set_json_subtype_on_containers: bool,
) -> Result<RuntimeValue> {
    let Some(node_len) = p3v2_node_len(blob, pos) else {
        return Err(err_malformed());
    };
    let end = pos + node_len;
    match blob[pos] {
        P3J_NULL => Ok(RuntimeValue::plain(Value::Null)),
        P3J_FALSE => Ok(RuntimeValue::plain(Value::Integer(0))),
        P3J_TRUE => Ok(RuntimeValue::plain(Value::Integer(1))),
        P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => {
            let i = p3v2_read_int(blob, pos).ok_or_else(err_malformed)?;
            Ok(RuntimeValue::plain(Value::Integer(i)))
        }
        P3J_FLOAT_RAW => {
            if p3v2_node_validity_check(blob, pos, 1).0 != 0 {
                return Err(err_malformed());
            }
            let s = &blob[pos + 1..pos + 9];
            let fv = f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
            Ok(RuntimeValue::plain(Value::Real(fv)))
        }
        t if t >= P3J_STR_SHORT_BASE
            || matches!(t, P3J_STR_PLAIN | P3J_STR_ESCAPED | P3J_STR_RAW) =>
        {
            let decoded = p3v2_decode_string(blob, pos)?;
            Ok(RuntimeValue::plain(Value::Text(decoded)))
        }
        P3J_EMPTY_ARRAY => {
            if return_blob_containers {
                Ok(RuntimeValue::plain(Value::Blob(vec![JSONB_ARRAY])))
            } else if set_json_subtype_on_containers {
                Ok(RuntimeValue::with_subtype(
                    Value::Text("[]".to_owned()),
                    JSON_SUBTYPE,
                ))
            } else {
                Ok(RuntimeValue::plain(Value::Text("[]".to_owned())))
            }
        }
        P3J_EMPTY_OBJECT => {
            if return_blob_containers {
                Ok(RuntimeValue::plain(Value::Blob(vec![JSONB_OBJECT])))
            } else if set_json_subtype_on_containers {
                Ok(RuntimeValue::with_subtype(
                    Value::Text("{}".to_owned()),
                    JSON_SUBTYPE,
                ))
            } else {
                Ok(RuntimeValue::plain(Value::Text("{}".to_owned())))
            }
        }
        P3J_ARRAY | P3J_OBJECT | P3J_OBJECT_COMPACT => {
            if return_blob_containers {
                if p3v2_node_validity_check(blob, pos, 1).0 != 0 {
                    let mut canon = String::new();
                    p3v2_node_to_text(blob, pos, 0, &mut canon)?;
                }
                if pos == 4 && end == blob.len() {
                    return Ok(RuntimeValue::plain(Value::Blob(blob.to_vec())));
                }
                let total = 4 + node_len;
                let tb = (total as u16).to_le_bytes();
                let mut sub = Vec::with_capacity(total);
                sub.extend_from_slice(&[P3V2_JSONB_MAGIC0, P3V2_JSONB_VER1, tb[0], tb[1]]);
                sub.extend_from_slice(&blob[pos..end]);
                Ok(RuntimeValue::plain(Value::Blob(sub)))
            } else {
                let mut canon = String::new();
                p3v2_node_to_text(blob, pos, 0, &mut canon)?;
                if set_json_subtype_on_containers {
                    Ok(RuntimeValue::with_subtype(Value::Text(canon), JSON_SUBTYPE))
                } else {
                    Ok(RuntimeValue::plain(Value::Text(canon)))
                }
            }
        }
        _ => Err(err_malformed()),
    }
}

fn p3v2_node_type_name(blob: &[u8], pos: usize) -> Result<&'static str> {
    if p3v2_node_validity_check(blob, pos, 1).0 != 0 {
        let mut tmp = String::new();
        p3v2_node_to_text(blob, pos, 0, &mut tmp)?;
    }
    match blob[pos] {
        P3J_NULL => Ok("null"),
        P3J_FALSE => Ok("false"),
        P3J_TRUE => Ok("true"),
        P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => Ok("integer"),
        P3J_FLOAT_RAW => Ok("real"),
        t if t >= P3J_STR_SHORT_BASE
            || matches!(t, P3J_STR_PLAIN | P3J_STR_ESCAPED | P3J_STR_RAW) =>
        {
            Ok("text")
        }
        P3J_EMPTY_ARRAY | P3J_ARRAY => Ok("array"),
        P3J_EMPTY_OBJECT | P3J_OBJECT | P3J_OBJECT_COMPACT => Ok("object"),
        _ => Err(err_malformed()),
    }
}

/// Converts a single JSONB node at `blob[offset]` to its SQL `RuntimeValue`:
/// - `return_blob_containers == true`: arrays/objects return `Value::Blob` (`subtype = 0`).
/// - `return_blob_containers == false`: arrays/objects return `Value::Text` (`subtype = 74` when
///   `set_json_subtype_on_containers` is true, else `0`).
pub fn jsonb_node_to_sql_value(
    blob: &[u8],
    offset: usize,
    return_blob_containers: bool,
    set_json_subtype_on_containers: bool,
) -> Result<RuntimeValue> {
    if is_p3v2_jsonb(blob) {
        let start = if offset == 0 { 4 } else { offset };
        return p3v2_node_to_sql_value(
            blob,
            start,
            return_blob_containers,
            set_json_subtype_on_containers,
        );
    }
    let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
        return Err(err_malformed());
    };
    let end = offset + n + sz;
    let payload = &blob[offset + n..end];
    match blob[offset] & 0x0f {
        JSONB_NULL => {
            if sz != 0 {
                return Err(err_malformed());
            }
            Ok(RuntimeValue::plain(Value::Null))
        }
        JSONB_TRUE => {
            if sz != 0 {
                return Err(err_malformed());
            }
            Ok(RuntimeValue::plain(Value::Integer(1)))
        }
        JSONB_FALSE => {
            if sz != 0 {
                return Err(err_malformed());
            }
            Ok(RuntimeValue::plain(Value::Integer(0)))
        }
        JSONB_INT => {
            if payload.is_empty() {
                return Err(err_malformed());
            }
            let (neg, digits) = if payload[0] == b'-' {
                if payload.len() < 2 {
                    return Err(err_malformed());
                }
                (true, &payload[1..])
            } else {
                (false, payload)
            };
            let mut acc = 0u64;
            let mut overflow = false;
            for &b in digits {
                if !b.is_ascii_digit() {
                    return Err(err_malformed());
                }
                if !overflow {
                    match acc
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(u64::from(b - b'0')))
                    {
                        Some(next) => acc = next,
                        None => overflow = true,
                    }
                }
            }
            if !overflow {
                if !neg && acc <= i64::MAX as u64 {
                    return Ok(RuntimeValue::plain(Value::Integer(acc as i64)));
                }
                if neg && acc == (i64::MAX as u64) + 1 {
                    return Ok(RuntimeValue::plain(Value::Integer(i64::MIN)));
                }
                if neg && acc <= i64::MAX as u64 {
                    return Ok(RuntimeValue::plain(Value::Integer(-(acc as i64))));
                }
            }
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            let fv = s.parse::<f64>().unwrap_or(f64::INFINITY);
            Ok(RuntimeValue::plain(Value::Real(fv)))
        }
        JSONB_INT5 => {
            if jsonb_validity_check(blob, offset, end, 1) != 0 {
                return Err(err_malformed());
            }
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            let (neg, hex_part) = if let Some(rest) = s.strip_prefix('-') {
                (true, rest)
            } else {
                (false, s)
            };
            let digits = hex_part
                .strip_prefix("0x")
                .or_else(|| hex_part.strip_prefix("0X"))
                .ok_or_else(err_malformed)?;
            if let Ok(u) = u64::from_str_radix(digits, 16) {
                if !neg && u <= i64::MAX as u64 {
                    Ok(RuntimeValue::plain(Value::Integer(u as i64)))
                } else if neg && u == (i64::MAX as u64) + 1 {
                    Ok(RuntimeValue::plain(Value::Integer(i64::MIN)))
                } else if neg && u <= i64::MAX as u64 {
                    Ok(RuntimeValue::plain(Value::Integer(-(u as i64))))
                } else {
                    let f = if neg { -(u as f64) } else { u as f64 };
                    Ok(RuntimeValue::plain(Value::Real(f)))
                }
            } else {
                let mut f = 0.0f64;
                for b in digits.bytes() {
                    let hv = hex_val(b).ok_or_else(err_malformed)? as f64;
                    f = f * 16.0 + hv;
                }
                Ok(RuntimeValue::plain(Value::Real(if neg { -f } else { f })))
            }
        }
        JSONB_FLOAT => {
            if jsonb_validity_check(blob, offset, end, 1) != 0 {
                return Err(err_malformed());
            }
            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
            let fv = s.parse::<f64>().unwrap_or_else(|_| {
                if s.starts_with('-') {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            });
            Ok(RuntimeValue::plain(Value::Real(fv)))
        }
        JSONB_FLOAT5 => {
            let mut canon = String::new();
            jsonb_node_to_text(blob, offset, 0, &mut canon)?;
            let fv = canon.parse::<f64>().unwrap_or_else(|_| {
                if canon.starts_with('-') {
                    f64::NEG_INFINITY
                } else {
                    f64::INFINITY
                }
            });
            Ok(RuntimeValue::plain(Value::Real(fv)))
        }
        JSONB_TEXT | JSONB_TEXTRAW | JSONB_TEXTJ | JSONB_TEXT5 => {
            let decoded = jsonb_decode_string(blob, offset)?;
            Ok(RuntimeValue::plain(Value::Text(decoded)))
        }
        JSONB_ARRAY | JSONB_OBJECT => {
            if return_blob_containers {
                if jsonb_validity_check(blob, offset, end, 1) != 0 {
                    let mut canon = String::new();
                    jsonb_node_to_text(blob, offset, 0, &mut canon)?;
                }
                Ok(RuntimeValue::plain(Value::Blob(blob[offset..end].to_vec())))
            } else {
                let mut canon = String::new();
                jsonb_node_to_text(blob, offset, 0, &mut canon)?;
                if set_json_subtype_on_containers {
                    Ok(RuntimeValue::with_subtype(Value::Text(canon), JSON_SUBTYPE))
                } else {
                    Ok(RuntimeValue::plain(Value::Text(canon)))
                }
            }
        }
        _ => Err(err_malformed()),
    }
}

/// Returns the SQLite `json_type()` string (`'null'`, `'true'`, `'false'`, `'integer'`, `'real'`,
/// `'text'`, `'array'`, `'object'`) for the JSONB node at `blob[offset]`.
pub fn jsonb_node_type_name(blob: &[u8], offset: usize) -> Result<&'static str> {
    if is_p3v2_jsonb(blob) {
        let start = if offset == 0 { 4 } else { offset };
        return p3v2_node_type_name(blob, start);
    }
    let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
        return Err(err_malformed());
    };
    let end = offset + n + sz;
    if jsonb_validity_check(blob, offset, end, 1) != 0 {
        let mut tmp = String::new();
        jsonb_node_to_text(blob, offset, 0, &mut tmp)?;
    }
    match blob[offset] & 0x0f {
        JSONB_NULL => Ok("null"),
        JSONB_TRUE => Ok("true"),
        JSONB_FALSE => Ok("false"),
        JSONB_INT | JSONB_INT5 => Ok("integer"),
        JSONB_FLOAT | JSONB_FLOAT5 => Ok("real"),
        JSONB_TEXT | JSONB_TEXTJ | JSONB_TEXT5 | JSONB_TEXTRAW => Ok("text"),
        JSONB_ARRAY => Ok("array"),
        JSONB_OBJECT => Ok("object"),
        _ => Err(err_malformed()),
    }
}

// ============================================================================
// JSON Path Parser & Lookup / Tree Editor
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathStep {
    Key(String),
    ArrayIndex(usize),
    ArrayFromEnd(usize), // `#-N` where `N == 0` means `#` (append / past-end)
}

pub fn parse_json_path(path: &str) -> Result<Vec<PathStep>> {
    Ok(parse_json_path_with_spans(path)?
        .into_iter()
        .map(|(_, _, step)| step)
        .collect())
}

fn parse_json_path_with_spans(path: &str) -> Result<Vec<(usize, usize, PathStep)>> {
    let Some(mut rest) = path.strip_prefix('$') else {
        return Err(err_bad_path(path));
    };
    let mut steps = Vec::new();
    while !rest.is_empty() {
        let step_start = path.len() - rest.len();
        if let Some(after_dot) = rest.strip_prefix('.') {
            if after_dot.is_empty() {
                return Err(err_bad_path(path));
            }
            if let Some(after_quote) = after_dot.strip_prefix('"') {
                let bytes = after_quote.as_bytes();
                let mut key = String::new();
                let mut i = 0usize;
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        rest = &after_quote[i + 1..];
                        closed = true;
                        break;
                    } else if bytes[i] == b'\\' {
                        let (consumed, cp) = json_unescape_one_char(&bytes[i..]);
                        if cp == JSON_INVALID_CHAR || consumed == 0 {
                            return Err(err_bad_path(path));
                        }
                        if cp != u32::MAX
                            && let Some(ch) = char::from_u32(cp)
                        {
                            key.push(ch);
                        }
                        i += consumed;
                    } else {
                        let ch = after_quote[i..]
                            .chars()
                            .next()
                            .ok_or_else(|| err_bad_path(path))?;
                        key.push(ch);
                        i += ch.len_utf8();
                    }
                }
                if !closed {
                    return Err(err_bad_path(path));
                }
                let step_end = path.len() - rest.len();
                steps.push((step_start, step_end, PathStep::Key(key)));
            } else {
                let end = after_dot.find(['.', '[']).unwrap_or(after_dot.len());
                if end == 0 {
                    return Err(err_bad_path(path));
                }
                let key = &after_dot[..end];
                rest = &after_dot[end..];
                let step_end = path.len() - rest.len();
                steps.push((step_start, step_end, PathStep::Key(key.to_owned())));
            }
        } else if let Some(after_bracket) = rest.strip_prefix('[') {
            let Some(close_idx) = after_bracket.find(']') else {
                return Err(err_bad_path(path));
            };
            let inside = &after_bracket[..close_idx];
            let step = if inside == "#" {
                PathStep::ArrayFromEnd(0)
            } else if let Some(neg_str) = inside.strip_prefix("#-") {
                if neg_str.is_empty() || !neg_str.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(err_bad_path(path));
                }
                let n = neg_str.parse::<usize>().map_err(|_| err_bad_path(path))?;
                PathStep::ArrayFromEnd(n)
            } else {
                if inside.is_empty() || !inside.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(err_bad_path(path));
                }
                let idx = inside.parse::<usize>().map_err(|_| err_bad_path(path))?;
                PathStep::ArrayIndex(idx)
            };
            rest = &after_bracket[close_idx + 1..];
            let step_end = path.len() - rest.len();
            steps.push((step_start, step_end, step));
        } else {
            return Err(err_bad_path(path));
        }
    }
    Ok(steps)
}

/// Looks up `steps` inside `blob` starting at `offset`, returning `Ok(Some((id_offset, val_offset)))`
/// where `id_offset` is the 0-based JSONB byte offset of the key (for object entries) or element
/// (for array elements / root), matching `json_each` / `json_tree` `id` in `json.c`.
pub fn jsonb_lookup_steps_with_id(
    blob: &[u8],
    mut offset: usize,
    steps: &[PathStep],
) -> Result<Option<(usize, usize)>> {
    if is_p3v2_jsonb(blob) {
        let mut pos = if offset == 0 { 4 } else { offset };
        let mut id_offset = offset;
        if p3v2_node_len(blob, pos).is_none() {
            return Err(err_malformed());
        }
        for step in steps {
            let Some(_nlen) = p3v2_node_len(blob, pos) else {
                return Err(err_malformed());
            };
            match (blob[pos], step) {
                (P3J_EMPTY_OBJECT, PathStep::Key(_)) => return Ok(None),
                (P3J_OBJECT_COMPACT, PathStep::Key(target_key)) => {
                    let n_keys = usize::from(blob[pos + 1]);
                    let dir_rel = usize::from(blob[pos + 2]);
                    let dir_abs = pos + dir_rel;
                    let mut found: Option<(usize, usize)> = None;
                    let mut cursor = pos + 3;
                    for m in 0..n_keys {
                        let s = dir_abs + 2 * m;
                        let klen = usize::from(blob[s] >> 2);
                        let val_rel = usize::from(blob[s + 1]);
                        let vpos = pos + val_rel;
                        if vpos < cursor + 1 + klen || vpos >= dir_abs {
                            return Err(err_malformed());
                        }
                        let kpos = vpos - 1 - klen;
                        if kpos != cursor || blob[kpos] != (P3J_STR_SHORT_BASE | (klen as u8)) {
                            return Err(err_malformed());
                        }
                        let key_str = p3v2_decode_string(blob, kpos)?;
                        if found.is_none() && key_str == *target_key {
                            found = Some((kpos, vpos));
                        }
                        let Some(vlen) = p3v2_node_len(&blob[..dir_abs], vpos) else {
                            return Err(err_malformed());
                        };
                        cursor = vpos + vlen;
                    }
                    if cursor != dir_abs {
                        return Err(err_malformed());
                    }
                    match found {
                        Some((kpos, vpos)) => {
                            id_offset = kpos;
                            pos = vpos;
                        }
                        None => return Ok(None),
                    }
                }
                (P3J_OBJECT, PathStep::Key(target_key)) => {
                    let n_keys = usize::from(blob[pos + 1]);
                    let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 2], blob[pos + 3]]));
                    let dir_abs = pos + dir_rel;
                    let mut found: Option<(usize, usize)> = None;
                    for m in 0..n_keys {
                        let s = dir_abs + 8 * m;
                        let key_rel = usize::from(u16::from_le_bytes([blob[s + 4], blob[s + 5]]));
                        let val_rel = usize::from(u16::from_le_bytes([blob[s + 6], blob[s + 7]]));
                        let kpos = pos + key_rel;
                        let vpos = pos + val_rel;
                        if kpos < pos + 6
                            || vpos <= kpos
                            || vpos >= dir_abs
                            || p3v2_str_header(&blob[..dir_abs], kpos)
                                .is_none_or(|(_, khdr, klen)| kpos + khdr + klen != vpos)
                            || p3v2_node_len(&blob[..dir_abs], vpos).is_none()
                        {
                            return Err(err_malformed());
                        }
                        let key_str = p3v2_decode_string(blob, kpos)?;
                        if found.is_none() && key_str == *target_key {
                            found = Some((kpos, vpos));
                        }
                    }
                    match found {
                        Some((kpos, vpos)) => {
                            id_offset = kpos;
                            pos = vpos;
                        }
                        None => return Ok(None),
                    }
                }
                (P3J_EMPTY_ARRAY, PathStep::ArrayIndex(_) | PathStep::ArrayFromEnd(_)) => {
                    return Ok(None);
                }
                (P3J_ARRAY, PathStep::ArrayIndex(target_idx)) => {
                    let n_elems = usize::from(u16::from_le_bytes([blob[pos + 1], blob[pos + 2]]));
                    if *target_idx >= n_elems {
                        return Ok(None);
                    }
                    let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 3], blob[pos + 4]]));
                    let dir_abs = pos + dir_rel;
                    let s = dir_abs + 2 * *target_idx;
                    let elem_rel = usize::from(u16::from_le_bytes([blob[s], blob[s + 1]]));
                    let cpos = pos + elem_rel;
                    if cpos < pos + 7
                        || cpos >= dir_abs
                        || p3v2_node_len(&blob[..dir_abs], cpos).is_none()
                    {
                        return Err(err_malformed());
                    }
                    id_offset = cpos;
                    pos = cpos;
                }
                (P3J_ARRAY, PathStep::ArrayFromEnd(back)) => {
                    let n_elems = usize::from(u16::from_le_bytes([blob[pos + 1], blob[pos + 2]]));
                    if *back == 0 || *back > n_elems {
                        return Ok(None);
                    }
                    let idx = n_elems - *back;
                    let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 3], blob[pos + 4]]));
                    let dir_abs = pos + dir_rel;
                    let s = dir_abs + 2 * idx;
                    let elem_rel = usize::from(u16::from_le_bytes([blob[s], blob[s + 1]]));
                    let cpos = pos + elem_rel;
                    if cpos < pos + 7
                        || cpos >= dir_abs
                        || p3v2_node_len(&blob[..dir_abs], cpos).is_none()
                    {
                        return Err(err_malformed());
                    }
                    id_offset = cpos;
                    pos = cpos;
                }
                _ => return Ok(None),
            }
        }
        return Ok(Some((id_offset, pos)));
    }
    let mut id_offset = offset;
    for step in steps {
        let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
            return Err(err_malformed());
        };
        let end = offset + n + sz;
        let etype = blob[offset] & 0x0f;
        match (etype, step) {
            (JSONB_OBJECT, PathStep::Key(target_key)) => {
                let mut j = offset + n;
                let mut found: Option<(usize, usize)> = None;
                while j < end {
                    let Some((kn, ksz)) = jsonb_payload_size(&blob[..end], j) else {
                        return Err(err_malformed());
                    };
                    let val_pos = j + kn + ksz;
                    if val_pos >= end {
                        return Err(err_malformed());
                    }
                    let Some((vn, vsz)) = jsonb_payload_size(&blob[..end], val_pos) else {
                        return Err(err_malformed());
                    };
                    let key_str = jsonb_decode_string(blob, j)?;
                    // In `json.c` (`jsonLookupStep`), the FIRST matching key wins!
                    if found.is_none() && key_str == *target_key {
                        found = Some((j, val_pos));
                    }
                    j = val_pos + vn + vsz;
                }
                match found {
                    Some((kpos, vpos)) => {
                        id_offset = kpos;
                        offset = vpos;
                    }
                    None => return Ok(None),
                }
            }
            (JSONB_ARRAY, PathStep::ArrayIndex(target_idx)) => {
                let mut j = offset + n;
                let mut idx = 0usize;
                let mut found = None;
                while j < end {
                    let Some((cn, csz)) = jsonb_payload_size(&blob[..end], j) else {
                        return Err(err_malformed());
                    };
                    if idx == *target_idx {
                        found = Some(j);
                        break;
                    }
                    idx += 1;
                    j += cn + csz;
                }
                match found {
                    Some(cpos) => {
                        id_offset = cpos;
                        offset = cpos;
                    }
                    None => return Ok(None),
                }
            }
            (JSONB_ARRAY, PathStep::ArrayFromEnd(back)) => {
                if *back == 0 {
                    // `$[#]` is past-end; never matches an existing element on lookup!
                    return Ok(None);
                }
                let mut elem_offsets = Vec::new();
                let mut j = offset + n;
                while j < end {
                    let Some((cn, csz)) = jsonb_payload_size(&blob[..end], j) else {
                        return Err(err_malformed());
                    };
                    elem_offsets.push(j);
                    j += cn + csz;
                }
                if *back > elem_offsets.len() {
                    return Ok(None);
                }
                let cpos = elem_offsets[elem_offsets.len() - *back];
                id_offset = cpos;
                offset = cpos;
            }
            _ => return Ok(None),
        }
    }
    Ok(Some((id_offset, offset)))
}

/// Looks up `steps` inside `blob` starting at `offset`, returning `Ok(Some(node_offset))` if found,
/// or `Ok(None)` if not found.
pub fn jsonb_lookup_steps(blob: &[u8], offset: usize, steps: &[PathStep]) -> Result<Option<usize>> {
    Ok(jsonb_lookup_steps_with_id(blob, offset, steps)?.map(|(_, v)| v))
}

enum JsonbCow<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl JsonbCow<'_> {
    #[inline]
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(b) => b,
            Self::Owned(v) => v.as_slice(),
        }
    }
}

#[inline]
fn parse_func_arg_to_jsonb_cow(val: &Value) -> Result<Option<JsonbCow<'_>>> {
    match val {
        Value::Null => Ok(None),
        Value::Blob(b) if blob_is_jsonb(b) => Ok(Some(JsonbCow::Borrowed(b.as_slice()))),
        _ => Ok(parse_func_arg_to_jsonb(val)?.map(JsonbCow::Owned)),
    }
}

#[inline]
fn validate_jsonb_root(blob: &[u8]) -> Result<()> {
    if blob.is_empty() {
        return Err(err_malformed());
    }
    if jsonb_validity_check(blob, 0, blob.len(), 1) == 0 {
        return Ok(());
    }
    let _ = jsonb_to_text(blob)?;
    Ok(())
}

#[inline]
fn next_label_codepoint(s: &mut &[u8], is_raw: bool) -> u32 {
    loop {
        if s.is_empty() {
            return 0;
        }
        if is_raw || s[0] != b'\\' {
            if s[0] < 0x80 {
                let cp = u32::from(s[0]);
                *s = &s[1..];
                return cp;
            }
            if let Ok(st) = std::str::from_utf8(s)
                && let Some(ch) = st.chars().next()
            {
                *s = &s[ch.len_utf8()..];
                return ch as u32;
            }
            *s = &s[1..];
            return JSON_INVALID_CHAR;
        }
        let (consumed, cp) = json_unescape_one_char(s);
        let step = consumed.clamp(1, s.len());
        *s = &s[step..];
        if cp == u32::MAX {
            continue;
        }
        return cp;
    }
}

#[inline(never)]
fn jsonb_label_compare_escaped(
    mut left: &[u8],
    raw_left: bool,
    mut right: &[u8],
    raw_right: bool,
) -> bool {
    loop {
        let cl = next_label_codepoint(&mut left, raw_left);
        let cr = next_label_codepoint(&mut right, raw_right);
        if cl == JSON_INVALID_CHAR || cr == JSON_INVALID_CHAR || cl != cr {
            return false;
        }
        if cl == 0 {
            return true;
        }
    }
}

#[inline]
fn jsonb_label_compare(left: &[u8], raw_left: bool, right: &[u8], raw_right: bool) -> bool {
    if raw_left && raw_right {
        left == right
    } else {
        jsonb_label_compare_escaped(left, raw_left, right, raw_right)
    }
}

#[inline]
fn jsonb_array_count_in_slice(blob: &[u8], elem_start: usize, i_end: usize) -> Result<u64> {
    let mut j = elem_start;
    let mut cnt = 0u64;
    while j < i_end {
        if (blob[j] & 0x0f) > JSONB_OBJECT {
            return Err(err_malformed());
        }
        let Some((cn, csz)) = jsonb_payload_size(&blob[..i_end], j) else {
            return Err(err_malformed());
        };
        cnt += 1;
        j += cn + csz;
    }
    Ok(cnt)
}

#[cold]
fn p3v2_validate_mismatched_node(blob: &[u8], pos: usize) -> Result<()> {
    if p3v2_node_validity_check(blob, pos, 1).0 != 0 {
        return Err(err_malformed());
    }
    Ok(())
}

#[inline]
fn p3v2_lookup_single_key(
    blob: &[u8],
    pos: usize,
    key_bytes: &[u8],
    raw_key: bool,
    target_tag: u32,
    compact_kinfo: Option<u8>,
) -> Result<Option<usize>> {
    if p3v2_node_len(blob, pos).is_none() {
        return Err(err_malformed());
    }
    match blob[pos] {
        P3J_EMPTY_OBJECT => return Ok(None),
        P3J_OBJECT_COMPACT => {
            let n_keys = usize::from(blob[pos + 1]);
            let dir_rel = usize::from(blob[pos + 2]);
            let dir_abs = pos + dir_rel;
            if let Some(tkinfo) = compact_kinfo {
                let klen = key_bytes.len();
                let expected_khdr = P3J_STR_SHORT_BASE | (klen as u8);
                for m in 0..n_keys {
                    let s = dir_abs + 2 * m;
                    if blob[s] != tkinfo {
                        continue;
                    }
                    let val_pos = pos + usize::from(blob[s + 1]);
                    if val_pos < pos + 4 + klen || val_pos >= dir_abs {
                        return Err(err_malformed());
                    }
                    let key_pos = val_pos - 1 - klen;
                    if blob[key_pos] != expected_khdr {
                        return Err(err_malformed());
                    }
                    if &blob[key_pos + 1..val_pos] != key_bytes {
                        continue;
                    }
                    if p3v2_node_len(&blob[..dir_abs], val_pos).is_none() {
                        return Err(err_malformed());
                    }
                    return Ok(Some(val_pos));
                }
                return Ok(None);
            }
            if raw_key {
                // `raw_key` with `len > 63` cannot match any `0..=63 B` compact key.
                return Ok(None);
            }
            for m in 0..n_keys {
                let s = dir_abs + 2 * m;
                let klen = usize::from(blob[s] >> 2);
                let val_pos = pos + usize::from(blob[s + 1]);
                if val_pos < pos + 4 + klen || val_pos >= dir_abs {
                    return Err(err_malformed());
                }
                let key_pos = val_pos - 1 - klen;
                if blob[key_pos] != (P3J_STR_SHORT_BASE | (klen as u8)) {
                    return Err(err_malformed());
                }
                if jsonb_label_compare(key_bytes, false, &blob[key_pos + 1..val_pos], true) {
                    if p3v2_node_len(&blob[..dir_abs], val_pos).is_none() {
                        return Err(err_malformed());
                    }
                    return Ok(Some(val_pos));
                }
            }
            return Ok(None);
        }
        P3J_OBJECT => {}
        _ => {
            p3v2_validate_mismatched_node(blob, pos)?;
            return Ok(None);
        }
    }
    let n_keys = usize::from(blob[pos + 1]);
    let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 2], blob[pos + 3]]));
    let dir_abs = pos + dir_rel;
    let query_plain = (target_tag & 0x80) == 0;
    for m in 0..n_keys {
        let s = dir_abs + 8 * m;
        let slot_tag = u32::from_le_bytes([blob[s], blob[s + 1], blob[s + 2], blob[s + 3]]);
        if query_plain && (slot_tag & 0x80) == 0 {
            if slot_tag != target_tag {
                continue;
            }
            let key_rel = usize::from(u16::from_le_bytes([blob[s + 4], blob[s + 5]]));
            let val_rel = usize::from(u16::from_le_bytes([blob[s + 6], blob[s + 7]]));
            let key_pos = pos + key_rel;
            let val_pos = pos + val_rel;
            if key_pos < pos + 6 || val_pos <= key_pos || val_pos >= dir_abs {
                return Err(err_malformed());
            }
            let Some((knorm_tag, khdr_len, klen)) = p3v2_str_header(&blob[..dir_abs], key_pos)
            else {
                return Err(err_malformed());
            };
            if !matches!(knorm_tag, P3J_STR_PLAIN | P3J_STR_RAW)
                || klen != key_bytes.len()
                || key_pos + khdr_len + klen != val_pos
            {
                return Err(err_malformed());
            }
            if klen > 3 && blob[key_pos + khdr_len + 3..val_pos] != key_bytes[3..] {
                continue;
            }
            if p3v2_node_len(&blob[..dir_abs], val_pos).is_none() {
                return Err(err_malformed());
            }
            return Ok(Some(val_pos));
        } else {
            let key_rel = usize::from(u16::from_le_bytes([blob[s + 4], blob[s + 5]]));
            let val_rel = usize::from(u16::from_le_bytes([blob[s + 6], blob[s + 7]]));
            let key_pos = pos + key_rel;
            let val_pos = pos + val_rel;
            if key_pos < pos + 6 || val_pos <= key_pos || val_pos >= dir_abs {
                return Err(err_malformed());
            }
            let Some((knorm_tag, khdr_len, klen)) = p3v2_str_header(&blob[..dir_abs], key_pos)
            else {
                return Err(err_malformed());
            };
            if key_pos + khdr_len + klen != val_pos {
                return Err(err_malformed());
            }
            let raw_label = knorm_tag != P3J_STR_ESCAPED;
            if jsonb_label_compare(
                key_bytes,
                raw_key,
                &blob[key_pos + khdr_len..val_pos],
                raw_label,
            ) {
                if p3v2_node_len(&blob[..dir_abs], val_pos).is_none() {
                    return Err(err_malformed());
                }
                return Ok(Some(val_pos));
            }
        }
    }
    Ok(None)
}

#[inline]
fn p3v2_lookup_single_array_index(blob: &[u8], pos: usize, idx: i64) -> Result<Option<usize>> {
    if p3v2_node_len(blob, pos).is_none() {
        return Err(err_malformed());
    }
    match blob[pos] {
        P3J_EMPTY_ARRAY => Ok(None),
        P3J_ARRAY => {
            let n_elems = usize::from(u16::from_le_bytes([blob[pos + 1], blob[pos + 2]]));
            let target_idx = if idx >= 0 {
                let u = idx as u64;
                if u >= n_elems as u64 {
                    return Ok(None);
                }
                u as usize
            } else {
                let back = idx.unsigned_abs();
                if back == 0 || back > n_elems as u64 {
                    return Ok(None);
                }
                n_elems - (back as usize)
            };
            let dir_rel = usize::from(u16::from_le_bytes([blob[pos + 3], blob[pos + 4]]));
            let dir_abs = pos + dir_rel;
            let s = dir_abs + 2 * target_idx;
            let elem_rel = usize::from(u16::from_le_bytes([blob[s], blob[s + 1]]));
            let elem_pos = pos + elem_rel;
            if elem_pos < pos + 7
                || elem_pos >= dir_abs
                || p3v2_node_len(&blob[..dir_abs], elem_pos).is_none()
            {
                return Err(err_malformed());
            }
            Ok(Some(elem_pos))
        }
        _ => {
            p3v2_validate_mismatched_node(blob, pos)?;
            Ok(None)
        }
    }
}

fn p3v2_lookup_raw_path(
    blob: &[u8],
    mut pos: usize,
    mut z_path: &str,
    full_path: &str,
) -> Result<Option<usize>> {
    let mut depth = 0usize;
    if p3v2_node_len(blob, pos).is_none() {
        return Err(err_malformed());
    }
    while !z_path.is_empty() {
        let pbytes = z_path.as_bytes();
        if pbytes[0] == b'.' {
            let after_dot = &z_path[1..];
            let abytes = after_dot.as_bytes();
            if abytes.is_empty() {
                return Err(err_bad_path(full_path));
            }
            let (key_bytes, raw_key, rest) = if abytes[0] == b'"' {
                let mut i = 1usize;
                let mut has_backslash = false;
                while i < abytes.len() && abytes[i] != b'"' {
                    if abytes[i] == b'\\' {
                        has_backslash = true;
                        if i + 1 < abytes.len() {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                if i >= abytes.len() {
                    return Err(err_bad_path(full_path));
                }
                (&abytes[1..i], !has_backslash, &after_dot[i + 1..])
            } else {
                let mut i = 0usize;
                while i < abytes.len() && abytes[i] != b'.' && abytes[i] != b'[' {
                    i += 1;
                }
                if i == 0 {
                    return Err(err_bad_path(full_path));
                }
                (&abytes[..i], true, &after_dot[i..])
            };
            let target_tag = p3v2_key_tag(key_bytes, raw_key);
            let compact_kinfo = p3v2_compact_kinfo(key_bytes, raw_key);
            let Some(v) =
                p3v2_lookup_single_key(blob, pos, key_bytes, raw_key, target_tag, compact_kinfo)?
            else {
                return Ok(None);
            };
            depth += 1;
            if depth >= JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            pos = v;
            z_path = rest;
        } else if pbytes[0] == b'[' {
            let mut kk = 0u64;
            let mut i = 1usize;
            while i < pbytes.len() && pbytes[i].is_ascii_digit() {
                if kk < 0xffff_ffff {
                    kk = kk * 10 + u64::from(pbytes[i] - b'0');
                }
                i += 1;
            }
            let (is_from_end, nn_back, rest) = if i >= 2 && pbytes.get(i) == Some(&b']') {
                (false, 0u64, &z_path[i + 1..])
            } else if pbytes.get(1) == Some(&b'#') {
                i = 2;
                if pbytes.get(2) == Some(&b'-') && pbytes.get(3).is_some_and(u8::is_ascii_digit) {
                    let mut nn = 0u64;
                    i = 3;
                    while i < pbytes.len() && pbytes[i].is_ascii_digit() {
                        if nn < 0xffff_ffff {
                            nn = nn * 10 + u64::from(pbytes[i] - b'0');
                        }
                        i += 1;
                    }
                    if pbytes.get(i) != Some(&b']') {
                        return Err(err_bad_path(full_path));
                    }
                    (true, nn, &z_path[i + 1..])
                } else if pbytes.get(i) == Some(&b']') {
                    // Validate current node before returning `None` for `$[#]`!
                    match blob[pos] {
                        P3J_EMPTY_ARRAY | P3J_ARRAY => {}
                        _ => p3v2_validate_mismatched_node(blob, pos)?,
                    }
                    return Ok(None);
                } else {
                    return Err(err_bad_path(full_path));
                }
            } else {
                return Err(err_bad_path(full_path));
            };

            let cpos_opt = if is_from_end {
                if nn_back == 0 {
                    match blob[pos] {
                        P3J_EMPTY_ARRAY | P3J_ARRAY => {}
                        _ => p3v2_validate_mismatched_node(blob, pos)?,
                    }
                    None
                } else {
                    p3v2_lookup_single_array_index(blob, pos, -(nn_back as i64))?
                }
            } else {
                p3v2_lookup_single_array_index(blob, pos, kk as i64)?
            };
            let Some(cpos) = cpos_opt else {
                return Ok(None);
            };
            depth += 1;
            if depth >= JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            pos = cpos;
            z_path = rest;
        } else {
            return Err(err_bad_path(full_path));
        }
    }
    Ok(Some(pos))
}

#[cold]
fn validate_mismatched_node(blob: &[u8], i_root: usize) -> Result<()> {
    let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
        return Err(err_malformed());
    };
    if jsonb_validity_check(blob, i_root, i_root + n + sz, 1) != 0 {
        return Err(err_malformed());
    }
    Ok(())
}

#[inline]
fn jsonb_lookup_single_key(
    blob: &[u8],
    i_root: usize,
    key_bytes: &[u8],
    raw_key: bool,
) -> Result<Option<usize>> {
    if is_p3v2_jsonb(blob) {
        let start = if i_root == 0 { 4 } else { i_root };
        return p3v2_lookup_single_key(
            blob,
            start,
            key_bytes,
            raw_key,
            p3v2_key_tag(key_bytes, raw_key),
            p3v2_compact_kinfo(key_bytes, raw_key),
        );
    }
    if i_root >= blob.len() || (blob[i_root] & 0x0f) > JSONB_OBJECT {
        return Err(err_malformed());
    }
    if (blob[i_root] & 0x0f) != JSONB_OBJECT {
        validate_mismatched_node(blob, i_root)?;
        return Ok(None);
    }
    let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
        return Err(err_malformed());
    };
    let mut j = i_root + n;
    let i_end = j + sz;
    while j < i_end {
        let kx = blob[j] & 0x0f;
        if !(JSONB_TEXT..=JSONB_TEXTRAW).contains(&kx) {
            return Err(err_malformed());
        }
        let Some((kn, ksz)) = jsonb_payload_size(&blob[..i_end], j) else {
            return Err(err_malformed());
        };
        let k = j + kn;
        if k + ksz >= i_end {
            return Err(err_malformed());
        }
        let raw_label = kx == JSONB_TEXT || kx == JSONB_TEXTRAW;
        let key_match = if raw_key && raw_label {
            ksz == key_bytes.len() && &blob[k..k + ksz] == key_bytes
        } else {
            jsonb_label_compare(key_bytes, raw_key, &blob[k..k + ksz], raw_label)
        };
        if key_match {
            let v = k + ksz;
            if (blob[v] & 0x0f) > JSONB_OBJECT || jsonb_payload_size(&blob[..i_end], v).is_none() {
                return Err(err_malformed());
            }
            return Ok(Some(v));
        }
        j = k + ksz;
        if (blob[j] & 0x0f) > JSONB_OBJECT {
            return Err(err_malformed());
        }
        let Some((vn, vsz)) = jsonb_payload_size(&blob[..i_end], j) else {
            return Err(err_malformed());
        };
        j += vn + vsz;
    }
    Ok(None)
}

#[inline]
fn jsonb_lookup_single_array_index(blob: &[u8], i_root: usize, idx: i64) -> Result<Option<usize>> {
    if is_p3v2_jsonb(blob) {
        let start = if i_root == 0 { 4 } else { i_root };
        return p3v2_lookup_single_array_index(blob, start, idx);
    }
    if i_root >= blob.len() || (blob[i_root] & 0x0f) > JSONB_OBJECT {
        return Err(err_malformed());
    }
    if (blob[i_root] & 0x0f) != JSONB_ARRAY {
        validate_mismatched_node(blob, i_root)?;
        return Ok(None);
    }
    let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
        return Err(err_malformed());
    };
    let elem_start = i_root + n;
    let i_end = elem_start + sz;
    let target_kk: u64 = if idx >= 0 {
        idx as u64
    } else {
        let back = idx.unsigned_abs();
        let cnt = jsonb_array_count_in_slice(blob, elem_start, i_end)?;
        if back > cnt {
            return Ok(None);
        }
        cnt - back
    };
    let mut j = elem_start;
    let mut kk = target_kk;
    while j < i_end {
        if (blob[j] & 0x0f) > JSONB_OBJECT {
            return Err(err_malformed());
        }
        let Some((cn, csz)) = jsonb_payload_size(&blob[..i_end], j) else {
            return Err(err_malformed());
        };
        if kk == 0 {
            return Ok(Some(j));
        }
        kk -= 1;
        j += cn + csz;
    }
    Ok(None)
}

/// Zero-allocation in-place JSONB path walker (`jsonLookupStep` in `sqlite/src/json.c`).
fn jsonb_lookup_raw_path(
    blob: &[u8],
    mut i_root: usize,
    mut z_path: &str,
    full_path: &str,
) -> Result<Option<usize>> {
    if is_p3v2_jsonb(blob) {
        let start = if i_root == 0 { 4 } else { i_root };
        return p3v2_lookup_raw_path(blob, start, z_path, full_path);
    }
    let mut depth = 0usize;
    if i_root >= blob.len()
        || (blob[i_root] & 0x0f) > JSONB_OBJECT
        || jsonb_payload_size(blob, i_root).is_none()
    {
        return Err(err_malformed());
    }

    while !z_path.is_empty() {
        let pbytes = z_path.as_bytes();
        if pbytes[0] == b'.' {
            let after_dot = &z_path[1..];
            let abytes = after_dot.as_bytes();
            if abytes.is_empty() {
                return Err(err_bad_path(full_path));
            }
            let (key_bytes, raw_key, rest) = if abytes[0] == b'"' {
                let mut i = 1usize;
                let mut has_backslash = false;
                while i < abytes.len() && abytes[i] != b'"' {
                    if abytes[i] == b'\\' {
                        has_backslash = true;
                        if i + 1 < abytes.len() {
                            i += 1;
                        }
                    }
                    i += 1;
                }
                if i >= abytes.len() {
                    return Err(err_bad_path(full_path));
                }
                (&abytes[1..i], !has_backslash, &after_dot[i + 1..])
            } else {
                let mut i = 0usize;
                while i < abytes.len() && abytes[i] != b'.' && abytes[i] != b'[' {
                    i += 1;
                }
                if i == 0 {
                    return Err(err_bad_path(full_path));
                }
                (&abytes[..i], true, &after_dot[i..])
            };

            if (blob[i_root] & 0x0f) != JSONB_OBJECT {
                validate_mismatched_node(blob, i_root)?;
                return Ok(None);
            }
            let Some(v) = jsonb_lookup_single_key(blob, i_root, key_bytes, raw_key)? else {
                return Ok(None);
            };
            depth += 1;
            if depth >= JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            i_root = v;
            z_path = rest;
        } else if pbytes[0] == b'[' {
            if (blob[i_root] & 0x0f) != JSONB_ARRAY {
                validate_mismatched_node(blob, i_root)?;
                return Ok(None);
            }
            let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
                return Err(err_malformed());
            };
            let elem_start = i_root + n;
            let i_end = elem_start + sz;

            let mut kk = 0u64;
            let mut i = 1usize;
            while i < pbytes.len() && pbytes[i].is_ascii_digit() {
                if kk < 0xffff_ffff {
                    kk = kk * 10 + u64::from(pbytes[i] - b'0');
                }
                i += 1;
            }
            let rest = if i >= 2 && pbytes.get(i) == Some(&b']') {
                &z_path[i + 1..]
            } else if pbytes.get(1) == Some(&b'#') {
                let cnt = jsonb_array_count_in_slice(blob, elem_start, i_end)?;
                i = 2;
                if pbytes.get(2) == Some(&b'-') && pbytes.get(3).is_some_and(u8::is_ascii_digit) {
                    let mut nn = 0u64;
                    i = 3;
                    while i < pbytes.len() && pbytes[i].is_ascii_digit() {
                        if nn < 0xffff_ffff {
                            nn = nn * 10 + u64::from(pbytes[i] - b'0');
                        }
                        i += 1;
                    }
                    if pbytes.get(i) != Some(&b']') {
                        return Err(err_bad_path(full_path));
                    }
                    if nn > cnt {
                        return Ok(None);
                    }
                    kk = cnt - nn;
                    &z_path[i + 1..]
                } else {
                    if pbytes.get(i) != Some(&b']') {
                        return Err(err_bad_path(full_path));
                    }
                    // `$[#]` is past-end; never matches an existing element on read!
                    return Ok(None);
                }
            } else {
                return Err(err_bad_path(full_path));
            };

            let mut j = elem_start;
            let mut found = None;
            while j < i_end {
                if (blob[j] & 0x0f) > JSONB_OBJECT {
                    return Err(err_malformed());
                }
                let Some((cn, csz)) = jsonb_payload_size(&blob[..i_end], j) else {
                    return Err(err_malformed());
                };
                if kk == 0 {
                    found = Some(j);
                    break;
                }
                kk -= 1;
                j += cn + csz;
            }
            let Some(cpos) = found else {
                return Ok(None);
            };
            depth += 1;
            if depth >= JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            i_root = cpos;
            z_path = rest;
        } else {
            return Err(err_bad_path(full_path));
        }
    }

    Ok(Some(i_root))
}

// ============================================================================
// Mutable DOM for `json_insert` / `json_replace` / `json_set` / `json_remove` /
// `json_array_insert` / `json_patch`
// ============================================================================

#[derive(Debug, Clone)]
enum EditNode {
    Raw(Vec<u8>),
    Array(Vec<EditNode>),
    Object(Vec<(String, Vec<u8>, EditNode)>), // (decoded_key, encoded_key_node, value_node)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    Insert,      // `json_insert`: insert only if missing
    Replace,     // `json_replace`: overwrite only if existing
    Set,         // `json_set`: insert or overwrite, creating intermediate containers
    Remove,      // `json_remove`: delete if existing
    ArrayInsert, // `json_array_insert`: insert before target array element
}

impl EditNode {
    fn from_jsonb(blob: &[u8], offset: usize, depth: usize) -> Result<(Self, usize)> {
        if depth > JSON_MAX_DEPTH {
            return Err(err_too_deep());
        }
        let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
            return Err(err_malformed());
        };
        let end = offset + n + sz;
        match blob[offset] & 0x0f {
            JSONB_ARRAY => {
                let mut items = Vec::new();
                let mut j = offset + n;
                while j < end {
                    let (child, next_j) = Self::from_jsonb(&blob[..end], j, depth + 1)?;
                    items.push(child);
                    j = next_j;
                }
                Ok((Self::Array(items), end))
            }
            JSONB_OBJECT => {
                let mut entries = Vec::new();
                let mut j = offset + n;
                while j < end {
                    let Some((kn, ksz)) = jsonb_payload_size(&blob[..end], j) else {
                        return Err(err_malformed());
                    };
                    let ket = blob[j] & 0x0f;
                    if !(JSONB_TEXT..=JSONB_TEXTRAW).contains(&ket) {
                        return Err(err_malformed());
                    }
                    let key_end = j + kn + ksz;
                    if key_end >= end {
                        return Err(err_malformed());
                    }
                    let decoded_key = jsonb_decode_string(blob, j)?;
                    let raw_key = blob[j..key_end].to_vec();
                    let (val_node, next_j) = Self::from_jsonb(&blob[..end], key_end, depth + 1)?;
                    entries.push((decoded_key, raw_key, val_node));
                    j = next_j;
                }
                Ok((Self::Object(entries), end))
            }
            _ => {
                if jsonb_validity_check(blob, offset, end, depth) != 0 {
                    let mut tmp = String::new();
                    jsonb_node_to_text(blob, offset, depth, &mut tmp)?;
                }
                Ok((Self::Raw(blob[offset..end].to_vec()), end))
            }
        }
    }

    fn to_jsonb(&self, depth: usize, out: &mut Vec<u8>) -> Result<()> {
        if depth > JSON_MAX_DEPTH {
            return Err(err_too_deep());
        }
        match self {
            Self::Raw(raw) => {
                // Verify combined depth if `raw` is itself a container (e.g. newly inserted JSONB)
                check_jsonb_depth(raw, 0, depth)?;
                out.extend_from_slice(raw);
            }
            Self::Array(items) => {
                if depth + 1 > JSON_MAX_DEPTH && !items.is_empty() {
                    return Err(err_too_deep());
                }
                let mut buf = Vec::new();
                for item in items {
                    item.to_jsonb(depth + 1, &mut buf)?;
                }
                jsonb_append_node(out, JSONB_ARRAY, &buf);
            }
            Self::Object(entries) => {
                if depth + 1 > JSON_MAX_DEPTH && !entries.is_empty() {
                    return Err(err_too_deep());
                }
                let mut buf = Vec::new();
                for (_, raw_key, val) in entries {
                    buf.extend_from_slice(raw_key);
                    val.to_jsonb(depth + 1, &mut buf)?;
                }
                jsonb_append_node(out, JSONB_OBJECT, &buf);
            }
        }
        Ok(())
    }

    fn is_null(&self) -> bool {
        matches!(self, Self::Raw(b) if b.len() == 1 && (b[0] & 0x0f) == JSONB_NULL)
    }
}

fn check_jsonb_depth(blob: &[u8], offset: usize, depth: usize) -> Result<usize> {
    if depth > JSON_MAX_DEPTH {
        return Err(err_too_deep());
    }
    let Some((n, sz)) = jsonb_payload_size(blob, offset) else {
        return Err(err_malformed());
    };
    let end = offset + n + sz;
    match blob[offset] & 0x0f {
        JSONB_ARRAY | JSONB_OBJECT => {
            if sz > 0 && depth + 1 > JSON_MAX_DEPTH {
                return Err(err_too_deep());
            }
            let mut j = offset + n;
            while j < end {
                j = check_jsonb_depth(&blob[..end], j, depth + 1)?;
            }
            Ok(end)
        }
        _ => Ok(end),
    }
}

fn encode_key_jsonb(key: &str) -> Vec<u8> {
    let mut raw = Vec::with_capacity(key.len() + 2);
    let plain = key.bytes().all(|b| is_json_plain_ok_byte(b) || b == b'\'');
    jsonb_append_node(
        &mut raw,
        if plain { JSONB_TEXT } else { JSONB_TEXTRAW },
        key.as_bytes(),
    );
    raw
}

fn build_intermediate_for_steps(steps: &[PathStep], new_val: EditNode) -> Option<EditNode> {
    let Some((first, rest)) = steps.split_first() else {
        return Some(new_val);
    };
    match first {
        PathStep::Key(k) => {
            let child = build_intermediate_for_steps(rest, new_val)?;
            Some(EditNode::Object(vec![(
                k.clone(),
                encode_key_jsonb(k),
                child,
            )]))
        }
        PathStep::ArrayIndex(0) | PathStep::ArrayFromEnd(0) => {
            let child = build_intermediate_for_steps(rest, new_val)?;
            Some(EditNode::Array(vec![child]))
        }
        PathStep::ArrayIndex(_) | PathStep::ArrayFromEnd(_) => None,
    }
}

fn apply_edit(node: &mut EditNode, steps: &[PathStep], mode: EditMode, new_val: Option<&[u8]>) {
    let Some((step, rest)) = steps.split_first() else {
        match mode {
            EditMode::Replace | EditMode::Set => {
                if let Some(v) = new_val {
                    *node = EditNode::Raw(v.to_vec());
                }
            }
            EditMode::Remove => {
                *node = EditNode::Raw(vec![JSONB_NULL]);
            }
            EditMode::Insert | EditMode::ArrayInsert => {}
        }
        return;
    };

    match (node, step) {
        (EditNode::Object(entries), PathStep::Key(k)) => {
            let existing_pos = entries.iter().position(|(ek, _, _)| ek == k);
            if rest.is_empty() {
                match (mode, existing_pos) {
                    (EditMode::Insert, None) | (EditMode::Set, None) => {
                        if let Some(v) = new_val {
                            entries.push((
                                k.clone(),
                                encode_key_jsonb(k),
                                EditNode::Raw(v.to_vec()),
                            ));
                        }
                    }
                    (EditMode::Replace, Some(pos)) | (EditMode::Set, Some(pos)) => {
                        if let Some(v) = new_val {
                            entries[pos].2 = EditNode::Raw(v.to_vec());
                        }
                    }
                    (EditMode::Remove, Some(pos)) => {
                        entries.remove(pos);
                    }
                    _ => {}
                }
            } else if let Some(pos) = existing_pos {
                apply_edit(&mut entries[pos].2, rest, mode, new_val);
            } else if matches!(
                mode,
                EditMode::Insert | EditMode::Set | EditMode::ArrayInsert
            ) && let Some(v) = new_val
                && let Some(sub) = build_intermediate_for_steps(rest, EditNode::Raw(v.to_vec()))
            {
                entries.push((k.clone(), encode_key_jsonb(k), sub));
            }
        }
        (EditNode::Array(items), PathStep::ArrayIndex(idx)) => {
            if rest.is_empty() {
                match mode {
                    EditMode::ArrayInsert => {
                        if *idx <= items.len()
                            && let Some(v) = new_val
                        {
                            items.insert(*idx, EditNode::Raw(v.to_vec()));
                        }
                    }
                    EditMode::Replace | EditMode::Set => {
                        if *idx < items.len()
                            && let Some(v) = new_val
                        {
                            items[*idx] = EditNode::Raw(v.to_vec());
                        } else if *idx == items.len()
                            && mode == EditMode::Set
                            && let Some(v) = new_val
                        {
                            items.push(EditNode::Raw(v.to_vec()));
                        }
                    }
                    EditMode::Remove => {
                        if *idx < items.len() {
                            items.remove(*idx);
                        }
                    }
                    EditMode::Insert => {
                        if *idx == items.len()
                            && let Some(v) = new_val
                        {
                            items.push(EditNode::Raw(v.to_vec()));
                        }
                    }
                }
            } else if *idx < items.len() {
                apply_edit(&mut items[*idx], rest, mode, new_val);
            } else if *idx == items.len()
                && matches!(
                    mode,
                    EditMode::Insert | EditMode::Set | EditMode::ArrayInsert
                )
                && let Some(v) = new_val
                && let Some(sub) = build_intermediate_for_steps(rest, EditNode::Raw(v.to_vec()))
            {
                items.push(sub);
            }
        }
        (EditNode::Array(items), PathStep::ArrayFromEnd(back)) => {
            if *back == 0 {
                // `[#]` appends to end of array
                if rest.is_empty() {
                    if matches!(
                        mode,
                        EditMode::Insert | EditMode::Set | EditMode::ArrayInsert
                    ) && let Some(v) = new_val
                    {
                        items.push(EditNode::Raw(v.to_vec()));
                    }
                } else if matches!(
                    mode,
                    EditMode::Insert | EditMode::Set | EditMode::ArrayInsert
                ) && let Some(v) = new_val
                    && let Some(sub) = build_intermediate_for_steps(rest, EditNode::Raw(v.to_vec()))
                {
                    items.push(sub);
                }
            } else if *back <= items.len() {
                let idx = items.len() - *back;
                if rest.is_empty() {
                    match mode {
                        EditMode::ArrayInsert => {
                            if let Some(v) = new_val {
                                items.insert(idx, EditNode::Raw(v.to_vec()));
                            }
                        }
                        EditMode::Replace | EditMode::Set => {
                            if let Some(v) = new_val {
                                items[idx] = EditNode::Raw(v.to_vec());
                            }
                        }
                        EditMode::Remove => {
                            items.remove(idx);
                        }
                        EditMode::Insert => {}
                    }
                } else {
                    apply_edit(&mut items[idx], rest, mode, new_val);
                }
            }
        }
        _ => {}
    }
}

fn merge_patch(target: EditNode, patch: EditNode) -> EditNode {
    match patch {
        EditNode::Object(patch_entries) => {
            let mut target_entries = match target {
                EditNode::Object(entries) => entries,
                _ => Vec::new(),
            };
            for (pk, praw_key, pval) in patch_entries {
                if pval.is_null() {
                    target_entries.retain(|(tk, _, _)| *tk != pk);
                } else if let Some(pos) = target_entries.iter().position(|(tk, _, _)| *tk == pk) {
                    let prev = std::mem::replace(
                        &mut target_entries[pos].2,
                        EditNode::Raw(vec![JSONB_NULL]),
                    );
                    target_entries[pos].2 = merge_patch(prev, pval);
                } else {
                    let cleaned = merge_patch(EditNode::Object(Vec::new()), pval);
                    target_entries.push((pk, praw_key, cleaned));
                }
            }
            EditNode::Object(target_entries)
        }
        other => other,
    }
}

// ============================================================================
// Public Scalar Evaluators (`json*` & `jsonb*` + `->` / `->>`)
// ============================================================================

fn finish_json_or_jsonb(jsonb: Vec<u8>, as_blob: bool) -> Result<RuntimeValue> {
    if as_blob {
        validate_jsonb_root(&jsonb)?;
        if !is_p3v2_jsonb(&jsonb)
            && let Some(p3) = try_transcode_format3_to_p3v2(&jsonb)
        {
            return Ok(RuntimeValue::plain(Value::Blob(p3)));
        }
        Ok(RuntimeValue::plain(Value::Blob(jsonb)))
    } else {
        let text = jsonb_to_text(&jsonb)?;
        Ok(RuntimeValue::with_subtype(Value::Text(text), JSON_SUBTYPE))
    }
}

/// `json(X)` / `jsonb(X)`
pub fn eval_json(arg: &RuntimeValue, as_blob: bool) -> Result<RuntimeValue> {
    if as_blob
        && matches!(
            arg.value,
            Value::Text(_) | Value::Integer(_) | Value::Real(_)
        )
    {
        let Some(jsonb) = parse_func_arg_to_jsonb(&arg.value)? else {
            return Ok(RuntimeValue::plain(Value::Null));
        };
        return Ok(RuntimeValue::plain(Value::Blob(jsonb)));
    }
    let Some(jsonb) = parse_func_arg_to_jsonb(&arg.value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    finish_json_or_jsonb(jsonb, as_blob)
}

/// `json_array(V1, V2, ...)` / `jsonb_array(V1, V2, ...)`
pub fn eval_json_array(args: &[RuntimeValue], as_blob: bool) -> Result<RuntimeValue> {
    let mut children = Vec::new();
    for arg in args {
        append_sql_arg_as_jsonb(&mut children, arg)?;
    }
    let mut out = Vec::with_capacity(children.len() + 5);
    jsonb_append_node(&mut out, JSONB_ARRAY, &children);
    finish_json_or_jsonb(out, as_blob)
}

/// `json_object(K1, V1, ...)` / `jsonb_object(K1, V1, ...)`
pub fn eval_json_object(args: &[RuntimeValue], as_blob: bool) -> Result<RuntimeValue> {
    if !args.len().is_multiple_of(2) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "json_object() requires an even number of arguments",
        ));
    }
    let mut children = Vec::new();
    for pair in args.as_chunks::<2>().0 {
        let key_str = match &pair[0].value {
            Value::Text(s) => s.clone(),
            // Verify: in `json.c`, `sqlite3_value_type(argv[i]) != SQLITE_TEXT` -> `"json_object() labels must be TEXT"`!
            _ => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "json_object() labels must be TEXT",
                ));
            }
        };
        let plain = key_str
            .bytes()
            .all(|b| is_json_plain_ok_byte(b) || b == b'\'');
        jsonb_append_node(
            &mut children,
            if plain { JSONB_TEXT } else { JSONB_TEXTRAW },
            key_str.as_bytes(),
        );
        append_sql_arg_as_jsonb(&mut children, &pair[1])?;
    }
    let mut out = Vec::with_capacity(children.len() + 5);
    jsonb_append_node(&mut out, JSONB_OBJECT, &children);
    finish_json_or_jsonb(out, as_blob)
}

/// `json_quote(X)`
pub fn eval_json_quote(arg: &RuntimeValue) -> Result<RuntimeValue> {
    if matches!(arg.value, Value::Null) {
        return Ok(RuntimeValue::with_subtype(
            Value::Text("null".to_owned()),
            JSON_SUBTYPE,
        ));
    }
    let mut buf = Vec::new();
    append_sql_arg_as_jsonb(&mut buf, arg)?;
    finish_json_or_jsonb(buf, false)
}

/// `json_array_length(X[, P])`
pub fn eval_json_array_length(args: &[RuntimeValue]) -> Result<RuntimeValue> {
    let Some(jb_cow) = parse_func_arg_to_jsonb_cow(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let jsonb = jb_cow.as_slice();
    if matches!(jb_cow, JsonbCow::Borrowed(_)) {
        validate_jsonb_root(jsonb)?;
    }
    let target_offset = if args.len() == 2 {
        let path_cow: std::borrow::Cow<'_, str> = match &args[1].value {
            Value::Null => return Ok(RuntimeValue::plain(Value::Null)),
            Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
            other => std::borrow::Cow::Owned(other.to_text_lossy()),
        };
        let Some(after_dollar) = path_cow.strip_prefix('$') else {
            return Err(err_bad_path(&path_cow));
        };
        let Some(pos) = jsonb_lookup_raw_path(jsonb, 0, after_dollar, &path_cow)? else {
            return Ok(RuntimeValue::plain(Value::Null));
        };
        pos
    } else {
        0usize
    };
    if is_p3v2_jsonb(jsonb) {
        let pos = if target_offset == 0 { 4 } else { target_offset };
        if p3v2_node_len(jsonb, pos).is_none() {
            return Err(err_malformed());
        }
        let cnt = match jsonb[pos] {
            P3J_EMPTY_ARRAY => 0i64,
            P3J_ARRAY => i64::from(u16::from_le_bytes([jsonb[pos + 1], jsonb[pos + 2]])),
            _ => 0i64,
        };
        return Ok(RuntimeValue::plain(Value::Integer(cnt)));
    }
    let Some((n, sz)) = jsonb_payload_size(jsonb, target_offset) else {
        return Err(err_malformed());
    };
    if (jsonb[target_offset] & 0x0f) != JSONB_ARRAY {
        return Ok(RuntimeValue::plain(Value::Integer(0)));
    }
    let cnt = jsonb_array_count_in_slice(jsonb, target_offset + n, target_offset + n + sz)? as i64;
    Ok(RuntimeValue::plain(Value::Integer(cnt)))
}

/// `json_type(X[, P])`
pub fn eval_json_type(args: &[RuntimeValue]) -> Result<RuntimeValue> {
    let Some(jb_cow) = parse_func_arg_to_jsonb_cow(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let jsonb = jb_cow.as_slice();
    if matches!(jb_cow, JsonbCow::Borrowed(_)) {
        validate_jsonb_root(jsonb)?;
    }
    let target_offset = if args.len() == 2 {
        let path_cow: std::borrow::Cow<'_, str> = match &args[1].value {
            Value::Null => return Ok(RuntimeValue::plain(Value::Null)),
            Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
            other => std::borrow::Cow::Owned(other.to_text_lossy()),
        };
        let Some(after_dollar) = path_cow.strip_prefix('$') else {
            return Err(err_bad_path(&path_cow));
        };
        let Some(pos) = jsonb_lookup_raw_path(jsonb, 0, after_dollar, &path_cow)? else {
            return Ok(RuntimeValue::plain(Value::Null));
        };
        pos
    } else {
        0usize
    };
    let tname = jsonb_node_type_name(jsonb, target_offset)?;
    Ok(RuntimeValue::plain(Value::Text(tname.to_owned())))
}

/// `json_valid(X[, flags])`
pub fn eval_json_valid(args: &[RuntimeValue]) -> Result<RuntimeValue> {
    let flags: u8 = if args.len() == 2 {
        let f = match &args[1].value {
            Value::Null => 0,
            v => v.to_integer_lossy(),
        };
        if !(1..=15).contains(&f) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "FLAGS parameter to json_valid() must be between 1 and 15",
            ));
        }
        f as u8
    } else {
        1
    };

    match &args[0].value {
        Value::Null => Ok(RuntimeValue::plain(Value::Null)),
        Value::Blob(b) => {
            if blob_is_jsonb(b) {
                let ok = if (flags & 0x04) != 0 {
                    true
                } else if (flags & 0x08) != 0 {
                    jsonb_validity_check(b, 0, b.len(), 1) == 0
                } else {
                    false
                };
                return Ok(RuntimeValue::plain(Value::Integer(i64::from(ok))));
            }
            if (flags & 0x03) == 0 {
                return Ok(RuntimeValue::plain(Value::Integer(0)));
            }
            let Ok(s) = std::str::from_utf8(b) else {
                return Ok(RuntimeValue::plain(Value::Integer(0)));
            };
            let ok = match parse_json_text_to_jsonb(s) {
                Ok(parsed) => (flags & 0x02) != 0 || !parsed.has_nonstd,
                Err(_) => false,
            };
            Ok(RuntimeValue::plain(Value::Integer(i64::from(ok))))
        }
        other => {
            if (flags & 0x03) == 0 {
                return Ok(RuntimeValue::plain(Value::Integer(0)));
            }
            let s = other.to_text_lossy();
            let ok = match parse_json_text_to_jsonb(&s) {
                Ok(parsed) => (flags & 0x02) != 0 || !parsed.has_nonstd,
                Err(_) => false,
            };
            Ok(RuntimeValue::plain(Value::Integer(i64::from(ok))))
        }
    }
}

/// `json_error_position(X)`
pub fn eval_json_error_position(arg: &RuntimeValue) -> Result<RuntimeValue> {
    match &arg.value {
        Value::Null => Ok(RuntimeValue::plain(Value::Null)),
        Value::Blob(b) => {
            if blob_is_jsonb(b) {
                let pos = jsonb_validity_check(b, 0, b.len(), 1) as i64;
                return Ok(RuntimeValue::plain(Value::Integer(pos)));
            }
            let Ok(s) = std::str::from_utf8(b) else {
                // Find first invalid UTF-8 byte 1-based character position
                let mut char_pos = 1i64;
                for &byte in b {
                    if (byte & 0xc0) != 0x80 {
                        char_pos += 1;
                    }
                }
                return Ok(RuntimeValue::plain(Value::Integer(char_pos.max(1))));
            };
            Ok(RuntimeValue::plain(Value::Integer(text_error_char_pos(s))))
        }
        other => {
            let s = other.to_text_lossy();
            Ok(RuntimeValue::plain(Value::Integer(text_error_char_pos(&s))))
        }
    }
}

fn text_error_char_pos(s: &str) -> i64 {
    match parse_json_text_to_jsonb(s) {
        Ok(_) => 0,
        Err(err) => {
            let bytes = s.as_bytes();
            let limit = err.byte_offset.min(bytes.len());
            let mut pos = 0i64;
            for &b in &bytes[..limit] {
                if (b & 0xc0) != 0x80 {
                    pos += 1;
                }
            }
            pos + 1
        }
    }
}

/// `json_extract(X, P1, ...)` / `jsonb_extract(X, P1, ...)`
pub fn eval_json_extract(args: &[RuntimeValue], as_blob: bool) -> Result<RuntimeValue> {
    if args.len() < 2 {
        return Ok(RuntimeValue::plain(Value::Null));
    }
    let Some(jb_cow) = parse_func_arg_to_jsonb_cow(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let jsonb = jb_cow.as_slice();

    if args.len() == 2 {
        let path_cow: std::borrow::Cow<'_, str> = match &args[1].value {
            Value::Null => return Ok(RuntimeValue::plain(Value::Null)),
            Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
            other => std::borrow::Cow::Owned(other.to_text_lossy()),
        };
        let Some(after_dollar) = path_cow.strip_prefix('$') else {
            return Err(err_bad_path(&path_cow));
        };
        let Some(pos) = jsonb_lookup_raw_path(jsonb, 0, after_dollar, &path_cow)? else {
            return Ok(RuntimeValue::plain(Value::Null));
        };
        return jsonb_node_to_sql_value(jsonb, pos, as_blob, !as_blob);
    }

    let mut children = Vec::new();
    for path_arg in &args[1..] {
        let path_cow: std::borrow::Cow<'_, str> = match &path_arg.value {
            Value::Null => return Ok(RuntimeValue::plain(Value::Null)),
            Value::Text(s) => std::borrow::Cow::Borrowed(s.as_str()),
            other => std::borrow::Cow::Owned(other.to_text_lossy()),
        };
        let Some(after_dollar) = path_cow.strip_prefix('$') else {
            return Err(err_bad_path(&path_cow));
        };
        if let Some(pos) = jsonb_lookup_raw_path(jsonb, 0, after_dollar, &path_cow)? {
            if is_p3v2_jsonb(jsonb) {
                let p3_pos = if pos == 0 { 4 } else { pos };
                transcode_p3v2_node_to_format3(jsonb, p3_pos, 1, &mut children)?;
            } else {
                let Some((n, sz)) = jsonb_payload_size(jsonb, pos) else {
                    return Err(err_malformed());
                };
                children.extend_from_slice(&jsonb[pos..pos + n + sz]);
            }
        } else {
            jsonb_append_node(&mut children, JSONB_NULL, &[]);
        }
    }
    let mut out = Vec::with_capacity(children.len() + 5);
    jsonb_append_node(&mut out, JSONB_ARRAY, &children);
    finish_json_or_jsonb(out, as_blob)
}

#[inline]
fn finish_arrow_node(blob: &[u8], pos: usize, as_sql: bool) -> Result<RuntimeValue> {
    if as_sql {
        jsonb_node_to_sql_value(blob, pos, false, false)
    } else {
        let mut text = String::new();
        jsonb_node_to_text(blob, pos, 0, &mut text)?;
        Ok(RuntimeValue::with_subtype(Value::Text(text), JSON_SUBTYPE))
    }
}

#[inline]
fn format_i64_json(mut i: i64, buf: &mut [u8; 24]) -> &str {
    let neg = i < 0;
    let mut u = i.unsigned_abs();
    let mut pos = buf.len();
    loop {
        pos -= 1;
        buf[pos] = b'0' + (u % 10) as u8;
        u /= 10;
        if u == 0 {
            break;
        }
    }
    if neg {
        pos -= 1;
        buf[pos] = b'-';
    }
    i = 0;
    let _ = i;
    std::str::from_utf8(&buf[pos..]).expect("ASCII digits")
}

#[inline]
fn parse_jsonb_int_i64(payload: &[u8]) -> Result<Option<i64>> {
    if payload.is_empty() {
        return Err(err_malformed());
    }
    let (neg, digits) = if payload[0] == b'-' {
        if payload.len() < 2 {
            return Err(err_malformed());
        }
        (true, &payload[1..])
    } else {
        (false, payload)
    };
    let mut acc = 0u64;
    let mut overflow = false;
    for &b in digits {
        if !b.is_ascii_digit() {
            return Err(err_malformed());
        }
        if !overflow {
            match acc
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(b - b'0')))
            {
                Some(next) => acc = next,
                None => overflow = true,
            }
        }
    }
    if !overflow {
        if !neg && acc <= i64::MAX as u64 {
            return Ok(Some(acc as i64));
        }
        if neg && acc == (i64::MAX as u64) + 1 {
            return Ok(Some(i64::MIN));
        }
        if neg && acc <= i64::MAX as u64 {
            return Ok(Some(-(acc as i64)));
        }
    }
    Ok(None)
}

/// Fast result of [`CompiledJsonArrowPath::eval_raw_field_int_fast`], avoiding `Value` /
/// `RuntimeValue` construction on integer/boolean/null `->>` extractions (`W10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastJsonIntOut {
    Null,
    Int(i64),
    Fallback,
}

/// Appends a single SQL [`RuntimeValue`] directly as an RFC-8259 JSON array element into `out`
/// (`json_group_array` semantics), avoiding per-element JSONB buffer allocations.
pub fn append_runtime_value_json_elem(arg: &RuntimeValue, out: &mut String) -> Result<()> {
    match &arg.value {
        Value::Null => out.push_str("null"),
        Value::Integer(i) => {
            let mut buf = [0u8; 24];
            out.push_str(format_i64_json(*i, &mut buf));
        }
        Value::Real(f) => {
            if f.is_nan() {
                out.push_str("null");
            } else if f.is_infinite() {
                out.push_str(format_json_real(*f));
            } else {
                out.push_str(&format_finite_json_real(*f));
            }
        }
        Value::Text(s) => {
            if u32::from(arg.subtype) == JSON_SUBTYPE {
                let parsed = parse_json_text_to_jsonb(s).map_err(|e| {
                    if e.too_deep {
                        err_too_deep()
                    } else {
                        err_malformed()
                    }
                })?;
                let end = jsonb_node_to_text(&parsed.jsonb, 0, 1, out)?;
                if end != parsed.jsonb.len() {
                    return Err(err_malformed());
                }
            } else if s.bytes().all(|b| is_json_plain_ok_byte(b) || b == b'\'') {
                out.push('"');
                out.push_str(s);
                out.push('"');
            } else {
                append_escaped_json_string(out, s);
            }
        }
        Value::Blob(b) => {
            if blob_is_jsonb(b) {
                let end = jsonb_node_to_text(b, 0, 1, out)?;
                if end != b.len() {
                    return Err(err_malformed());
                }
            } else {
                return Err(err_blob_forbidden());
            }
        }
    }
    Ok(())
}

/// Pre-parsed step for [`CompiledJsonArrowPath`].
#[derive(Debug, Clone)]
enum CompiledArrowStep {
    Key {
        bytes: Box<[u8]>,
        raw_key: bool,
        p3_tag: u32,
        compact_kinfo: Option<u8>,
    },
    ArrayIndex(u64),
    ArrayFromEnd(u64),
    PastEnd,
}

#[derive(Debug, Clone, Copy)]
struct InlineCompactKey {
    kinfo: u8,
    khdr: u8,
    klen: u8,
    bytes: [u8; 15],
}

impl InlineCompactKey {
    #[inline]
    fn from_step(step: &CompiledArrowStep) -> Option<Self> {
        let CompiledArrowStep::Key {
            bytes,
            raw_key: true,
            compact_kinfo: Some(kinfo),
            ..
        } = step
        else {
            return None;
        };
        if bytes.len() > 15 {
            return None;
        }
        let mut buf = [0u8; 15];
        buf[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            kinfo: *kinfo,
            khdr: P3J_STR_SHORT_BASE | (bytes.len() as u8),
            klen: bytes.len() as u8,
            bytes: buf,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum InlineCompactKeys {
    One(InlineCompactKey),
    Two(InlineCompactKey, InlineCompactKey),
}

impl InlineCompactKeys {
    fn from_steps(steps: &[CompiledArrowStep]) -> Option<Self> {
        match steps {
            [s0] => Some(Self::One(InlineCompactKey::from_step(s0)?)),
            [s0, s1] => Some(Self::Two(
                InlineCompactKey::from_step(s0)?,
                InlineCompactKey::from_step(s1)?,
            )),
            _ => None,
        }
    }
}

#[inline(always)]
fn lookup_p3v2_compact_key_inline(
    blob: &[u8],
    pos: usize,
    k: &InlineCompactKey,
) -> Result<Option<usize>> {
    if pos + 3 > blob.len() {
        return Err(err_malformed());
    }
    let n_keys = usize::from(blob[pos + 1]);
    let dir_rel = usize::from(blob[pos + 2]);
    if n_keys == 0 || n_keys > 31 || dir_rel <= 4 {
        return Err(err_malformed());
    }
    let dir_abs = pos + dir_rel;
    if dir_abs + 2 * n_keys > blob.len() {
        return Err(err_malformed());
    }
    let klen = usize::from(k.klen);
    let target_slice = &k.bytes[..klen];
    for m in 0..n_keys {
        let s = dir_abs + 2 * m;
        if blob[s] != k.kinfo {
            continue;
        }
        let val_pos = pos + usize::from(blob[s + 1]);
        if val_pos < pos + 4 + klen || val_pos >= dir_abs {
            return Err(err_malformed());
        }
        let key_pos = val_pos - 1 - klen;
        if blob[key_pos] != k.khdr {
            return Err(err_malformed());
        }
        if &blob[key_pos + 1..val_pos] != target_slice {
            continue;
        }
        if p3v2_node_len(&blob[..dir_abs], val_pos).is_none() {
            return Err(err_malformed());
        }
        return Ok(Some(val_pos));
    }
    Ok(None)
}

#[derive(Debug, Clone)]
enum CompiledArrowInner {
    Steps(Box<[CompiledArrowStep]>),
    Fallback(Box<str>),
}

/// Pre-parsed constant RHS path for `->` and `->>` JSON extraction, avoiding per-row path
/// string parsing and borrowing JSONB slices directly in place.
#[derive(Debug, Clone)]
pub struct CompiledJsonArrowPath {
    compact_keys: Option<InlineCompactKeys>,
    inner: CompiledArrowInner,
}

impl CompiledJsonArrowPath {
    #[inline]
    fn from_steps_vec(steps: Vec<CompiledArrowStep>) -> Self {
        let compact_keys = InlineCompactKeys::from_steps(&steps);
        Self {
            compact_keys,
            inner: CompiledArrowInner::Steps(steps.into_boxed_slice()),
        }
    }

    pub fn compile(rhs_path: &str) -> Self {
        if let Some(after_dollar) = rhs_path.strip_prefix('$') {
            if let Some(steps) = Self::try_parse_raw_steps(after_dollar) {
                return Self::from_steps_vec(steps);
            }
        } else if !rhs_path.is_empty()
            && rhs_path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Self::from_steps_vec(vec![CompiledArrowStep::Key {
                bytes: rhs_path.as_bytes().into(),
                raw_key: true,
                p3_tag: p3v2_key_tag(rhs_path.as_bytes(), true),
                compact_kinfo: p3v2_compact_kinfo(rhs_path.as_bytes(), true),
            }]);
        } else if rhs_path.len() >= 3
            && rhs_path.starts_with('[')
            && rhs_path.ends_with(']')
            && let Some(steps) = Self::try_parse_raw_steps(rhs_path)
        {
            return Self::from_steps_vec(steps);
        } else {
            let quoted = format!(".\"{rhs_path}\"");
            if let Some(steps) = Self::try_parse_raw_steps(&quoted) {
                return Self::from_steps_vec(steps);
            }
        }
        Self {
            compact_keys: None,
            inner: CompiledArrowInner::Fallback(rhs_path.into()),
        }
    }

    fn try_parse_raw_steps(mut z_path: &str) -> Option<Vec<CompiledArrowStep>> {
        let mut steps = Vec::new();
        while !z_path.is_empty() {
            let pbytes = z_path.as_bytes();
            if pbytes[0] == b'.' {
                let after_dot = &z_path[1..];
                let abytes = after_dot.as_bytes();
                if abytes.is_empty() {
                    return None;
                }
                let (key_bytes, raw_key, rest) = if abytes[0] == b'"' {
                    let mut i = 1usize;
                    let mut has_backslash = false;
                    while i < abytes.len() && abytes[i] != b'"' {
                        if abytes[i] == b'\\' {
                            has_backslash = true;
                            if i + 1 < abytes.len() {
                                i += 1;
                            }
                        }
                        i += 1;
                    }
                    if i >= abytes.len() {
                        return None;
                    }
                    (&abytes[1..i], !has_backslash, &after_dot[i + 1..])
                } else {
                    let mut i = 0usize;
                    while i < abytes.len() && abytes[i] != b'.' && abytes[i] != b'[' {
                        i += 1;
                    }
                    if i == 0 {
                        return None;
                    }
                    (&abytes[..i], true, &after_dot[i..])
                };
                steps.push(CompiledArrowStep::Key {
                    bytes: key_bytes.into(),
                    raw_key,
                    p3_tag: p3v2_key_tag(key_bytes, raw_key),
                    compact_kinfo: p3v2_compact_kinfo(key_bytes, raw_key),
                });
                if steps.len() >= JSON_MAX_DEPTH {
                    return None;
                }
                z_path = rest;
            } else if pbytes[0] == b'[' {
                let mut kk = 0u64;
                let mut i = 1usize;
                while i < pbytes.len() && pbytes[i].is_ascii_digit() {
                    if kk < 0xffff_ffff {
                        kk = kk * 10 + u64::from(pbytes[i] - b'0');
                    }
                    i += 1;
                }
                if i >= 2 && pbytes.get(i) == Some(&b']') {
                    steps.push(CompiledArrowStep::ArrayIndex(kk));
                    z_path = &z_path[i + 1..];
                } else if pbytes.get(1) == Some(&b'#') {
                    if pbytes.get(2) == Some(&b'-') && pbytes.get(3).is_some_and(u8::is_ascii_digit)
                    {
                        let mut nn = 0u64;
                        i = 3;
                        while i < pbytes.len() && pbytes[i].is_ascii_digit() {
                            if nn < 0xffff_ffff {
                                nn = nn * 10 + u64::from(pbytes[i] - b'0');
                            }
                            i += 1;
                        }
                        if pbytes.get(i) != Some(&b']') {
                            return None;
                        }
                        steps.push(CompiledArrowStep::ArrayFromEnd(nn));
                        z_path = &z_path[i + 1..];
                    } else if pbytes.get(2) == Some(&b']') {
                        steps.push(CompiledArrowStep::PastEnd);
                        z_path = &z_path[3..];
                    } else {
                        return None;
                    }
                } else {
                    return None;
                }
                if steps.len() >= JSON_MAX_DEPTH {
                    return None;
                }
            } else {
                return None;
            }
        }
        Some(steps)
    }

    #[inline]
    fn lookup_in_jsonb_slice(&self, blob: &[u8]) -> Result<Option<usize>> {
        if let Some(ref ck) = self.compact_keys
            && is_p3v2_jsonb(blob)
            && blob[4] == P3J_OBJECT_COMPACT
        {
            match ck {
                InlineCompactKeys::One(k0) => {
                    return lookup_p3v2_compact_key_inline(blob, 4, k0);
                }
                InlineCompactKeys::Two(k0, k1) => {
                    let Some(p1) = lookup_p3v2_compact_key_inline(blob, 4, k0)? else {
                        return Ok(None);
                    };
                    if blob[p1] == P3J_OBJECT_COMPACT {
                        return lookup_p3v2_compact_key_inline(blob, p1, k1);
                    }
                    return p3v2_lookup_single_key(
                        blob,
                        p1,
                        &k1.bytes[..usize::from(k1.klen)],
                        true,
                        p3v2_key_tag(&k1.bytes[..usize::from(k1.klen)], true),
                        Some(k1.kinfo),
                    );
                }
            }
        }
        match &self.inner {
            CompiledArrowInner::Steps(steps) => {
                if is_p3v2_jsonb(blob) {
                    if steps.is_empty() {
                        if p3v2_node_len(blob, 4).is_none() {
                            return Err(err_malformed());
                        }
                        return Ok(Some(4));
                    }
                    let mut pos = 4usize;
                    for step in steps.iter() {
                        match step {
                            CompiledArrowStep::Key {
                                bytes,
                                raw_key,
                                p3_tag,
                                compact_kinfo,
                            } => {
                                let Some(v) = p3v2_lookup_single_key(
                                    blob,
                                    pos,
                                    bytes,
                                    *raw_key,
                                    *p3_tag,
                                    *compact_kinfo,
                                )?
                                else {
                                    return Ok(None);
                                };
                                pos = v;
                            }
                            CompiledArrowStep::ArrayIndex(kk) => {
                                let Some(v) =
                                    p3v2_lookup_single_array_index(blob, pos, *kk as i64)?
                                else {
                                    return Ok(None);
                                };
                                pos = v;
                            }
                            CompiledArrowStep::ArrayFromEnd(nn) => {
                                if *nn == 0 {
                                    if p3v2_node_len(blob, pos).is_none() {
                                        return Err(err_malformed());
                                    }
                                    match blob[pos] {
                                        P3J_EMPTY_ARRAY | P3J_ARRAY => {}
                                        _ => p3v2_validate_mismatched_node(blob, pos)?,
                                    }
                                    return Ok(None);
                                }
                                let Some(v) =
                                    p3v2_lookup_single_array_index(blob, pos, -(*nn as i64))?
                                else {
                                    return Ok(None);
                                };
                                pos = v;
                            }
                            CompiledArrowStep::PastEnd => {
                                if p3v2_node_len(blob, pos).is_none() {
                                    return Err(err_malformed());
                                }
                                match blob[pos] {
                                    P3J_EMPTY_ARRAY | P3J_ARRAY => {}
                                    _ => p3v2_validate_mismatched_node(blob, pos)?,
                                }
                                return Ok(None);
                            }
                        }
                    }
                    return Ok(Some(pos));
                }
                if blob.is_empty() || (blob[0] & 0x0f) > JSONB_OBJECT {
                    return Err(err_malformed());
                }
                if steps.is_empty() {
                    if jsonb_payload_size(blob, 0).is_none() {
                        return Err(err_malformed());
                    }
                    return Ok(Some(0));
                }
                let mut i_root = 0usize;
                for step in steps.iter() {
                    match step {
                        CompiledArrowStep::Key { bytes, raw_key, .. } => {
                            let Some(v) = jsonb_lookup_single_key(blob, i_root, bytes, *raw_key)?
                            else {
                                return Ok(None);
                            };
                            i_root = v;
                        }
                        CompiledArrowStep::ArrayIndex(kk) => {
                            if (blob[i_root] & 0x0f) != JSONB_ARRAY {
                                validate_mismatched_node(blob, i_root)?;
                                return Ok(None);
                            }
                            let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
                                return Err(err_malformed());
                            };
                            let elem_start = i_root + n;
                            let i_end = elem_start + sz;
                            let mut j = elem_start;
                            let mut rem = *kk;
                            let mut found = None;
                            while j < i_end {
                                if (blob[j] & 0x0f) > JSONB_OBJECT {
                                    return Err(err_malformed());
                                }
                                let Some((cn, csz)) = jsonb_payload_size(&blob[..i_end], j) else {
                                    return Err(err_malformed());
                                };
                                if rem == 0 {
                                    found = Some(j);
                                    break;
                                }
                                rem -= 1;
                                j += cn + csz;
                            }
                            let Some(cpos) = found else {
                                return Ok(None);
                            };
                            i_root = cpos;
                        }
                        CompiledArrowStep::ArrayFromEnd(nn) => {
                            if (blob[i_root] & 0x0f) != JSONB_ARRAY {
                                validate_mismatched_node(blob, i_root)?;
                                return Ok(None);
                            }
                            let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
                                return Err(err_malformed());
                            };
                            let elem_start = i_root + n;
                            let i_end = elem_start + sz;
                            let cnt = jsonb_array_count_in_slice(blob, elem_start, i_end)?;
                            if *nn > cnt || *nn == 0 {
                                return Ok(None);
                            }
                            let mut rem = cnt - *nn;
                            let mut j = elem_start;
                            let mut found = None;
                            while j < i_end {
                                if (blob[j] & 0x0f) > JSONB_OBJECT {
                                    return Err(err_malformed());
                                }
                                let Some((cn, csz)) = jsonb_payload_size(&blob[..i_end], j) else {
                                    return Err(err_malformed());
                                };
                                if rem == 0 {
                                    found = Some(j);
                                    break;
                                }
                                rem -= 1;
                                j += cn + csz;
                            }
                            let Some(cpos) = found else {
                                return Ok(None);
                            };
                            i_root = cpos;
                        }
                        CompiledArrowStep::PastEnd => {
                            if (blob[i_root] & 0x0f) != JSONB_ARRAY {
                                validate_mismatched_node(blob, i_root)?;
                                return Ok(None);
                            }
                            let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
                                return Err(err_malformed());
                            };
                            let _ = jsonb_array_count_in_slice(blob, i_root + n, i_root + n + sz)?;
                            return Ok(None);
                        }
                    }
                }
                Ok(Some(i_root))
            }
            CompiledArrowInner::Fallback(raw_path) => {
                if let Some(after_dollar) = raw_path.strip_prefix('$') {
                    jsonb_lookup_raw_path(blob, 0, after_dollar, raw_path)
                } else if raw_path.len() >= 3
                    && raw_path.starts_with('[')
                    && raw_path.ends_with(']')
                {
                    jsonb_lookup_raw_path(blob, 0, raw_path, raw_path)
                } else {
                    let quoted = format!(".\"{raw_path}\"");
                    jsonb_lookup_raw_path(blob, 0, &quoted, raw_path)
                }
            }
        }
    }

    #[inline]
    fn eval_on_jsonb_slice(&self, blob: &[u8], as_sql: bool) -> Result<RuntimeValue> {
        let Some(pos) = self.lookup_in_jsonb_slice(blob)? else {
            return Ok(RuntimeValue::plain(Value::Null));
        };
        finish_arrow_node(blob, pos, as_sql)
    }

    #[inline]
    pub fn eval_value(&self, lhs: &Value, as_sql: bool) -> Result<RuntimeValue> {
        let Some(jb_cow) = parse_func_arg_to_jsonb_cow(lhs)? else {
            return Ok(RuntimeValue::plain(Value::Null));
        };
        self.eval_on_jsonb_slice(jb_cow.as_slice(), as_sql)
    }

    /// Zero-`Value` fast integer/boolean/null `->>` extractor directly on a borrowed B-tree
    /// [`crate::RawField`] (`W10` `WHERE (payload ->> '$.active') = 1`, `SUM(payload ->> '$.meta.val')`).
    #[inline]
    pub fn eval_raw_field_int_fast(
        &self,
        field: crate::RawField<'_>,
        as_sql: bool,
    ) -> Result<FastJsonIntOut> {
        if !as_sql {
            return Ok(FastJsonIntOut::Fallback);
        }
        match field {
            crate::RawField::Null => Ok(FastJsonIntOut::Null),
            crate::RawField::Blob(b) if is_p3v2_jsonb(b) => {
                let Some(pos) = self.lookup_in_jsonb_slice(b)? else {
                    return Ok(FastJsonIntOut::Null);
                };
                match b[pos] {
                    P3J_NULL => Ok(FastJsonIntOut::Null),
                    P3J_FALSE => Ok(FastJsonIntOut::Int(0)),
                    P3J_TRUE => Ok(FastJsonIntOut::Int(1)),
                    P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => {
                        match p3v2_read_int(b, pos) {
                            Some(i) => Ok(FastJsonIntOut::Int(i)),
                            None => Err(err_malformed()),
                        }
                    }
                    _ => Ok(FastJsonIntOut::Fallback),
                }
            }
            crate::RawField::Blob(b) if blob_is_jsonb(b) => {
                let Some(pos) = self.lookup_in_jsonb_slice(b)? else {
                    return Ok(FastJsonIntOut::Null);
                };
                let Some((n, sz)) = jsonb_payload_size(b, pos) else {
                    return Err(err_malformed());
                };
                match b[pos] & 0x0f {
                    JSONB_NULL => {
                        if sz != 0 {
                            return Err(err_malformed());
                        }
                        Ok(FastJsonIntOut::Null)
                    }
                    JSONB_TRUE => {
                        if sz != 0 {
                            return Err(err_malformed());
                        }
                        Ok(FastJsonIntOut::Int(1))
                    }
                    JSONB_FALSE => {
                        if sz != 0 {
                            return Err(err_malformed());
                        }
                        Ok(FastJsonIntOut::Int(0))
                    }
                    JSONB_INT => match parse_jsonb_int_i64(&b[pos + n..pos + n + sz])? {
                        Some(i) => Ok(FastJsonIntOut::Int(i)),
                        None => Ok(FastJsonIntOut::Fallback),
                    },
                    _ => Ok(FastJsonIntOut::Fallback),
                }
            }
            _ => Ok(FastJsonIntOut::Fallback),
        }
    }

    /// Evaluates this pre-compiled JSON arrow path directly into an [`crate::InlineVal`] when the
    /// result fits inline (zero `String` allocation for short `JSONB_TEXT` values like `"vip"` in
    /// `W14`), or returns `Ok(Err(Value))` for heap values.
    #[inline]
    pub fn eval_raw_field_inline(
        &self,
        field: crate::RawField<'_>,
        enc: crate::TextEncoding,
        as_sql: bool,
    ) -> Result<std::result::Result<crate::InlineVal, Value>> {
        if as_sql {
            match field {
                crate::RawField::Null => return Ok(Ok(crate::InlineVal::Null)),
                crate::RawField::Blob(b) if is_p3v2_jsonb(b) => {
                    let Some(pos) = self.lookup_in_jsonb_slice(b)? else {
                        return Ok(Ok(crate::InlineVal::Null));
                    };
                    match b[pos] {
                        P3J_NULL => return Ok(Ok(crate::InlineVal::Null)),
                        P3J_TRUE => return Ok(Ok(crate::InlineVal::Integer(1))),
                        P3J_FALSE => return Ok(Ok(crate::InlineVal::Integer(0))),
                        P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => {
                            let i = p3v2_read_int(b, pos).ok_or_else(err_malformed)?;
                            return Ok(Ok(crate::InlineVal::Integer(i)));
                        }
                        P3J_FLOAT_RAW => {
                            if p3v2_node_validity_check(b, pos, 1).0 != 0 {
                                return Err(err_malformed());
                            }
                            let s = &b[pos + 1..pos + 9];
                            let fv = f64::from_le_bytes([
                                s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
                            ]);
                            return Ok(Ok(crate::InlineVal::Real(fv)));
                        }
                        t if t >= P3J_STR_SHORT_BASE => {
                            let slen = usize::from(t & 0x7f);
                            let payload =
                                b.get(pos + 1..pos + 1 + slen).ok_or_else(err_malformed)?;
                            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
                            if let Some(iv) = crate::InlineVal::try_short_text(s) {
                                return Ok(Ok(iv));
                            }
                            return Ok(Err(Value::Text(s.to_owned())));
                        }
                        P3J_STR_PLAIN | P3J_STR_RAW => {
                            let slen = usize::from(u16::from_le_bytes([b[pos + 1], b[pos + 2]]));
                            let payload =
                                b.get(pos + 3..pos + 3 + slen).ok_or_else(err_malformed)?;
                            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
                            if let Some(iv) = crate::InlineVal::try_short_text(s) {
                                return Ok(Ok(iv));
                            }
                            return Ok(Err(Value::Text(s.to_owned())));
                        }
                        _ => {}
                    }
                    let v = p3v2_node_to_sql_value(b, pos, false, false)?.value;
                    return Ok(crate::InlineVal::try_from_value(&v).ok_or(v));
                }
                crate::RawField::Blob(b) if blob_is_jsonb(b) => {
                    let Some(pos) = self.lookup_in_jsonb_slice(b)? else {
                        return Ok(Ok(crate::InlineVal::Null));
                    };
                    let Some((n, sz)) = jsonb_payload_size(b, pos) else {
                        return Err(err_malformed());
                    };
                    let payload = &b[pos + n..pos + n + sz];
                    match b[pos] & 0x0f {
                        JSONB_NULL => {
                            if sz != 0 {
                                return Err(err_malformed());
                            }
                            return Ok(Ok(crate::InlineVal::Null));
                        }
                        JSONB_TRUE => {
                            if sz != 0 {
                                return Err(err_malformed());
                            }
                            return Ok(Ok(crate::InlineVal::Integer(1)));
                        }
                        JSONB_FALSE => {
                            if sz != 0 {
                                return Err(err_malformed());
                            }
                            return Ok(Ok(crate::InlineVal::Integer(0)));
                        }
                        JSONB_INT => {
                            if let Some(i) = parse_jsonb_int_i64(payload)? {
                                return Ok(Ok(crate::InlineVal::Integer(i)));
                            }
                        }
                        JSONB_TEXT | JSONB_TEXTRAW => {
                            let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
                            if let Some(iv) = crate::InlineVal::try_short_text(s) {
                                return Ok(Ok(iv));
                            }
                            return Ok(Err(Value::Text(s.to_owned())));
                        }
                        _ => {}
                    }
                    let v = jsonb_node_to_sql_value(b, pos, false, false)?.value;
                    return Ok(crate::InlineVal::try_from_value(&v).ok_or(v));
                }
                _ => {}
            }
        }
        let v = self.eval_raw_field(field, enc, as_sql)?.value;
        Ok(crate::InlineVal::try_from_value(&v).ok_or(v))
    }

    /// Formats the JSON arrow extraction of `field` directly into `out` as an RFC-8259 JSON array
    /// element (`json_group_array` semantics), borrowing `JSONB_TEXT` slices directly from the
    /// B-tree page with zero `String` / `Vec<u8>` allocations (`W10` Q2).
    #[inline]
    pub fn append_raw_field_json_elem(
        &self,
        field: crate::RawField<'_>,
        enc: crate::TextEncoding,
        as_sql: bool,
        out: &mut String,
    ) -> Result<()> {
        match field {
            crate::RawField::Null => {
                out.push_str("null");
                Ok(())
            }
            crate::RawField::Blob(b) if is_p3v2_jsonb(b) => {
                let Some(pos) = self.lookup_in_jsonb_slice(b)? else {
                    out.push_str("null");
                    return Ok(());
                };
                if !as_sql {
                    let _ = p3v2_node_to_text(b, pos, 1, out)?;
                    return Ok(());
                }
                match b[pos] {
                    P3J_NULL => {
                        out.push_str("null");
                        Ok(())
                    }
                    P3J_TRUE => {
                        out.push('1');
                        Ok(())
                    }
                    P3J_FALSE => {
                        out.push('0');
                        Ok(())
                    }
                    P3J_INT_I8 | P3J_INT_I16 | P3J_INT_I32 | P3J_INT_I64 => {
                        let i = p3v2_read_int(b, pos).ok_or_else(err_malformed)?;
                        let mut buf = [0u8; 24];
                        out.push_str(format_i64_json(i, &mut buf));
                        Ok(())
                    }
                    t if t >= P3J_STR_SHORT_BASE => {
                        let slen = usize::from(t & 0x7f);
                        let payload = b.get(pos + 1..pos + 1 + slen).ok_or_else(err_malformed)?;
                        let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
                        if sqliters_simd::scan_json_plain_ascii_string(payload) == payload.len()
                            || payload
                                .iter()
                                .all(|&c| is_json_plain_ok_byte(c) || c == b'\'')
                        {
                            out.push('"');
                            out.push_str(s);
                            out.push('"');
                        } else {
                            append_escaped_json_string(out, s);
                        }
                        Ok(())
                    }
                    P3J_STR_PLAIN => {
                        let slen = usize::from(u16::from_le_bytes([b[pos + 1], b[pos + 2]]));
                        let payload = b.get(pos + 3..pos + 3 + slen).ok_or_else(err_malformed)?;
                        let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
                        if sqliters_simd::scan_json_plain_ascii_string(payload) == payload.len()
                            || payload
                                .iter()
                                .all(|&c| is_json_plain_ok_byte(c) || c == b'\'')
                        {
                            out.push('"');
                            out.push_str(s);
                            out.push('"');
                        } else {
                            append_escaped_json_string(out, s);
                        }
                        Ok(())
                    }
                    P3J_STR_RAW => {
                        let slen = usize::from(u16::from_le_bytes([b[pos + 1], b[pos + 2]]));
                        let payload = b.get(pos + 3..pos + 3 + slen).ok_or_else(err_malformed)?;
                        let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
                        append_escaped_json_string(out, s);
                        Ok(())
                    }
                    _ => {
                        let rv = p3v2_node_to_sql_value(b, pos, false, false)?;
                        append_runtime_value_json_elem(&rv, out)
                    }
                }
            }
            crate::RawField::Blob(b) if blob_is_jsonb(b) => {
                let Some(pos) = self.lookup_in_jsonb_slice(b)? else {
                    out.push_str("null");
                    return Ok(());
                };
                if !as_sql {
                    let _ = jsonb_node_to_text(b, pos, 1, out)?;
                    return Ok(());
                }
                let Some((n, sz)) = jsonb_payload_size(b, pos) else {
                    return Err(err_malformed());
                };
                let payload = &b[pos + n..pos + n + sz];
                match b[pos] & 0x0f {
                    JSONB_NULL => {
                        if sz != 0 {
                            return Err(err_malformed());
                        }
                        out.push_str("null");
                        Ok(())
                    }
                    JSONB_TRUE => {
                        if sz != 0 {
                            return Err(err_malformed());
                        }
                        out.push('1');
                        Ok(())
                    }
                    JSONB_FALSE => {
                        if sz != 0 {
                            return Err(err_malformed());
                        }
                        out.push('0');
                        Ok(())
                    }
                    JSONB_INT => {
                        if let Some(i) = parse_jsonb_int_i64(payload)? {
                            let mut buf = [0u8; 24];
                            out.push_str(format_i64_json(i, &mut buf));
                            return Ok(());
                        }
                        let rv = jsonb_node_to_sql_value(b, pos, false, false)?;
                        append_runtime_value_json_elem(&rv, out)
                    }
                    JSONB_TEXT | JSONB_TEXTRAW => {
                        let s = std::str::from_utf8(payload).map_err(|_| err_malformed())?;
                        if payload
                            .iter()
                            .all(|&c| is_json_plain_ok_byte(c) || c == b'\'')
                        {
                            out.push('"');
                            out.push_str(s);
                            out.push('"');
                        } else {
                            append_escaped_json_string(out, s);
                        }
                        Ok(())
                    }
                    _ => {
                        let rv = jsonb_node_to_sql_value(b, pos, false, false)?;
                        append_runtime_value_json_elem(&rv, out)
                    }
                }
            }
            other => {
                let rv = self.eval_raw_field(other, enc, as_sql)?;
                append_runtime_value_json_elem(&rv, out)
            }
        }
    }

    /// Evaluates this pre-compiled JSON arrow path directly on a borrowed [`crate::RawField`]
    /// from an on-disk B-tree page with zero `Value::Blob` copy on JSONB columns.
    #[inline]
    pub fn eval_raw_field(
        &self,
        field: crate::RawField<'_>,
        enc: crate::TextEncoding,
        as_sql: bool,
    ) -> Result<RuntimeValue> {
        match field {
            crate::RawField::Null => Ok(RuntimeValue::plain(Value::Null)),
            crate::RawField::Blob(b) => {
                if blob_is_jsonb(b) {
                    self.eval_on_jsonb_slice(b, as_sql)
                } else {
                    let s = std::str::from_utf8(b).map_err(|_| err_malformed())?;
                    let parsed = parse_json_text_to_jsonb(s).map_err(|e| {
                        if e.too_deep {
                            err_too_deep()
                        } else {
                            err_malformed()
                        }
                    })?;
                    self.eval_on_jsonb_slice(&parsed.jsonb, as_sql)
                }
            }
            crate::RawField::Text(b) => {
                if enc == crate::TextEncoding::Utf8
                    && let Ok(s) = std::str::from_utf8(b)
                {
                    let parsed = parse_json_text_to_jsonb(s).map_err(|e| {
                        if e.too_deep {
                            err_too_deep()
                        } else {
                            err_malformed()
                        }
                    })?;
                    self.eval_on_jsonb_slice(&parsed.jsonb, as_sql)
                } else {
                    let owned = Value::Text(enc.decode_bytes(b));
                    self.eval_value(&owned, as_sql)
                }
            }
            crate::RawField::Int(i) => self.eval_value(&Value::Integer(i), as_sql),
            crate::RawField::Float(f) => self.eval_value(&Value::Real(f), as_sql),
        }
    }
}

/// Zero-allocation `->` / `->>` evaluator when the RHS path is already borrowed as `&str`.
pub fn eval_json_arrow_path_str(lhs: &Value, rhs_path: &str, as_sql: bool) -> Result<RuntimeValue> {
    let Some(jb_cow) = parse_func_arg_to_jsonb_cow(lhs)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let blob = jb_cow.as_slice();
    let pos_opt = if let Some(after_dollar) = rhs_path.strip_prefix('$') {
        jsonb_lookup_raw_path(blob, 0, after_dollar, rhs_path)?
    } else if !rhs_path.is_empty()
        && rhs_path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        jsonb_lookup_single_key(blob, 0, rhs_path.as_bytes(), true)?
    } else if rhs_path.len() >= 3 && rhs_path.starts_with('[') && rhs_path.ends_with(']') {
        jsonb_lookup_raw_path(blob, 0, rhs_path, rhs_path)?
    } else {
        let quoted = format!(".\"{rhs_path}\"");
        jsonb_lookup_raw_path(blob, 0, &quoted, rhs_path)?
    };
    let Some(pos) = pos_opt else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    finish_arrow_node(blob, pos, as_sql)
}

/// `->` (`as_sql == false`) and `->>` (`as_sql == true`) operators on JSON/JSON5 text or JSONB blobs.
pub fn eval_json_arrow(lhs: &Value, rhs: &Value, as_sql: bool) -> Result<RuntimeValue> {
    match (lhs, rhs) {
        (Value::Null, _) | (_, Value::Null) => Ok(RuntimeValue::plain(Value::Null)),
        (_, Value::Text(s)) => eval_json_arrow_path_str(lhs, s, as_sql),
        (_, Value::Integer(idx)) => {
            let Some(jb_cow) = parse_func_arg_to_jsonb_cow(lhs)? else {
                return Ok(RuntimeValue::plain(Value::Null));
            };
            let blob = jb_cow.as_slice();
            let Some(pos) = jsonb_lookup_single_array_index(blob, 0, *idx)? else {
                return Ok(RuntimeValue::plain(Value::Null));
            };
            finish_arrow_node(blob, pos, as_sql)
        }
        (_, Value::Real(f)) => {
            let s = format_sqlite_real(*f);
            eval_json_arrow_path_str(lhs, &s, as_sql)
        }
        (_, Value::Blob(b)) => {
            let s = std::str::from_utf8(b).map_err(|_| err_bad_path("<blob>"))?;
            eval_json_arrow_path_str(lhs, s, as_sql)
        }
    }
}

#[inline]
fn jsonb_for_edit<'a>(blob: &'a [u8], scratch: &'a mut Vec<u8>) -> Result<&'a [u8]> {
    if is_p3v2_jsonb(blob) {
        scratch.clear();
        transcode_p3v2_jsonb_to_format3(blob, scratch)?;
        Ok(scratch.as_slice())
    } else {
        Ok(blob)
    }
}

/// `json_insert` / `jsonb_insert` / `json_replace` / `jsonb_replace` / `json_set` / `jsonb_set` /
/// `json_array_insert` / `jsonb_array_insert`
pub fn eval_json_edit(
    func_name: &str,
    args: &[RuntimeValue],
    mode: EditMode,
    as_blob: bool,
) -> Result<RuntimeValue> {
    if args.is_empty() || !(args.len() - 1).is_multiple_of(2) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("{func_name}() requires an odd number of arguments"),
        ));
    }
    let Some(jb_cow) = parse_func_arg_to_jsonb_cow(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let mut f3_scratch = Vec::new();
    let jsonb = jsonb_for_edit(jb_cow.as_slice(), &mut f3_scratch)?;
    let (mut root, parsed_end) = EditNode::from_jsonb(jsonb, 0, 0)?;
    if parsed_end != jsonb.len() {
        return Err(err_malformed());
    }
    let mut val_jsonb = Vec::new();
    for pair in args[1..].as_chunks::<2>().0 {
        if matches!(pair[0].value, Value::Null) {
            continue;
        }
        let path_str = pair[0].value.to_text_lossy();
        if mode == EditMode::ArrayInsert && !path_str.ends_with(']') {
            return Err(err_not_array_element(&path_str));
        }
        let steps = parse_json_path(&path_str)?;
        if mode == EditMode::ArrayInsert
            && !matches!(
                steps.last(),
                Some(PathStep::ArrayIndex(_) | PathStep::ArrayFromEnd(_))
            )
        {
            return Err(err_not_array_element(&path_str));
        }
        val_jsonb.clear();
        append_sql_arg_as_jsonb(&mut val_jsonb, &pair[1])?;
        apply_edit(&mut root, &steps, mode, Some(&val_jsonb));
    }
    let mut out = Vec::with_capacity(jsonb.len().saturating_add(16));
    root.to_jsonb(0, &mut out)?;
    finish_json_or_jsonb(out, as_blob)
}

/// `json_remove(X, P1, P2, ...)` / `jsonb_remove(X, P1, P2, ...)`
pub fn eval_json_remove(args: &[RuntimeValue], as_blob: bool) -> Result<RuntimeValue> {
    if args.is_empty() {
        return Ok(RuntimeValue::plain(Value::Null));
    }
    let Some(jb_cow) = parse_func_arg_to_jsonb_cow(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let mut f3_scratch = Vec::new();
    let jsonb = jsonb_for_edit(jb_cow.as_slice(), &mut f3_scratch)?;
    let (mut root, parsed_end) = EditNode::from_jsonb(jsonb, 0, 0)?;
    if parsed_end != jsonb.len() {
        return Err(err_malformed());
    }
    for path_arg in &args[1..] {
        if matches!(path_arg.value, Value::Null) {
            // In `json.c`, `if( zPath==0 ) return;` -> returns SQL NULL!
            return Ok(RuntimeValue::plain(Value::Null));
        }
        let path_str = path_arg.value.to_text_lossy();
        if path_str == "$" {
            // Removing `$` returns SQL NULL in SQLite (`jsonRemoveFunc`)!
            return Ok(RuntimeValue::plain(Value::Null));
        }
        let steps = parse_json_path(&path_str)?;
        apply_edit(&mut root, &steps, EditMode::Remove, None);
    }
    let mut out = Vec::with_capacity(jsonb.len());
    root.to_jsonb(0, &mut out)?;
    finish_json_or_jsonb(out, as_blob)
}

/// `json_patch(X, Y)` / `jsonb_patch(X, Y)` (RFC 7396 Merge Patch)
pub fn eval_json_patch(
    target_arg: &RuntimeValue,
    patch_arg: &RuntimeValue,
    as_blob: bool,
) -> Result<RuntimeValue> {
    let Some(target_cow) = parse_func_arg_to_jsonb_cow(&target_arg.value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let Some(patch_cow) = parse_func_arg_to_jsonb_cow(&patch_arg.value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let mut t_scratch = Vec::new();
    let mut p_scratch = Vec::new();
    let target_b = jsonb_for_edit(target_cow.as_slice(), &mut t_scratch)?;
    let patch_b = jsonb_for_edit(patch_cow.as_slice(), &mut p_scratch)?;
    let (target_node, tend) = EditNode::from_jsonb(target_b, 0, 0)?;
    if tend != target_b.len() {
        return Err(err_malformed());
    }
    let (patch_node, pend) = EditNode::from_jsonb(patch_b, 0, 0)?;
    if pend != patch_b.len() {
        return Err(err_malformed());
    }
    let merged = merge_patch(target_node, patch_node);
    let mut out = Vec::with_capacity(target_b.len().saturating_add(patch_b.len()));
    merged.to_jsonb(0, &mut out)?;
    finish_json_or_jsonb(out, as_blob)
}

/// `json_pretty(X[, indent])` (`subtype = 0`, matching `jsonPrettyFunc` in `json.c`).
pub fn eval_json_pretty(args: &[RuntimeValue]) -> Result<RuntimeValue> {
    let Some(jb_cow) = parse_func_arg_to_jsonb_cow(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let indent = if args.len() >= 2 && !matches!(args[1].value, Value::Null) {
        args[1].value.to_text_lossy()
    } else {
        "    ".to_owned()
    };
    let pretty = jsonb_to_pretty_text(jb_cow.as_slice(), &indent)?;
    Ok(RuntimeValue::plain(Value::Text(pretty)))
}

// ============================================================================
// Window & Group Aggregate Accumulators (`json_group_array`, `jsonb_group_array`,
// `json_group_object`, `jsonb_group_object`)
// ============================================================================

#[derive(Debug, Clone)]
pub enum JsonGroupAccum {
    Array {
        as_blob: bool,
        items: VecDeque<Vec<u8>>,
    },
    Object {
        as_blob: bool,
        // `None` entries correspond to rows where `NAME IS NULL`, which must be tracked
        // so sliding-window `inverse()` pops in exact FIFO correspondence with `step()`!
        entries: VecDeque<Option<(Vec<u8>, Vec<u8>)>>,
    },
}

impl JsonGroupAccum {
    pub fn new_array(as_blob: bool) -> Self {
        Self::Array {
            as_blob,
            items: VecDeque::new(),
        }
    }

    pub fn new_object(as_blob: bool) -> Self {
        Self::Object {
            as_blob,
            entries: VecDeque::new(),
        }
    }

    pub fn step(&mut self, args: &[RuntimeValue]) -> Result<()> {
        match self {
            Self::Array { items, .. } => {
                let arg = args
                    .first()
                    .cloned()
                    .unwrap_or(RuntimeValue::plain(Value::Null));
                let mut node = Vec::new();
                append_sql_arg_as_jsonb(&mut node, &arg)?;
                items.push_back(node);
                Ok(())
            }
            Self::Object { entries, .. } => {
                if args.is_empty() || matches!(args[0].value, Value::Null) {
                    entries.push_back(None);
                    return Ok(());
                }
                let key_str = args[0].value.to_text_lossy();
                let key_node = encode_key_jsonb(&key_str);
                let val_arg = args
                    .get(1)
                    .cloned()
                    .unwrap_or(RuntimeValue::plain(Value::Null));
                let mut val_node = Vec::new();
                append_sql_arg_as_jsonb(&mut val_node, &val_arg)?;
                entries.push_back(Some((key_node, val_node)));
                Ok(())
            }
        }
    }

    pub fn inverse(&mut self) {
        match self {
            Self::Array { items, .. } => {
                items.pop_front();
            }
            Self::Object { entries, .. } => {
                entries.pop_front();
            }
        }
    }

    pub fn compute(&self) -> Result<RuntimeValue> {
        match self {
            Self::Array { as_blob, items } => {
                let total_len: usize = items.iter().map(Vec::len).sum();
                let mut children = Vec::with_capacity(total_len);
                for item in items {
                    children.extend_from_slice(item);
                }
                let mut out = Vec::with_capacity(children.len() + 5);
                jsonb_append_node(&mut out, JSONB_ARRAY, &children);
                finish_json_or_jsonb(out, *as_blob)
            }
            Self::Object { as_blob, entries } => {
                let mut children = Vec::new();
                for entry in entries.iter().flatten() {
                    children.extend_from_slice(&entry.0);
                    children.extend_from_slice(&entry.1);
                }
                let mut out = Vec::with_capacity(children.len() + 5);
                jsonb_append_node(&mut out, JSONB_OBJECT, &children);
                finish_json_or_jsonb(out, *as_blob)
            }
        }
    }
}

// ============================================================================
// `json_each` / `json_tree` / `jsonb_each` / `jsonb_tree` Row Builder
// ============================================================================

#[derive(Debug, Clone)]
pub struct JsonWalkRow {
    pub key: Value,
    pub value: RuntimeValue,
    pub type_name: &'static str,
    pub atom: Value,
    pub id: i64,
    pub parent: Value,
    pub fullkey: String,
    pub path: String,
}

fn format_fullkey_object_step(parent_fullkey: &str, key: &str) -> String {
    let needs_quotes = key.is_empty()
        || key
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b == b'_'));
    if needs_quotes {
        let mut s = String::with_capacity(parent_fullkey.len() + key.len() + 4);
        s.push_str(parent_fullkey);
        s.push('.');
        append_escaped_json_string(&mut s, key);
        s
    } else {
        format!("{parent_fullkey}.{key}")
    }
}

#[allow(clippy::too_many_arguments)]
fn make_walk_row(
    jsonb: &[u8],
    id_offset: usize,
    val_offset: usize,
    key: Value,
    parent: Value,
    fullkey: String,
    path: String,
    as_blob_containers: bool,
) -> Result<JsonWalkRow> {
    let type_name = jsonb_node_type_name(jsonb, val_offset)?;
    let etype = jsonb[val_offset] & 0x0f;
    let is_container = etype == JSONB_ARRAY || etype == JSONB_OBJECT;
    let mut val_rt = jsonb_node_to_sql_value(
        jsonb,
        val_offset,
        as_blob_containers,
        !as_blob_containers && is_container,
    )?;
    if as_blob_containers
        && is_container
        && let Value::Blob(ref b) = val_rt.value
        && !is_p3v2_jsonb(b)
        && let Some(p3) = try_transcode_format3_to_p3v2(b)
    {
        val_rt.value = Value::Blob(p3);
    }
    let atom = if is_container {
        Value::Null
    } else {
        val_rt.value.clone()
    };
    Ok(JsonWalkRow {
        key,
        value: val_rt,
        type_name,
        atom,
        id: id_offset as i64,
        parent,
        fullkey,
        path,
    })
}

/// Evaluates `json_each` / `json_tree` / `jsonb_each` / `jsonb_tree` into materialized rows.
pub fn build_json_walk_rows(
    json_arg: &Value,
    root_arg: Option<&Value>,
    is_tree: bool,
    as_blob_containers: bool,
) -> Result<Vec<JsonWalkRow>> {
    let mut scratch_jsonb = Vec::new();
    let mut rows = Vec::new();
    build_json_walk_rows_into(
        json_arg,
        root_arg,
        is_tree,
        as_blob_containers,
        &mut scratch_jsonb,
        &mut rows,
    )?;
    Ok(rows)
}

/// Evaluates `json_each` / `json_tree` / `jsonb_each` / `jsonb_tree` into caller-owned reusable
/// `scratch_jsonb` and `rows` buffers, borrowing `JSONB` blobs directly without cloning.
pub fn build_json_walk_rows_into(
    json_arg: &Value,
    root_arg: Option<&Value>,
    is_tree: bool,
    as_blob_containers: bool,
    scratch_jsonb: &mut Vec<u8>,
    rows: &mut Vec<JsonWalkRow>,
) -> Result<()> {
    rows.clear();
    let jsonb: &[u8] = match json_arg {
        Value::Null => return Ok(()),
        Value::Blob(b) if is_p3v2_jsonb(b) => {
            scratch_jsonb.clear();
            transcode_p3v2_jsonb_to_format3(b, scratch_jsonb)?;
            scratch_jsonb.as_slice()
        }
        Value::Blob(b) if blob_is_jsonb(b) => b.as_slice(),
        other => {
            if !eval_jsonb_value_into(other, scratch_jsonb)? {
                return Ok(());
            }
            if is_p3v2_jsonb(scratch_jsonb) {
                let mut f3 = Vec::with_capacity(scratch_jsonb.len());
                transcode_p3v2_jsonb_to_format3(scratch_jsonb, &mut f3)?;
                *scratch_jsonb = f3;
            }
            scratch_jsonb.as_slice()
        }
    };
    let root_path_str = match root_arg {
        None => "$".to_owned(),
        Some(Value::Null) => return Ok(()),
        Some(v) => v.to_text_lossy(),
    };
    let spanned_steps = parse_json_path_with_spans(&root_path_str)?;
    let steps: Vec<PathStep> = spanned_steps
        .iter()
        .map(|(_, _, step)| step.clone())
        .collect();
    let Some((start_id_offset, start_offset)) = jsonb_lookup_steps_with_id(jsonb, 0, &steps)?
    else {
        return Ok(());
    };

    let Some((n, sz)) = jsonb_payload_size(jsonb, start_offset) else {
        return Err(err_malformed());
    };
    let end = start_offset + n + sz;
    let etype = jsonb[start_offset] & 0x0f;

    if !is_tree {
        match etype {
            JSONB_ARRAY => {
                let mut j = start_offset + n;
                let mut idx = 0i64;
                while j < end {
                    let Some((cn, csz)) = jsonb_payload_size(&jsonb[..end], j) else {
                        return Err(err_malformed());
                    };
                    let fk = format!("{root_path_str}[{idx}]");
                    rows.push(make_walk_row(
                        jsonb,
                        j,
                        j,
                        Value::Integer(idx),
                        Value::Null,
                        fk,
                        root_path_str.clone(),
                        as_blob_containers,
                    )?);
                    idx += 1;
                    j += cn + csz;
                }
            }
            JSONB_OBJECT => {
                let mut j = start_offset + n;
                while j < end {
                    let Some((kn, ksz)) = jsonb_payload_size(&jsonb[..end], j) else {
                        return Err(err_malformed());
                    };
                    let kstr = jsonb_decode_string(jsonb, j)?;
                    let raw_vpos = j + kn + ksz;
                    let (vpos, next_j) = if raw_vpos >= jsonb.len() {
                        // `jsonSkipLabel` (`json.c:5393`): when last member has a label
                        // with no value at end of blob, clamp value index to label `j`.
                        (j, end)
                    } else {
                        let Some((vn, vsz)) = jsonb_payload_size(&jsonb[..end], raw_vpos) else {
                            return Err(err_malformed());
                        };
                        (raw_vpos, raw_vpos + vn + vsz)
                    };
                    let fk = format_fullkey_object_step(&root_path_str, &kstr);
                    rows.push(make_walk_row(
                        jsonb,
                        j,
                        vpos,
                        Value::Text(kstr),
                        Value::Null,
                        fk,
                        root_path_str.clone(),
                        as_blob_containers,
                    )?);
                    j = next_j;
                }
            }
            _ => {
                rows.push(make_walk_row(
                    jsonb,
                    start_id_offset,
                    start_offset,
                    Value::Null,
                    Value::Null,
                    root_path_str.clone(),
                    root_path_str,
                    as_blob_containers,
                )?);
            }
        }
        return Ok(());
    }

    let (root_key, root_parent_path) = match spanned_steps.last() {
        None => (Value::Null, root_path_str.clone()),
        Some((s_start, s_end, PathStep::Key(k))) => {
            let parent_path = root_path_str[..*s_start].to_owned();
            let raw_seg = &root_path_str[*s_start..*s_end];
            let key_text =
                if raw_seg.starts_with(".\"") && raw_seg.ends_with('"') && raw_seg.len() >= 3 {
                    raw_seg[2..raw_seg.len() - 1].to_owned()
                } else {
                    k.clone()
                };
            (Value::Text(key_text), parent_path)
        }
        Some((s_start, _, PathStep::ArrayIndex(idx))) => {
            let parent_path = root_path_str[..*s_start].to_owned();
            (Value::Integer(*idx as i64), parent_path)
        }
        Some((s_start, _, PathStep::ArrayFromEnd(_))) => {
            let parent_path = root_path_str[..*s_start].to_owned();
            (Value::Null, parent_path)
        }
    };

    #[allow(clippy::too_many_arguments)]
    fn walk_tree_dfs(
        jsonb: &[u8],
        id_offset: usize,
        val_offset: usize,
        key: Value,
        parent_id: Value,
        fullkey: String,
        path: String,
        depth: usize,
        as_blob_containers: bool,
        rows: &mut Vec<JsonWalkRow>,
    ) -> Result<()> {
        if depth > JSON_MAX_DEPTH {
            return Err(err_too_deep());
        }
        let row = make_walk_row(
            jsonb,
            id_offset,
            val_offset,
            key,
            parent_id,
            fullkey.clone(),
            path,
            as_blob_containers,
        )?;
        let my_id = row.id;
        rows.push(row);

        let Some((n, sz)) = jsonb_payload_size(jsonb, val_offset) else {
            return Err(err_malformed());
        };
        let end = val_offset + n + sz;
        match jsonb[val_offset] & 0x0f {
            JSONB_ARRAY => {
                if depth + 1 > JSON_MAX_DEPTH {
                    return Err(err_too_deep());
                }
                let mut j = val_offset + n;
                let mut idx = 0i64;
                while j < end {
                    let Some((cn, csz)) = jsonb_payload_size(&jsonb[..end], j) else {
                        return Err(err_malformed());
                    };
                    let child_fk = format!("{fullkey}[{idx}]");
                    walk_tree_dfs(
                        jsonb,
                        j,
                        j,
                        Value::Integer(idx),
                        Value::Integer(my_id),
                        child_fk,
                        fullkey.clone(),
                        depth + 1,
                        as_blob_containers,
                        rows,
                    )?;
                    idx += 1;
                    j += cn + csz;
                }
            }
            JSONB_OBJECT => {
                if depth + 1 > JSON_MAX_DEPTH {
                    return Err(err_too_deep());
                }
                let mut j = val_offset + n;
                while j < end {
                    let Some((kn, ksz)) = jsonb_payload_size(&jsonb[..end], j) else {
                        return Err(err_malformed());
                    };
                    let kstr = jsonb_decode_string(jsonb, j)?;
                    let raw_vpos = j + kn + ksz;
                    let (vpos, next_j) = if raw_vpos >= jsonb.len() {
                        (j, end)
                    } else {
                        let Some((vn, vsz)) = jsonb_payload_size(&jsonb[..end], raw_vpos) else {
                            return Err(err_malformed());
                        };
                        (raw_vpos, raw_vpos + vn + vsz)
                    };
                    let child_fk = format_fullkey_object_step(&fullkey, &kstr);
                    walk_tree_dfs(
                        jsonb,
                        j,
                        vpos,
                        Value::Text(kstr),
                        Value::Integer(my_id),
                        child_fk,
                        fullkey.clone(),
                        depth + 1,
                        as_blob_containers,
                        rows,
                    )?;
                    j = next_j;
                }
            }
            _ => {}
        }
        Ok(())
    }

    let canon_fullkey = if steps.is_empty() {
        "$".to_owned()
    } else {
        root_path_str
    };
    walk_tree_dfs(
        jsonb,
        start_id_offset,
        start_offset,
        root_key,
        Value::Null,
        canon_fullkey,
        root_parent_path,
        1,
        as_blob_containers,
        rows,
    )?;
    Ok(())
}

#[cfg(test)]
mod p3v2_jsonb_tests {
    use super::*;
    use crate::{LegacyFormat3Guard, RawField, RuntimeValue, TextEncoding};

    #[test]
    fn p3v2_jsonb_compact_wide_o1_extract_mutators_and_legacy_fallback() {
        let src = r#"{"code":"J-0042","active":true,"meta":{"val":-12345,"grp":7,"pi":3.25},"arr":[10,"x",false,null]}"#;
        let mut jb = Vec::new();
        parse_json_text_to_jsonb_into(src, &mut jb).unwrap();
        assert!(is_p3v2_jsonb(&jb));
        assert_eq!(
            eval_json_valid(&[
                RuntimeValue::plain(Value::Blob(jb.clone())),
                RuntimeValue::plain(Value::Integer(4)),
            ])
            .unwrap()
            .value,
            Value::Integer(1)
        );
        assert_eq!(
            eval_json_valid(&[
                RuntimeValue::plain(Value::Blob(jb.clone())),
                RuntimeValue::plain(Value::Integer(8)),
            ])
            .unwrap()
            .value,
            Value::Integer(1)
        );

        // 1-key and 2-key inline compact lookups + array index lookups.
        let p_code = CompiledJsonArrowPath::compile("$.code");
        let p_active = CompiledJsonArrowPath::compile("$.active");
        let p_val = CompiledJsonArrowPath::compile("$.meta.val");
        let p_arr1 = CompiledJsonArrowPath::compile("$.arr[1]");
        let p_last = CompiledJsonArrowPath::compile("$.arr[#-1]");

        let rf = RawField::Blob(&jb);
        assert_eq!(
            p_code
                .eval_raw_field(rf, TextEncoding::Utf8, true)
                .unwrap()
                .value,
            Value::Text("J-0042".to_owned())
        );
        assert_eq!(
            p_active.eval_raw_field_int_fast(rf, true).unwrap(),
            FastJsonIntOut::Int(1)
        );
        assert_eq!(
            p_val.eval_raw_field_int_fast(rf, true).unwrap(),
            FastJsonIntOut::Int(-12345)
        );
        assert_eq!(
            p_arr1
                .eval_raw_field(rf, TextEncoding::Utf8, true)
                .unwrap()
                .value,
            Value::Text("x".to_owned())
        );
        assert_eq!(
            p_last
                .eval_raw_field(rf, TextEncoding::Utf8, true)
                .unwrap()
                .value,
            Value::Null
        );

        // Short-slice compact object path (`P3J_OBJECT_COMPACT` 0x0f).
        let short_obj = r#"{"code":"J-0001","active":false,"meta":{"val":99,"grp":3}}"#;
        let mut jb_short = Vec::new();
        parse_json_text_to_jsonb_into(short_obj, &mut jb_short).unwrap();
        assert!(is_p3v2_jsonb(&jb_short));
        assert_eq!(jb_short[4], P3J_OBJECT_COMPACT);
        assert_eq!(
            p_val
                .eval_raw_field_int_fast(RawField::Blob(&jb_short), true)
                .unwrap(),
            FastJsonIntOut::Int(99)
        );
        assert_eq!(
            eval_json(&RuntimeValue::plain(Value::Blob(jb_short.clone())), false)
                .unwrap()
                .value,
            Value::Text(short_obj.to_owned())
        );

        // Wide object (>31 keys), escaped keys, duplicate keys, mutators, and walk parity.
        let mut big_members = Vec::new();
        for k in 0..40 {
            big_members.push(format!("\"k{k:02}\":{k}"));
        }
        big_members.push(r#""esc\nkey":"v\t1""#.to_owned());
        big_members.push(r#""dup":1,"dup":2"#.to_owned());
        let wide_src = format!("{{{}}}", big_members.join(","));
        let mut jb_wide = Vec::new();
        parse_json_text_to_jsonb_into(&wide_src, &mut jb_wide).unwrap();
        assert!(is_p3v2_jsonb(&jb_wide));
        assert_eq!(jb_wide[4], P3J_OBJECT);
        assert_eq!(
            CompiledJsonArrowPath::compile("$.k39")
                .eval_raw_field_int_fast(RawField::Blob(&jb_wide), true)
                .unwrap(),
            FastJsonIntOut::Int(39)
        );
        assert_eq!(
            CompiledJsonArrowPath::compile("$.\"esc\nkey\"")
                .eval_raw_field(RawField::Blob(&jb_wide), TextEncoding::Utf8, true)
                .unwrap()
                .value,
            Value::Text("v\t1".to_owned())
        );

        let patched = eval_json_edit(
            "jsonb_set",
            &[
                RuntimeValue::plain(Value::Blob(jb_short.clone())),
                RuntimeValue::plain(Value::Text("$.meta.val".to_owned())),
                RuntimeValue::plain(Value::Integer(777)),
            ],
            EditMode::Set,
            true,
        )
        .unwrap()
        .value;
        let Value::Blob(patched_bytes) = &patched else {
            panic!("expected JSONB blob");
        };
        assert_eq!(
            p_val
                .eval_raw_field_int_fast(RawField::Blob(patched_bytes), true)
                .unwrap(),
            FastJsonIntOut::Int(777)
        );

        let root_meta = Value::Text("$.meta".to_owned());
        let each_rows =
            build_json_walk_rows(&Value::Blob(jb.clone()), Some(&root_meta), false, false).unwrap();
        assert_eq!(each_rows.len(), 3);

        // LegacyFormat3Guard produces canonical Format-3 JSONB that remains readable outside guard.
        let f3_jb = {
            let _g = LegacyFormat3Guard::enter();
            let mut b = Vec::new();
            parse_json_text_to_jsonb_into(short_obj, &mut b).unwrap();
            b
        };
        assert!(!is_p3v2_jsonb(&f3_jb));
        assert_eq!(
            p_val
                .eval_raw_field_int_fast(RawField::Blob(&f3_jb), true)
                .unwrap(),
            FastJsonIntOut::Int(99)
        );

        // Corrupt P3v2 JSONB rejection.
        let mut corrupt = jb_short.clone();
        corrupt[5] = 0; // invalid n_keys == 0 on P3J_OBJECT_COMPACT
        assert_eq!(
            eval_json_valid(&[
                RuntimeValue::plain(Value::Blob(corrupt.clone())),
                RuntimeValue::plain(Value::Integer(8)),
            ])
            .unwrap()
            .value,
            Value::Integer(0)
        );
        assert!(eval_json(&RuntimeValue::plain(Value::Blob(corrupt)), false).is_err());
    }
}
