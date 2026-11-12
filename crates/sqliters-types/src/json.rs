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
    let (hdr_len, sz) = match sz_code {
        0..=11 => (1usize, usize::from(sz_code)),
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

/// Deep JSONB validity check (`jsonbValidityCheck` in `json.c`).
/// Returns `0` if valid, or the 1-based byte offset of the first invalid element/byte.
pub fn jsonb_validity_check(blob: &[u8], i: usize, i_end: usize, depth: usize) -> usize {
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
            let mut j = payload_start;
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

    /// [`Self::skip_ws_and_comments`] with the overwhelmingly common "already at a token" case
    /// answered inline: any ASCII byte above space other than `/` ends the skip immediately.
    #[inline(always)]
    fn skip_ws(&mut self, i: usize) -> std::result::Result<usize, JsonTextParseError> {
        match self.bytes.get(i) {
            Some(&c) if c > b' ' && c < 0x80 && c != b'/' => Ok(i),
            _ => self.skip_ws_and_comments(i),
        }
    }

    fn skip_ws_and_comments(
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
            while j < bytes.len() {
                let c = bytes[j];
                if (0x20..0x80).contains(&c) && c != b'"' && c != b'\\' {
                    j += 1;
                } else {
                    break;
                }
            }
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
        let i = self.skip_ws(start)?;
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
                let mut j = self.skip_ws(i + 1)?;
                if j < self.bytes.len() && self.bytes[j] == b'}' {
                    self.depth -= 1;
                    jsonb_append_node(self.out, JSONB_OBJECT, &[]);
                    return Ok(j + 1);
                }
                let hdr_pos = self.out.len();
                self.out.extend_from_slice(&[0, 0]);
                let res = (|| -> std::result::Result<usize, JsonTextParseError> {
                    loop {
                        j = self.skip_ws(j)?;
                        if j >= self.bytes.len() {
                            return Err(self.err(j));
                        }
                        if self.bytes[j] == b'"' || self.bytes[j] == b'\'' {
                            j = self.parse_string(j)?;
                        } else {
                            j = self.parse_unquoted_key(j)?;
                        }
                        j = self.skip_ws(j)?;
                        if j >= self.bytes.len() || self.bytes[j] != b':' {
                            return Err(self.err(j));
                        }
                        j = self.parse_value(j + 1)?;
                        j = self.skip_ws(j)?;
                        if j >= self.bytes.len() {
                            return Err(self.err(j));
                        }
                        if self.bytes[j] == b',' {
                            let after_comma = self.skip_ws(j + 1)?;
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
                let mut j = self.skip_ws(i + 1)?;
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
                        j = self.skip_ws(j)?;
                        if j >= self.bytes.len() {
                            return Err(self.err(j));
                        }
                        if self.bytes[j] == b',' {
                            let after_comma = self.skip_ws(j + 1)?;
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

/// Parses JSON or JSON5 text into `out` as canonical SQLite 3.54.1 JSONB bytes, returning
/// `Ok(has_nonstd)` without allocating temporary container buffers.
pub fn parse_json_text_to_jsonb_into(
    src: &str,
    out: &mut Vec<u8>,
) -> std::result::Result<bool, JsonTextParseError> {
    let mut parser = TextParser::new(src, out);
    let end = parser.parse_value(0)?;
    let tail = parser.skip_ws(end)?;
    if tail != parser.bytes.len() {
        return Err(parser.err(tail));
    }
    Ok(parser.has_nonstd)
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
    for ch in s.chars() {
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

/// Renders the JSONB element starting at `blob[offset]` into canonical RFC-8259 JSON text,
/// returning the end byte offset (`offset + n + sz`).
pub fn jsonb_node_to_text(
    blob: &[u8],
    offset: usize,
    depth: usize,
    out: &mut String,
) -> Result<usize> {
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
/// `jsonFunctionArgToBlob` (json.c): the value argument of `json_set` / `json_insert` /
/// `json_replace` / `json_array_insert`. An SQL text that is not JSON becomes a `TEXTRAW`
/// element whatever it contains (the functions that build their result as JSON text first, such
/// as `jsonb_array`, let the parser pick `TEXT` for plain strings).
fn append_edit_arg_as_jsonb(out: &mut Vec<u8>, arg: &RuntimeValue) -> Result<()> {
    if let Value::Text(s) = &arg.value
        && u32::from(arg.subtype) != JSON_SUBTYPE
    {
        crate::dtrace!("TR|jsonb|argtext|o=TEXTRAW");
        jsonb_append_node(out, JSONB_TEXTRAW, s.as_bytes());
        return Ok(());
    }
    append_sql_arg_as_jsonb(out, arg)
}

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
                    Ok(parsed) => out.extend_from_slice(&parsed.jsonb),
                    Err(e) if e.too_deep => return Err(err_too_deep()),
                    Err(_) => return Err(err_malformed()),
                }
            } else {
                let plain = s.bytes().all(|b| is_json_plain_ok_byte(b) || b == b'\'');
                let etype = if plain { JSONB_TEXT } else { JSONB_TEXTRAW };
                jsonb_append_node(out, etype, s.as_bytes());
            }
        }
        Value::Blob(b) => {
            if blob_is_jsonb(b) {
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
        if jsonb_label_compare(key_bytes, raw_key, &blob[k..k + ksz], raw_label) {
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
                let mut tmp = String::new();
                jsonb_node_to_text(blob, offset, depth, &mut tmp)?;
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

/// Label of an object member created by a path edit (`jsonLookupStep`): `TEXTRAW`. (C writes
/// `TEXT5` with the escaped spelling when the path quotes the key and it contains a backslash;
/// the path steps here hold the unescaped key, so that spelling is not reproduced.)
fn encode_new_label_jsonb(key: &str) -> Vec<u8> {
    let mut raw = Vec::with_capacity(key.len() + 2);
    crate::dtrace!("TR|jsonb|label|o=TEXTRAW");
    jsonb_append_node(&mut raw, JSONB_TEXTRAW, key.as_bytes());
    raw
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
                encode_new_label_jsonb(k),
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
                                encode_new_label_jsonb(k),
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
                entries.push((k.clone(), encode_new_label_jsonb(k), sub));
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
            let Some((n, sz)) = jsonb_payload_size(jsonb, pos) else {
                return Err(err_malformed());
            };
            children.extend_from_slice(&jsonb[pos..pos + n + sz]);
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

/// Pre-parsed step for [`CompiledJsonArrowPath`].
#[derive(Debug, Clone)]
enum CompiledArrowStep {
    Key { bytes: Box<[u8]>, raw_key: bool },
    ArrayIndex(u64),
    ArrayFromEnd(u64),
    PastEnd,
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
    inner: CompiledArrowInner,
}

impl CompiledJsonArrowPath {
    pub fn compile(rhs_path: &str) -> Self {
        if let Some(after_dollar) = rhs_path.strip_prefix('$') {
            if let Some(steps) = Self::try_parse_raw_steps(after_dollar) {
                return Self {
                    inner: CompiledArrowInner::Steps(steps.into_boxed_slice()),
                };
            }
        } else if !rhs_path.is_empty()
            && rhs_path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Self {
                inner: CompiledArrowInner::Steps(
                    vec![CompiledArrowStep::Key {
                        bytes: rhs_path.as_bytes().into(),
                        raw_key: true,
                    }]
                    .into_boxed_slice(),
                ),
            };
        } else if rhs_path.len() >= 3
            && rhs_path.starts_with('[')
            && rhs_path.ends_with(']')
            && let Some(steps) = Self::try_parse_raw_steps(rhs_path)
        {
            return Self {
                inner: CompiledArrowInner::Steps(steps.into_boxed_slice()),
            };
        } else {
            let quoted = format!(".\"{rhs_path}\"");
            if let Some(steps) = Self::try_parse_raw_steps(&quoted) {
                return Self {
                    inner: CompiledArrowInner::Steps(steps.into_boxed_slice()),
                };
            }
        }
        Self {
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
    fn eval_on_jsonb_slice(&self, blob: &[u8], as_sql: bool) -> Result<RuntimeValue> {
        match &self.inner {
            CompiledArrowInner::Steps(steps) => {
                if blob.is_empty()
                    || (blob[0] & 0x0f) > JSONB_OBJECT
                    || jsonb_payload_size(blob, 0).is_none()
                {
                    return Err(err_malformed());
                }
                let mut i_root = 0usize;
                for step in steps.iter() {
                    match step {
                        CompiledArrowStep::Key { bytes, raw_key } => {
                            let Some(v) = jsonb_lookup_single_key(blob, i_root, bytes, *raw_key)?
                            else {
                                return Ok(RuntimeValue::plain(Value::Null));
                            };
                            i_root = v;
                        }
                        CompiledArrowStep::ArrayIndex(kk) => {
                            if (blob[i_root] & 0x0f) != JSONB_ARRAY {
                                validate_mismatched_node(blob, i_root)?;
                                return Ok(RuntimeValue::plain(Value::Null));
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
                                return Ok(RuntimeValue::plain(Value::Null));
                            };
                            i_root = cpos;
                        }
                        CompiledArrowStep::ArrayFromEnd(nn) => {
                            if (blob[i_root] & 0x0f) != JSONB_ARRAY {
                                validate_mismatched_node(blob, i_root)?;
                                return Ok(RuntimeValue::plain(Value::Null));
                            }
                            let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
                                return Err(err_malformed());
                            };
                            let elem_start = i_root + n;
                            let i_end = elem_start + sz;
                            let cnt = jsonb_array_count_in_slice(blob, elem_start, i_end)?;
                            if *nn > cnt || *nn == 0 {
                                return Ok(RuntimeValue::plain(Value::Null));
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
                                return Ok(RuntimeValue::plain(Value::Null));
                            };
                            i_root = cpos;
                        }
                        CompiledArrowStep::PastEnd => {
                            if (blob[i_root] & 0x0f) != JSONB_ARRAY {
                                validate_mismatched_node(blob, i_root)?;
                                return Ok(RuntimeValue::plain(Value::Null));
                            }
                            let Some((n, sz)) = jsonb_payload_size(blob, i_root) else {
                                return Err(err_malformed());
                            };
                            let _ = jsonb_array_count_in_slice(blob, i_root + n, i_root + n + sz)?;
                            return Ok(RuntimeValue::plain(Value::Null));
                        }
                    }
                }
                finish_arrow_node(blob, i_root, as_sql)
            }
            CompiledArrowInner::Fallback(raw_path) => {
                let pos_opt = if let Some(after_dollar) = raw_path.strip_prefix('$') {
                    jsonb_lookup_raw_path(blob, 0, after_dollar, raw_path)?
                } else if raw_path.len() >= 3
                    && raw_path.starts_with('[')
                    && raw_path.ends_with(']')
                {
                    jsonb_lookup_raw_path(blob, 0, raw_path, raw_path)?
                } else {
                    let quoted = format!(".\"{raw_path}\"");
                    jsonb_lookup_raw_path(blob, 0, &quoted, raw_path)?
                };
                let Some(pos) = pos_opt else {
                    return Ok(RuntimeValue::plain(Value::Null));
                };
                finish_arrow_node(blob, pos, as_sql)
            }
        }
    }

    #[inline]
    pub fn eval_value(&self, lhs: &Value, as_sql: bool) -> Result<RuntimeValue> {
        let Some(jb_cow) = parse_func_arg_to_jsonb_cow(lhs)? else {
            return Ok(RuntimeValue::plain(Value::Null));
        };
        self.eval_on_jsonb_slice(jb_cow.as_slice(), as_sql)
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
    let Some(jsonb) = parse_func_arg_to_jsonb(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let (mut root, parsed_end) = EditNode::from_jsonb(&jsonb, 0, 0)?;
    if parsed_end != jsonb.len() {
        return Err(err_malformed());
    }
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
        let mut val_jsonb = Vec::new();
        append_edit_arg_as_jsonb(&mut val_jsonb, &pair[1])?;
        apply_edit(&mut root, &steps, mode, Some(&val_jsonb));
    }
    let mut out = Vec::new();
    root.to_jsonb(0, &mut out)?;
    finish_json_or_jsonb(out, as_blob)
}

/// `json_remove(X, P1, P2, ...)` / `jsonb_remove(X, P1, P2, ...)`
pub fn eval_json_remove(args: &[RuntimeValue], as_blob: bool) -> Result<RuntimeValue> {
    if args.is_empty() {
        return Ok(RuntimeValue::plain(Value::Null));
    }
    let Some(jsonb) = parse_func_arg_to_jsonb(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let (mut root, parsed_end) = EditNode::from_jsonb(&jsonb, 0, 0)?;
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
    let mut out = Vec::new();
    root.to_jsonb(0, &mut out)?;
    finish_json_or_jsonb(out, as_blob)
}

/// `json_patch(X, Y)` / `jsonb_patch(X, Y)` (RFC 7396 Merge Patch)
pub fn eval_json_patch(
    target_arg: &RuntimeValue,
    patch_arg: &RuntimeValue,
    as_blob: bool,
) -> Result<RuntimeValue> {
    let Some(target_b) = parse_func_arg_to_jsonb(&target_arg.value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let Some(patch_b) = parse_func_arg_to_jsonb(&patch_arg.value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let (target_node, tend) = EditNode::from_jsonb(&target_b, 0, 0)?;
    if tend != target_b.len() {
        return Err(err_malformed());
    }
    let (patch_node, pend) = EditNode::from_jsonb(&patch_b, 0, 0)?;
    if pend != patch_b.len() {
        return Err(err_malformed());
    }
    let merged = merge_patch(target_node, patch_node);
    let mut out = Vec::new();
    merged.to_jsonb(0, &mut out)?;
    finish_json_or_jsonb(out, as_blob)
}

/// `json_pretty(X[, indent])` (`subtype = 0`, matching `jsonPrettyFunc` in `json.c`).
pub fn eval_json_pretty(args: &[RuntimeValue]) -> Result<RuntimeValue> {
    let Some(jsonb) = parse_func_arg_to_jsonb(&args[0].value)? else {
        return Ok(RuntimeValue::plain(Value::Null));
    };
    let indent = if args.len() >= 2 && !matches!(args[1].value, Value::Null) {
        args[1].value.to_text_lossy()
    } else {
        "    ".to_owned()
    };
    let pretty = jsonb_to_pretty_text(&jsonb, &indent)?;
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
    let val_rt = jsonb_node_to_sql_value(
        jsonb,
        val_offset,
        as_blob_containers,
        !as_blob_containers && is_container,
    )?;
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
    let Some(jsonb) = parse_func_arg_to_jsonb(json_arg)? else {
        return Ok(Vec::new());
    };
    let root_path_str = match root_arg {
        None => "$".to_owned(),
        Some(Value::Null) => return Ok(Vec::new()),
        Some(v) => v.to_text_lossy(),
    };
    let spanned_steps = parse_json_path_with_spans(&root_path_str)?;
    let steps: Vec<PathStep> = spanned_steps
        .iter()
        .map(|(_, _, step)| step.clone())
        .collect();
    let Some((start_id_offset, start_offset)) = jsonb_lookup_steps_with_id(&jsonb, 0, &steps)?
    else {
        return Ok(Vec::new());
    };

    let mut rows = Vec::new();
    let Some((n, sz)) = jsonb_payload_size(&jsonb, start_offset) else {
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
                        &jsonb,
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
                    let kstr = jsonb_decode_string(&jsonb, j)?;
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
                        &jsonb,
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
                    &jsonb,
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
        return Ok(rows);
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
        &jsonb,
        start_id_offset,
        start_offset,
        root_key,
        Value::Null,
        canon_fullkey,
        root_parent_path,
        1,
        as_blob_containers,
        &mut rows,
    )?;
    Ok(rows)
}
