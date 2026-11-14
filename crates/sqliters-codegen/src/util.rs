//! Small helpers of `util.c` / `build.c` used by the parser and code generator.

use crate::consts::*;

/// `sqlite3Isspace(c)`
#[inline]
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}
/// `sqlite3Isdigit(c)`
#[inline]
pub fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}
/// `sqlite3Isxdigit(c)`
#[inline]
pub fn is_xdigit(c: u8) -> bool {
    c.is_ascii_hexdigit()
}
/// `sqlite3Isquote(c)`: one of `"`, `'`, `[`, `` ` ``
#[inline]
pub fn is_quote(c: u8) -> bool {
    matches!(c, b'"' | b'\'' | b'[' | b'`')
}
/// `sqlite3HexToInt(h)`
#[inline]
pub fn hex_to_int(h: u8) -> u8 {
    let mut h = h;
    h += 9 * (1 & (h >> 6));
    h & 0xf
}

/// `sqlite3Dequote(z)`: convert an SQL-style quoted string into a normal string by removing
/// the quote characters. The text is returned unchanged if it does not start with a quote.
pub fn sqlite3_dequote(z: &str) -> String {
    let b = z.as_bytes();
    if b.is_empty() {
        return String::new();
    }
    let mut quote = b[0];
    if !is_quote(quote) {
        return z.to_owned();
    }
    if quote == b'[' {
        quote = b']';
    }
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 1;
    // (C reads the terminating NUL when the closing quote is missing)
    loop {
        let c = if i < b.len() { b[i] } else { 0 };
        if c == quote {
            let c1 = if i + 1 < b.len() { b[i + 1] } else { 0 };
            if c1 == quote {
                out.push(quote);
                i += 1;
            } else {
                break;
            }
        } else {
            if c == 0 {
                break;
            }
            out.push(c);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `sqlite3StrICmp(a,b)==0`
#[inline]
pub fn str_ieq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// `sqlite3GetInt32(zNum, &value)`: `Some(v)` if `z_num` begins with a 32-bit signed integer
/// (decimal, or hexadecimal `0x…` that fits in 31 bits).
pub fn sqlite3_get_int32(z_num: &str) -> Option<i32> {
    let b = z_num.as_bytes();
    let at = |i: usize| -> u8 { if i < b.len() { b[i] } else { 0 } };
    let mut p = 0usize;
    let mut neg = 0i64;
    if at(0) == b'-' {
        neg = 1;
        p += 1;
    } else if at(0) == b'+' {
        p += 1;
    } else if at(0) == b'0' && (at(1) == b'x' || at(1) == b'X') && is_xdigit(at(2)) {
        let mut u: u32 = 0;
        p += 2;
        while at(p) == b'0' {
            p += 1;
        }
        let mut i = 0;
        while i < 8 && is_xdigit(at(p + i)) {
            u = u.wrapping_mul(16).wrapping_add(hex_to_int(at(p + i)) as u32);
            i += 1;
        }
        if (u & 0x8000_0000) == 0 && !is_xdigit(at(p + i)) {
            return Some(u as i32);
        } else {
            return None;
        }
    }
    if !is_digit(at(p)) {
        return None;
    }
    while at(p) == b'0' {
        p += 1;
    }
    let mut v: i64 = 0;
    let mut i = 0;
    while i < 11 {
        let c = at(p + i);
        if !c.is_ascii_digit() {
            break;
        }
        v = v * 10 + (c - b'0') as i64;
        i += 1;
    }
    if i > 10 {
        return None;
    }
    if v - neg > 2147483647 {
        return None;
    }
    if neg != 0 {
        v = -v;
    }
    Some(v as i32)
}

/// `sqlite3DecOrHexToI64(z, &out)`: returns (value, rc) where rc is 0 success, 1 excess text
/// after the integer, 2 too large or malformed, 3 the special case 9223372036854775808.
pub fn sqlite3_dec_or_hex_to_i64(z: &str) -> (i64, i32) {
    let b = z.as_bytes();
    let at = |i: usize| -> u8 { if i < b.len() { b[i] } else { 0 } };
    if at(0) == b'0' && (at(1) == b'x' || at(1) == b'X') {
        let mut u: u64 = 0;
        let mut i = 2;
        while at(i) == b'0' {
            i += 1;
        }
        let mut k = i;
        while is_xdigit(at(k)) {
            u = u.wrapping_mul(16).wrapping_add(hex_to_int(at(k)) as u64);
            k += 1;
        }
        let out = u as i64;
        if k - i > 16 {
            return (out, 2);
        }
        if at(k) != 0 {
            return (out, 1);
        }
        (out, 0)
    } else {
        // n = strspn(z,"+- \n\t0123456789"); if( z[n] ) n++;
        let mut n = 0;
        while n < b.len() && matches!(b[n], b'+' | b'-' | b' ' | b'\n' | b'\t' | b'0'..=b'9') {
            n += 1;
        }
        if n < b.len() {
            n += 1;
        }
        sqliters_types::fp::sqlite_atoi64(&b[..n])
    }
}

/// `sqlite3HexToBlob(db, z, n)`: convert the hex digits `z` (the text between the quotes of
/// `x'…'`) into a blob.
pub fn sqlite3_hex_to_blob(z: &[u8]) -> Vec<u8> {
    let n = z.len() & !1;
    let mut out = Vec::with_capacity(n / 2);
    let mut i = 0;
    while i < n {
        out.push((hex_to_int(z[i]) << 4) | hex_to_int(z[i + 1]));
        i += 2;
    }
    out
}

/// `sqlite3AffinityType(zIn, 0)`: the column affinity of a declared type.
pub fn sqlite3_affinity_type(z_in: &str) -> u8 {
    let mut h: u32 = 0;
    let mut aff = SQLITE_AFF_NUMERIC;
    for &x in z_in.as_bytes() {
        h = (h << 8).wrapping_add(x.to_ascii_lowercase() as u32);
        if h == u32::from_be_bytes(*b"char") {
            aff = SQLITE_AFF_TEXT;
        } else if h == u32::from_be_bytes(*b"clob") {
            aff = SQLITE_AFF_TEXT;
        } else if h == u32::from_be_bytes(*b"text") {
            aff = SQLITE_AFF_TEXT;
        } else if h == u32::from_be_bytes(*b"blob")
            && (aff == SQLITE_AFF_NUMERIC || aff == SQLITE_AFF_REAL)
        {
            aff = SQLITE_AFF_BLOB;
        } else if h == u32::from_be_bytes(*b"real") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if h == u32::from_be_bytes(*b"floa") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if h == u32::from_be_bytes(*b"doub") && aff == SQLITE_AFF_NUMERIC {
            aff = SQLITE_AFF_REAL;
        } else if (h & 0x00FF_FFFF) == ((b'i' as u32) << 16) + ((b'n' as u32) << 8) + b't' as u32 {
            aff = SQLITE_AFF_INTEGER;
            break;
        }
    }
    aff
}

/// `sqlite3IsNumericAffinity(X)`
#[inline]
pub fn is_numeric_affinity(x: u8) -> bool {
    x >= SQLITE_AFF_NUMERIC
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_int32() {
        assert_eq!(sqlite3_get_int32("0"), Some(0));
        assert_eq!(sqlite3_get_int32("2147483647"), Some(2147483647));
        assert_eq!(sqlite3_get_int32("2147483648"), None);
        assert_eq!(sqlite3_get_int32("-2147483648"), Some(-2147483648));
        assert_eq!(sqlite3_get_int32("0x7fffffff"), Some(0x7fffffff));
        assert_eq!(sqlite3_get_int32("0x80000000"), None);
        assert_eq!(sqlite3_get_int32("00000000000012"), Some(12));
        assert_eq!(sqlite3_get_int32("12abc"), Some(12));
        assert_eq!(sqlite3_get_int32("x"), None);
    }

    #[test]
    fn dequote() {
        assert_eq!(sqlite3_dequote("'it''s'"), "it's");
        assert_eq!(sqlite3_dequote("\"a\"\"b\""), "a\"b");
        assert_eq!(sqlite3_dequote("[a]b]"), "a");
        assert_eq!(sqlite3_dequote("`x`"), "x");
        assert_eq!(sqlite3_dequote("abc"), "abc");
    }

    #[test]
    fn affinity_type() {
        assert_eq!(sqlite3_affinity_type("INTEGER"), SQLITE_AFF_INTEGER);
        assert_eq!(sqlite3_affinity_type("varchar(10)"), SQLITE_AFF_TEXT);
        assert_eq!(sqlite3_affinity_type("BLOB"), SQLITE_AFF_BLOB);
        assert_eq!(sqlite3_affinity_type("double"), SQLITE_AFF_REAL);
        assert_eq!(sqlite3_affinity_type("numeric"), SQLITE_AFF_NUMERIC);
        assert_eq!(sqlite3_affinity_type(""), SQLITE_AFF_NUMERIC);
        assert_eq!(sqlite3_affinity_type("floating point"), SQLITE_AFF_INTEGER);
    }
}
