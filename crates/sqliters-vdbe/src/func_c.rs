//! Byte-exact ports of scalar SQL functions from C SQLite 3.54.1 `func.c`.
//!
//! The C functions work on `sqlite3_value_text()` / `sqlite3_value_bytes()` (UTF-8 bytes; a
//! BLOB is taken as text in the database encoding) and many of them scan the text as a C
//! string (stop at the first NUL). The ports below keep those rules; a result that C would
//! return as TEXT containing invalid UTF-8 is converted lossily, because a TEXT value of this
//! engine is a Rust `String`.
#![forbid(unsafe_code)]

use std::borrow::Cow;

use sqliters_types::{Collation, ErrorCode, Result, SqliteError, TextEncoding, Value};

use crate::functions::VmRuntimeContext;

/// `sqlite3_value_text()`: UTF-8 bytes of the value, `None` for NULL.
fn value_text<'a>(v: &'a Value, enc: TextEncoding) -> Option<Cow<'a, [u8]>> {
    match v {
        Value::Null => None,
        Value::Integer(i) => Some(Cow::Owned(i.to_string().into_bytes())),
        Value::Real(r) => Some(Cow::Owned(sqliters_types::real_to_text(*r).into_bytes())),
        Value::Text(s) => Some(Cow::Borrowed(s.as_bytes())),
        Value::Blob(b) => {
            if enc == TextEncoding::Utf8 {
                Some(Cow::Borrowed(b.as_slice()))
            } else {
                // The blob is text in the database encoding (UTF-16).
                Some(Cow::Owned(enc.decode_bytes(b).into_bytes()))
            }
        }
    }
}

/// A C string view: the bytes before the first NUL.
fn cstr(z: &[u8]) -> &[u8] {
    match z.iter().position(|&c| c == 0) {
        Some(p) => &z[..p],
        None => z,
    }
}

fn text_value(bytes: Vec<u8>) -> Value {
    Value::Text(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

fn too_big() -> SqliteError {
    SqliteError::new(ErrorCode::TooBig, "string or blob too big")
}

/// `sqlite3Utf8Read`: the code point at the start of `z` and the bytes consumed.
fn utf8_read(z: &[u8]) -> (u32, usize) {
    const TRANS1: [u8; 64] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        0x0d, 0x0e, 0x0f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x01, 0x02, 0x03,
        0x00, 0x01, 0x00, 0x00,
    ];
    let Some(&b0) = z.first() else {
        return (0, 0);
    };
    let mut c = u32::from(b0);
    let mut n = 1;
    if c >= 0xc0 {
        c = u32::from(TRANS1[(c - 0xc0) as usize]);
        while n < z.len() && z[n] & 0xc0 == 0x80 {
            c = (c << 6).wrapping_add(u32::from(0x3f & z[n]));
            n += 1;
        }
        if c < 0x80 || (c & 0xffff_f800) == 0xd800 || (c & 0xffff_fffe) == 0xfffe {
            c = 0xfffd;
        }
    }
    (c, n)
}

/// `SQLITE_SKIP_UTF8`: length of the character starting at `z[i]`.
fn skip_utf8(z: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    if z[i] >= 0xc0 {
        while j < z.len() && z[j] & 0xc0 == 0x80 {
            j += 1;
        }
    }
    j - i
}

fn append_utf8(out: &mut Vec<u8>, c: u32) {
    if c < 0x80 {
        out.push(c as u8);
    } else if c < 0x800 {
        out.push(0xc0 + ((c >> 6) & 0x1f) as u8);
        out.push(0x80 + (c & 0x3f) as u8);
    } else if c < 0x10000 {
        out.push(0xe0 + ((c >> 12) & 0x0f) as u8);
        out.push(0x80 + ((c >> 6) & 0x3f) as u8);
        out.push(0x80 + (c & 0x3f) as u8);
    } else {
        out.push(0xf0 + ((c >> 18) & 0x07) as u8);
        out.push(0x80 + ((c >> 12) & 0x3f) as u8);
        out.push(0x80 + ((c >> 6) & 0x3f) as u8);
        out.push(0x80 + (c & 0x3f) as u8);
    }
}

/// `sqlite3_value_numeric_type()` followed by `sqlite3_value_double()`: `None` unless the
/// value is (or, for text, entirely converts to) a number.
fn numeric_double(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Real(r) => Some(*r),
        Value::Text(s) => match sqliters_types::parse_numeric_text(s)? {
            Value::Integer(i) => Some(i as f64),
            Value::Real(r) => Some(r),
            _ => None,
        },
        Value::Null | Value::Blob(_) => None,
    }
}

/// The text of `quote(X)` / `unistr_quote(X)` (`sqlite3QuoteValue`).
fn quote_value(v: &Value, escape: bool, enc: TextEncoding) -> Result<Vec<u8>> {
    Ok(match v {
        Value::Null => b"NULL".to_vec(),
        Value::Integer(i) => i.to_string().into_bytes(),
        // "%!0.17g": infinity is shown as 9.0e+999.
        Value::Real(r) if r.is_infinite() => {
            if *r > 0.0 {
                b"9.0e+999".to_vec()
            } else {
                b"-9.0e+999".to_vec()
            }
        }
        Value::Real(r) => sqliters_types::real_to_text(*r).into_bytes(),
        Value::Blob(b) => {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            let mut out = Vec::with_capacity(b.len() * 2 + 3);
            out.extend_from_slice(b"X'");
            for &c in b {
                out.push(HEX[(c >> 4) as usize]);
                out.push(HEX[(c & 0xf) as usize]);
            }
            out.push(b'\'');
            out
        }
        Value::Text(_) => {
            let fmt: &[u8] = if escape { b"%#Q" } else { b"%Q" };
            let _ = enc;
            crate::printf::sqlite_printf(fmt, std::slice::from_ref(v)).map_err(|_| too_big())?
        }
    })
}

/// Evaluates the functions ported here; `None` when `upper` is not one of them.
#[allow(clippy::too_many_lines)]
pub(crate) fn eval(upper: &str, args: &[Value], ctx: &VmRuntimeContext) -> Option<Result<Value>> {
    let enc = ctx.text_encoding;
    Some(match upper {
        "OCTET_LENGTH" if args.len() == 1 => Ok(match &args[0] {
            Value::Null => Value::Null,
            Value::Blob(b) => Value::Integer(b.len() as i64),
            Value::Text(s) => {
                if enc == TextEncoding::Utf8 {
                    Value::Integer(s.len() as i64)
                } else {
                    Value::Integer(2 * s.encode_utf16().count() as i64)
                }
            }
            other => {
                let m = if enc == TextEncoding::Utf8 { 1 } else { 2 };
                Value::Integer(other.to_text_lossy().len() as i64 * m)
            }
        }),
        "HEX" if args.len() == 1 => {
            // sqlite3_value_blob(): the bytes as stored, i.e. text in the database encoding.
            let bytes: Cow<'_, [u8]> = match &args[0] {
                Value::Null => Cow::Borrowed(&[][..]),
                Value::Blob(b) => Cow::Borrowed(b.as_slice()),
                Value::Text(s) if enc == TextEncoding::Utf8 => Cow::Borrowed(s.as_bytes()),
                // A number has no stored text: sqlite3_value_blob() renders it as UTF-8.
                Value::Integer(_) | Value::Real(_) => {
                    Cow::Owned(args[0].to_text_lossy().into_bytes())
                }
                other => {
                    let s = other.to_text_lossy();
                    if enc == TextEncoding::Utf8 {
                        Cow::Owned(s.into_bytes())
                    } else {
                        let mut out = Vec::with_capacity(s.len() * 2);
                        for u in s.encode_utf16() {
                            if enc == TextEncoding::Utf16Be {
                                out.extend_from_slice(&u.to_be_bytes());
                            } else {
                                out.extend_from_slice(&u.to_le_bytes());
                            }
                        }
                        Cow::Owned(out)
                    }
                }
            };
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            let mut out = String::with_capacity(bytes.len() * 2);
            for &b in bytes.iter() {
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0x0f) as usize] as char);
            }
            Ok(Value::Text(out))
        }
        "UNHEX" if args.len() == 1 || args.len() == 2 => {
            let Some(hex_full) = value_text(&args[0], enc) else {
                return Some(Ok(Value::Null));
            };
            let pass_full = if args.len() == 2 {
                match value_text(&args[1], enc) {
                    Some(p) => p,
                    None => return Some(Ok(Value::Null)),
                }
            } else {
                Cow::Borrowed(&[][..])
            };
            let z = cstr(&hex_full);
            let pass: &[u8] = &pass_full;
            let contains = |ch: u32| -> bool {
                let mut i = 0;
                while i < pass.len() {
                    let (c, n) = if pass[i] < 0x80 {
                        (u32::from(pass[i]), 1)
                    } else {
                        utf8_read(&pass[i..])
                    };
                    if c == ch {
                        return true;
                    }
                    i += n;
                }
                false
            };
            let hexval = |c: u8| -> u8 { (c as char).to_digit(16).unwrap_or(0) as u8 };
            let mut out = Vec::with_capacity(z.len() / 2 + 1);
            let mut i = 0usize;
            'outer: while i < z.len() {
                while !z[i].is_ascii_hexdigit() {
                    let (ch, n) = if z[i] < 0x80 {
                        (u32::from(z[i]), 1)
                    } else {
                        utf8_read(&z[i..])
                    };
                    i += n;
                    if !contains(ch) {
                        return Some(Ok(Value::Null));
                    }
                    if i >= z.len() {
                        break 'outer;
                    }
                }
                let c = z[i];
                i += 1;
                let d = if i < z.len() { z[i] } else { 0 };
                i += 1;
                if !d.is_ascii_hexdigit() {
                    return Some(Ok(Value::Null));
                }
                out.push((hexval(c) << 4) | hexval(d));
            }
            Ok(Value::Blob(out))
        }
        "UNICODE" if args.len() == 1 => {
            let Some(z) = value_text(&args[0], enc) else {
                return Some(Ok(Value::Null));
            };
            if z.is_empty() || z[0] == 0 {
                Ok(Value::Null)
            } else {
                Ok(Value::Integer(i64::from(utf8_read(&z).0)))
            }
        }
        "CHAR" => {
            let mut out = Vec::with_capacity(args.len() * 4);
            for a in args {
                let mut x = a.to_integer_lossy();
                if !(0..=0x10_ffff).contains(&x) {
                    x = 0xfffd;
                }
                let c = (x & 0x1f_ffff) as u32;
                // A surrogate cannot live in a Rust string: U+FFFD stands in for it.
                if (0xd800..0xe000).contains(&c) {
                    append_utf8(&mut out, 0xfffd);
                } else {
                    append_utf8(&mut out, c);
                }
            }
            Ok(text_value(out))
        }
        "SUBSTR" | "SUBSTRING" if args.len() == 2 || args.len() == 3 => {
            let is_blob = matches!(args[0], Value::Blob(_));
            let mut p1 = args[1].to_integer_lossy();
            let holder;
            let (z, len): (&[u8], i64) = if let Value::Blob(b) = &args[0] {
                if b.is_empty() {
                    // sqlite3_value_blob() of a zero-length blob is a NULL pointer.
                    return Some(Ok(Value::Null));
                }
                (b.as_slice(), b.len() as i64)
            } else {
                match value_text(&args[0], enc) {
                    Some(t) => {
                        holder = t;
                        let z = cstr(&holder);
                        let mut len = 0i64;
                        if p1 < 0 {
                            let mut i = 0;
                            while i < z.len() {
                                i += skip_utf8(z, i);
                                len += 1;
                            }
                        }
                        (z, len)
                    }
                    None => return Some(Ok(Value::Null)),
                }
            };
            let mut p2 = if args.len() == 3 {
                let v = args[2].to_integer_lossy();
                if v == 0 && args[2].is_null() {
                    return Some(Ok(Value::Null));
                }
                v
            } else {
                i64::MAX
            };
            if p1 == 0 && args[1].is_null() {
                return Some(Ok(Value::Null));
            }
            if p1 < 0 {
                p1 += len;
                if p1 < 0 {
                    if p2 < 0 {
                        p2 = 0;
                    } else {
                        p2 += p1;
                    }
                    p1 = 0;
                }
            } else if p1 > 0 {
                p1 -= 1;
            } else if p2 > 0 {
                p2 -= 1;
            }
            if p2 < 0 {
                if p2 < -p1 {
                    p2 = p1;
                } else {
                    p2 = -p2;
                }
                p1 -= p2;
            }
            if is_blob {
                if p1 >= len {
                    p1 = 0;
                    p2 = 0;
                } else if p2 > len - p1 {
                    p2 = len - p1;
                }
                Ok(Value::Blob(z[p1 as usize..(p1 + p2) as usize].to_vec()))
            } else {
                let mut a = 0usize;
                while p1 > 0 && a < z.len() {
                    a += skip_utf8_lead(z, a);
                    p1 -= 1;
                }
                let mut b = a;
                while p2 > 0 && b < z.len() {
                    b += skip_utf8_lead(z, b);
                    p2 -= 1;
                }
                Ok(text_value(z[a..b].to_vec()))
            }
        }
        "REPLACE" if args.len() == 3 => {
            let Some(s) = value_text(&args[0], enc) else {
                return Some(Ok(Value::Null));
            };
            let Some(pat) = value_text(&args[1], enc) else {
                return Some(Ok(Value::Null));
            };
            if pat.is_empty() || pat[0] == 0 {
                return Some(Ok(text_value(s.into_owned())));
            }
            let Some(rep) = value_text(&args[2], enc) else {
                return Some(Ok(Value::Null));
            };
            let (zs, zp, zr): (&[u8], &[u8], &[u8]) = (&s, &pat, &rep);
            let mut out = Vec::with_capacity(zs.len());
            let mut i = 0usize;
            if zp.len() <= zs.len() {
                let limit = zs.len() - zp.len();
                while i <= limit {
                    if zs[i] != zp[0] || &zs[i..i + zp.len()] != zp {
                        out.push(zs[i]);
                        i += 1;
                    } else {
                        if out.len() + zr.len() + (zs.len() - i) > 1_000_000_000 {
                            return Some(Err(too_big()));
                        }
                        out.extend_from_slice(zr);
                        i += zp.len();
                    }
                }
            }
            out.extend_from_slice(&zs[i.min(zs.len())..]);
            Ok(text_value(out))
        }
        "INSTR" if args.len() == 2 => {
            if args[0].is_null() || args[1].is_null() {
                return Some(Ok(Value::Null));
            }
            let both_blob = matches!(args[0], Value::Blob(_)) && matches!(args[1], Value::Blob(_));
            let (h, n): (Cow<'_, [u8]>, Cow<'_, [u8]>) = if both_blob {
                (
                    Cow::Borrowed(args[0].as_bytes().unwrap_or(&[])),
                    Cow::Borrowed(args[1].as_bytes().unwrap_or(&[])),
                )
            } else {
                (
                    value_text(&args[0], enc).unwrap_or(Cow::Borrowed(&[])),
                    value_text(&args[1], enc).unwrap_or(Cow::Borrowed(&[])),
                )
            };
            let (hay, needle): (&[u8], &[u8]) = (&h, &n);
            let mut pos = 1i64;
            if !needle.is_empty() {
                let mut i = 0usize;
                loop {
                    if needle.len() > hay.len() - i {
                        pos = 0;
                        break;
                    }
                    if &hay[i..i + needle.len()] == needle {
                        break;
                    }
                    pos += 1;
                    i += 1;
                    while !both_blob && i < hay.len() && hay[i] & 0xc0 == 0x80 {
                        i += 1;
                    }
                }
            }
            Ok(Value::Integer(pos))
        }
        "TRIM" | "LTRIM" | "RTRIM" if args.len() == 1 || args.len() == 2 => {
            if args[0].is_null() {
                return Some(Ok(Value::Null));
            }
            let Some(input) = value_text(&args[0], enc) else {
                return Some(Ok(Value::Null));
            };
            let set_holder;
            let mut chars: Vec<&[u8]> = Vec::new();
            if args.len() == 1 {
                chars.push(b" ");
            } else {
                match value_text(&args[1], enc) {
                    None => return Some(Ok(Value::Null)),
                    Some(t) => {
                        set_holder = t;
                        let z = cstr(&set_holder);
                        let mut i = 0;
                        while i < z.len() {
                            let n = skip_utf8(z, i);
                            chars.push(&z[i..i + n]);
                            i += n;
                        }
                    }
                }
            }
            let mut z: &[u8] = &input;
            if !chars.is_empty() {
                if upper != "RTRIM" {
                    'l: while !z.is_empty() {
                        for c in &chars {
                            if c.len() <= z.len() && &z[..c.len()] == *c {
                                z = &z[c.len()..];
                                continue 'l;
                            }
                        }
                        break;
                    }
                }
                if upper != "LTRIM" {
                    'r: while !z.is_empty() {
                        for c in &chars {
                            if c.len() <= z.len() && &z[z.len() - c.len()..] == *c {
                                z = &z[..z.len() - c.len()];
                                continue 'r;
                            }
                        }
                        break;
                    }
                }
            }
            Ok(text_value(z.to_vec()))
        }
        "LOWER" | "UPPER" if args.len() == 1 => {
            let Some(z) = value_text(&args[0], enc) else {
                return Some(Ok(Value::Null));
            };
            let mut out = z.into_owned();
            if upper == "LOWER" {
                out.make_ascii_lowercase();
            } else {
                out.make_ascii_uppercase();
            }
            Ok(text_value(out))
        }
        "LIKE" | "GLOB" if args.len() == 2 || (upper == "LIKE" && args.len() == 3) => {
            // likeFunc: the ESCAPE argument is examined first; all three are C strings.
            let mut escape = None;
            if args.len() == 3 {
                let Some(e) = value_text(&args[2], enc) else {
                    return Some(Ok(Value::Null));
                };
                let e = cstr(&e);
                let one_char = !e.is_empty() && skip_utf8(e, 0) == e.len();
                if !one_char {
                    return Some(Err(SqliteError::new(
                        ErrorCode::Error,
                        "ESCAPE expression must be a single character",
                    )));
                }
                escape = char::from_u32(utf8_read(e).0);
            }
            let (Some(pat), Some(txt)) = (value_text(&args[0], enc), value_text(&args[1], enc))
            else {
                return Some(Ok(Value::Null));
            };
            let pat = String::from_utf8_lossy(cstr(&pat));
            let txt = String::from_utf8_lossy(cstr(&txt));
            let matched = if upper == "GLOB" {
                crate::functions::sqlite_glob_match(&pat, &txt)
            } else {
                crate::functions::sqlite_like_match(&pat, &txt, escape)
            };
            Ok(Value::Integer(i64::from(matched)))
        }
        "ROUND" if args.len() == 1 || args.len() == 2 => {
            let mut n = 0i64;
            if args.len() == 2 {
                if args[1].is_null() {
                    return Some(Ok(Value::Null));
                }
                n = args[1].to_integer_lossy().clamp(0, 30);
            }
            if args[0].is_null() {
                return Some(Ok(Value::Null));
            }
            let mut r = args[0].to_real_lossy();
            if !(-4_503_599_627_370_496.0..=4_503_599_627_370_496.0).contains(&r) {
                // No fractional part: nothing to round (also NaN / infinities).
            } else if n == 0 {
                r = ((r + if r < 0.0 { -0.5 } else { 0.5 }) as i64) as f64;
            } else {
                let text =
                    crate::printf::sqlite_printf(b"%!.*f", &[Value::Integer(n), Value::Real(r)])
                        .unwrap_or_default();
                r = sqliters_types::sqlite_atof(&text).0;
            }
            Ok(Value::real(r))
        }
        "MIN" | "MAX" if args.len() >= 2 => {
            // minmaxFunc: min() moves to a later argument on a tie, max() keeps the earlier.
            let is_max = upper == "MAX";
            if args[0].is_null() {
                return Some(Ok(Value::Null));
            }
            let mut best = 0usize;
            for i in 1..args.len() {
                if args[i].is_null() {
                    return Some(Ok(Value::Null));
                }
                let c = args[best].cmp_sqlite(&args[i], Collation::Binary);
                if (is_max && c.is_lt()) || (!is_max && c.is_ge()) {
                    best = i;
                }
            }
            Ok(args[best].clone())
        }
        "LN" | "LOG" | "LOG10" | "LOG2"
            if args.len() == 1 || (upper == "LOG" && args.len() == 2) =>
        {
            let Some(mut x) = numeric_double(&args[0]) else {
                return Some(Ok(Value::Null));
            };
            if x <= 0.0 {
                return Some(Ok(Value::Null));
            }
            let ans = if args.len() == 2 {
                let b = x.ln();
                if b <= 0.0 {
                    return Some(Ok(Value::Null));
                }
                x = args[1].to_real_lossy();
                if x <= 0.0 {
                    return Some(Ok(Value::Null));
                }
                x.ln() / b
            } else {
                match upper {
                    "LN" => x.ln(),
                    "LOG2" => x.log2(),
                    _ => x.log10(),
                }
            };
            Ok(Value::real(ans))
        }
        "CONCAT" | "CONCAT_WS" => {
            let (sep, rest): (Cow<'_, [u8]>, &[Value]) = if upper == "CONCAT_WS" {
                if args.len() < 2 {
                    return None;
                }
                match value_text(&args[0], enc) {
                    Some(s) => (s, &args[1..]),
                    None => return Some(Ok(Value::Null)),
                }
            } else {
                if args.is_empty() {
                    return None;
                }
                (Cow::Borrowed(&[][..]), args)
            };
            let mut out = Vec::new();
            let mut not_null = false;
            for a in rest {
                if let Some(t) = value_text(a, enc) {
                    if not_null && !sep.is_empty() {
                        out.extend_from_slice(&sep);
                    }
                    out.extend_from_slice(&t);
                    not_null = true;
                }
            }
            Ok(text_value(out))
        }
        "UNISTR" if args.len() == 1 => {
            let Some(full) = value_text(&args[0], enc) else {
                return Some(Ok(Value::Null));
            };
            let zin: &[u8] = &full;
            let n_in = zin.len();
            let at = |i: usize| -> u8 { if i < n_in { zin[i] } else { 0 } };
            let n_hex = |i: usize, n: usize| -> Option<u32> {
                let mut v = 0u32;
                for k in 0..n {
                    let d = (at(i + k) as char).to_digit(16)?;
                    v = (v << 4) + d;
                }
                Some(v)
            };
            let err = || SqliteError::new(ErrorCode::Error, "invalid Unicode escape");
            let mut out = Vec::with_capacity(n_in);
            let mut i = 0usize;
            while i < n_in {
                // strchr(): the search for a backslash ends at a NUL.
                let seg_end = zin[i..]
                    .iter()
                    .position(|&c| c == b'\\' || c == 0)
                    .map(|p| i + p);
                let bs = match seg_end {
                    Some(p) if zin[p] == b'\\' => p,
                    _ => {
                        out.extend_from_slice(&zin[i..]);
                        break;
                    }
                };
                out.extend_from_slice(&zin[i..bs]);
                i = bs;
                let c1 = at(i + 1);
                let (v, adv) = if c1 == b'\\' {
                    out.push(b'\\');
                    i += 2;
                    continue;
                } else if c1.is_ascii_hexdigit() {
                    (n_hex(i + 1, 4), 5)
                } else if c1 == b'+' {
                    (n_hex(i + 2, 6), 8)
                } else if c1 == b'u' {
                    (n_hex(i + 2, 4), 6)
                } else if c1 == b'U' {
                    (n_hex(i + 2, 8), 10)
                } else {
                    (None, 0)
                };
                let Some(v) = v else {
                    return Some(Err(err()));
                };
                i += adv;
                // sqlite3AppendOneUtf8Character
                if (0xd800..0xe000).contains(&v) || v > 0x10_ffff {
                    append_utf8(&mut out, 0xfffd);
                } else {
                    append_utf8(&mut out, v);
                }
            }
            Ok(text_value(out))
        }
        "QUOTE" | "UNISTR_QUOTE" if args.len() == 1 => {
            quote_value(&args[0], upper == "UNISTR_QUOTE", enc).map(text_value)
        }
        _ => return None,
    })
}

/// One character forward as `substrFunc` steps: an ASCII byte, or a lead byte with its
/// continuation bytes.
fn skip_utf8_lead(z: &[u8], i: usize) -> usize {
    if z[i] < 0x80 {
        return 1;
    }
    let mut j = i + 1;
    while j < z.len() && z[j] & 0xc0 == 0x80 {
        j += 1;
    }
    j - i
}

#[cfg(test)]
mod tests {
    use super::eval;
    use crate::functions::VmRuntimeContext;
    use sqliters_types::{TextEncoding, Value};

    fn call(name: &str, args: &[Value]) -> Value {
        eval(name, args, &VmRuntimeContext::default())
            .unwrap()
            .unwrap()
    }
    fn t(s: &str) -> Value {
        Value::Text(s.to_owned())
    }
    fn i(v: i64) -> Value {
        Value::Integer(v)
    }

    // Expected values are C SQLite 3.54.1 results.
    #[test]
    fn substr_matches_c() {
        assert_eq!(call("SUBSTR", &[t("NEW"), i(-6)]), t("NEW"));
        assert_eq!(call("SUBSTR", &[t("hello"), i(0), i(3)]), t("he"));
        assert_eq!(call("SUBSTR", &[t("hello"), i(-3), i(2)]), t("ll"));
        assert_eq!(call("SUBSTR", &[t("hello"), i(4), i(-2)]), t("el"));
        assert_eq!(call("SUBSTR", &[t("héllo"), i(2), i(2)]), t("él"));
        assert_eq!(
            call("SUBSTR", &[Value::Blob(vec![1, 2, 3, 4]), i(2), i(2)]),
            Value::Blob(vec![2, 3])
        );
        assert_eq!(
            call("SUBSTR", &[Value::Blob(vec![0]), i(0)]),
            Value::Blob(vec![0])
        );
        assert_eq!(call("SUBSTR", &[t("abc"), Value::Null]), Value::Null);
    }

    #[test]
    fn hex_and_octet_length_follow_the_database_encoding() {
        let mut ctx = VmRuntimeContext::default();
        assert_eq!(eval("HEX", &[t("a")], &ctx).unwrap().unwrap(), t("61"));
        assert_eq!(eval("HEX", &[Value::Null], &ctx).unwrap().unwrap(), t(""));
        ctx.text_encoding = TextEncoding::Utf16Le;
        assert_eq!(
            eval("HEX", &[t("abc")], &ctx).unwrap().unwrap(),
            t("610062006300")
        );
        assert_eq!(eval("OCTET_LENGTH", &[i(-1)], &ctx).unwrap().unwrap(), i(4));
        assert_eq!(
            eval("OCTET_LENGTH", &[t("ab")], &ctx).unwrap().unwrap(),
            i(4)
        );
        ctx.text_encoding = TextEncoding::Utf16Be;
        assert_eq!(eval("HEX", &[t("a")], &ctx).unwrap().unwrap(), t("0061"));
    }

    #[test]
    fn misc_functions_match_c() {
        assert_eq!(
            call("MIN", &[Value::Real(0.0), i(0), Value::Blob(vec![1])]),
            i(0)
        );
        assert_eq!(call("MAX", &[i(0), Value::Real(0.0)]), i(0));
        assert_eq!(call("LN", &[i(0)]), Value::Null);
        assert_eq!(call("LOG", &[i(255), t("+1 day")]), Value::Real(0.0));
        assert_eq!(call("LOG", &[Value::Real(0.5), i(9)]), Value::Null);
        assert_eq!(
            call("ROUND", &[t(" 12 "), Value::Real(1e16)]),
            Value::Real(12.0)
        );
        assert_eq!(call("ROUND", &[Value::Real(2.5)]), Value::Real(3.0));
        assert_eq!(call("ROUND", &[Value::Real(1.005), i(2)]), Value::Real(1.0));
        assert_eq!(call("UNHEX", &[Value::Blob(vec![0])]), Value::Blob(vec![]));
        assert_eq!(call("UNHEX", &[i(10), t("1.0")]), Value::Blob(vec![0x10]));
        assert_eq!(call("UNHEX", &[i(255), t("x")]), Value::Null);
        assert_eq!(call("UNICODE", &[Value::Blob(vec![0])]), Value::Null);
        assert_eq!(call("CONCAT", &[t("a"), Value::Null, i(1)]), t("a1"));
        assert_eq!(
            call("CONCAT_WS", &[t("-"), t("a"), Value::Null, i(1)]),
            t("a-1")
        );
        assert_eq!(call("UNISTR", &[t("a\\0041\\u00e9\\\\")]), t("aAé\\"));
        assert_eq!(call("UNISTR_QUOTE", &[t("a\nb")]), t("unistr('a\\u000ab')"));
        assert_eq!(call("QUOTE", &[Value::Real(f64::INFINITY)]), t("9.0e+999"));
        assert_eq!(call("CHAR", &[i(65), Value::Null, i(-1)]), t("A\0\u{fffd}"));
        assert_eq!(
            call("INSTR", &[Value::Blob(vec![1, 2, 3]), Value::Blob(vec![3])]),
            i(3)
        );
        assert_eq!(call("TRIM", &[t("xxaxx"), t("x")]), t("a"));
        assert_eq!(
            call("REPLACE", &[t("aXbXc"), t("X"), t("--")]),
            t("a--b--c")
        );
    }
}
