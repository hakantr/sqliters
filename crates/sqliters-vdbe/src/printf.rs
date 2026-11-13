//! The `printf()` / `format()` SQL function: a port of `sqlite3_str_vappendf` (printf.c) as it
//! runs with `SQLITE_PRINTF_SQLFUNC` (arguments come from SQL values).
//!
//! Everything works on bytes like the C routine does: width and precision count bytes unless
//! the `!` flag is given, strings end at the first NUL, floating point conversions go through
//! [`sqliters_types::fp_decode`] (`sqlite3FpDecode`).
#![forbid(unsafe_code)]

use sqliters_types::{Value, fp_decode};

/// `SQLITE_LIMIT_LENGTH` default: the accumulator refuses to grow beyond this (`SQLITE_TOOBIG`).
const MX_ALLOC: usize = 1_000_000_000;

/// The accumulated text exceeded `SQLITE_LIMIT_LENGTH` ("string or blob too big").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintfTooBig;

struct Acc {
    out: Vec<u8>,
    too_big: bool,
}

impl Acc {
    /// `sqlite3StrAccumEnlarge` size rule.
    fn room(&mut self, n: usize) -> bool {
        if self.too_big {
            return false;
        }
        if self.out.len().saturating_add(n).saturating_add(1) > MX_ALLOC {
            self.too_big = true;
            self.out = Vec::new();
            return false;
        }
        true
    }
    fn append(&mut self, z: &[u8]) {
        if self.room(z.len()) {
            self.out.extend_from_slice(z);
        }
    }
    fn append_char(&mut self, n: i64, c: u8) {
        if n <= 0 {
            return;
        }
        let n = usize::try_from(n).unwrap_or(usize::MAX);
        if self.room(n) {
            let len = self.out.len();
            self.out.resize(len + n, c);
        }
    }
}

struct Args<'a> {
    args: &'a [Value],
    used: usize,
}

/// `sqlite3_value_text()` of an argument as C sees it: bytes up to the first NUL.
fn c_text(v: &Value) -> Option<std::borrow::Cow<'_, [u8]>> {
    fn cut(b: &[u8]) -> &[u8] {
        match b.iter().position(|&c| c == 0) {
            Some(p) => &b[..p],
            None => b,
        }
    }
    match v {
        Value::Null => None,
        Value::Integer(i) => Some(std::borrow::Cow::Owned(i.to_string().into_bytes())),
        Value::Real(r) => Some(std::borrow::Cow::Owned(
            sqliters_types::real_to_text(*r).into_bytes(),
        )),
        Value::Text(s) => Some(std::borrow::Cow::Borrowed(cut(s.as_bytes()))),
        Value::Blob(b) => Some(std::borrow::Cow::Borrowed(cut(b))),
    }
}

impl<'a> Args<'a> {
    fn int(&mut self) -> i64 {
        match self.args.get(self.used) {
            Some(v) => {
                self.used += 1;
                v.to_integer_lossy()
            }
            None => 0,
        }
    }
    fn double(&mut self) -> f64 {
        match self.args.get(self.used) {
            Some(v) => {
                self.used += 1;
                v.to_real_lossy()
            }
            None => 0.0,
        }
    }
    fn text(&mut self) -> Option<std::borrow::Cow<'a, [u8]>> {
        match self.args.get(self.used) {
            Some(v) => {
                self.used += 1;
                c_text(v)
            }
            None => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Et {
    Radix,
    Float,
    Exp,
    Generic,
    Size,
    Str,
    Percent,
    CharX,
    EscQ,
    EscBigQ,
    Pointer,
    EscW,
    Ordinal,
    Decimal,
    EscJ,
    EscBigJ,
    Invalid,
}

/// `(type, base, signed, uppercase digits / exponent letter, alt-form prefix)` of `fmtinfo[]`.
fn fmt_info(c: u8) -> (Et, u64, bool, bool, &'static [u8]) {
    match c {
        b'd' | b'i' => (Et::Decimal, 10, true, true, b""),
        b'u' => (Et::Decimal, 10, false, true, b""),
        b'e' => (Et::Exp, 0, true, false, b""),
        b'E' => (Et::Exp, 0, true, true, b""),
        b'f' => (Et::Float, 0, true, true, b""),
        b'g' => (Et::Generic, 0, true, false, b""),
        b'G' => (Et::Generic, 0, true, true, b""),
        b'j' => (Et::EscJ, 0, false, true, b""),
        b'J' => (Et::EscBigJ, 0, false, true, b""),
        b'Q' => (Et::EscBigQ, 0, false, true, b""),
        b'q' => (Et::EscQ, 0, false, true, b""),
        b'w' => (Et::EscW, 0, false, true, b""),
        b'p' => (Et::Pointer, 16, false, true, b"0x"),
        b'n' => (Et::Size, 0, false, true, b""),
        b'o' => (Et::Radix, 8, false, true, b"0"),
        b'x' => (Et::Radix, 16, false, false, b"0x"),
        b'X' => (Et::Radix, 16, false, true, b"0X"),
        b'%' => (Et::Percent, 0, false, true, b""),
        b'r' => (Et::Ordinal, 10, true, true, b""),
        b's' | b'z' => (Et::Str, 0, false, true, b""),
        b'c' => (Et::CharX, 0, false, true, b""),
        // 'S' and 'T' are internal-only conversions: they end the rendering like an
        // unknown conversion does.
        _ => (Et::Invalid, 0, false, true, b""),
    }
}

/// Adds to `width` one for every UTF-8 continuation byte of `z` (the `!` flag: width in
/// characters).
fn adjust_width_for_utf8(altform2: bool, width: &mut i64, z: &[u8]) {
    if altform2 && *width > 0 {
        for &b in z {
            if b & 0xc0 == 0x80 {
                *width += 1;
            }
        }
    }
}

/// Renders `fmt` with `args` exactly as the `printf()` SQL function of C SQLite does.
/// `fmt` ends at its first NUL byte.
pub fn sqlite_printf(fmt: &[u8], args: &[Value]) -> Result<Vec<u8>, PrintfTooBig> {
    const DIGITS_UPPER: &[u8; 16] = b"0123456789ABCDEF";
    const DIGITS_LOWER: &[u8; 16] = b"0123456789abcdef";
    let fmt = match fmt.iter().position(|&c| c == 0) {
        Some(p) => &fmt[..p],
        None => fmt,
    };
    let n = fmt.len();
    let at = |i: usize| -> u8 { if i < n { fmt[i] } else { 0 } };
    let mut acc = Acc {
        out: Vec::with_capacity(n + 16),
        too_big: false,
    };
    let mut a = Args { args, used: 0 };
    let mut i = 0usize;
    'outer: while i < n {
        if fmt[i] != b'%' {
            let start = i;
            while i < n && fmt[i] != b'%' {
                i += 1;
            }
            acc.append(&fmt[start..i]);
            if i >= n {
                break;
            }
        }
        i += 1;
        let mut c = at(i);
        if c == 0 {
            acc.append(b"%");
            break;
        }
        let mut left_justify = false;
        let mut flag_prefix = 0u8;
        let mut alternate = false;
        let mut altform2 = false;
        let mut zeropad = false;
        let mut thousand = false;
        let mut width: i64 = 0;
        let mut precision: i64 = -1;
        loop {
            let mut done = false;
            match c {
                b'-' => left_justify = true,
                b'+' => flag_prefix = b'+',
                b' ' => flag_prefix = b' ',
                b'#' => alternate = true,
                b'!' => altform2 = true,
                b'0' => zeropad = true,
                b',' => thousand = true,
                b'l' => {
                    i += 1;
                    c = at(i);
                    if c == b'l' {
                        i += 1;
                        c = at(i);
                    }
                    done = true;
                }
                b'1'..=b'9' => {
                    let mut wx = u32::from(c - b'0');
                    loop {
                        i += 1;
                        c = at(i);
                        if c.is_ascii_digit() {
                            wx = wx.wrapping_mul(10).wrapping_add(u32::from(c - b'0'));
                        } else {
                            break;
                        }
                    }
                    width = i64::from(wx & 0x7fff_ffff);
                    if c != b'.' && c != b'l' {
                        done = true;
                    } else {
                        i -= 1;
                    }
                }
                b'*' => {
                    let w = a.int() as i32;
                    width = i64::from(w);
                    if w < 0 {
                        left_justify = true;
                        width = if w >= -2_147_483_647 {
                            -i64::from(w)
                        } else {
                            0
                        };
                    }
                    let c1 = at(i + 1);
                    if c1 != b'.' && c1 != b'l' {
                        i += 1;
                        c = at(i);
                        done = true;
                    }
                }
                b'.' => {
                    i += 1;
                    c = at(i);
                    if c == b'*' {
                        let p = a.int() as i32;
                        precision = i64::from(p);
                        if p < 0 {
                            precision = if p >= -2_147_483_647 {
                                -i64::from(p)
                            } else {
                                -1
                            };
                        }
                        i += 1;
                        c = at(i);
                    } else {
                        let mut px = 0u32;
                        while c.is_ascii_digit() {
                            px = px.wrapping_mul(10).wrapping_add(u32::from(c - b'0'));
                            i += 1;
                            c = at(i);
                        }
                        precision = i64::from(px & 0x7fff_ffff);
                    }
                    if c == b'l' {
                        i -= 1;
                    } else {
                        done = true;
                    }
                }
                _ => done = true,
            }
            if done {
                break;
            }
            i += 1;
            c = at(i);
            if c == 0 {
                break;
            }
        }
        // `i` is at the conversion character; the next round starts behind it.
        i += 1;
        let (mut xtype, base, signed, upper, alt_prefix) = fmt_info(c);

        // The text of the conversion and its length are produced into `body`; the common
        // tail pads it to `width`.
        let mut body: Vec<u8> = Vec::new();
        match xtype {
            Et::Pointer | Et::Ordinal | Et::Radix | Et::Decimal => {
                if xtype != Et::Decimal {
                    thousand = false;
                }
                let longvalue: u64;
                let prefix: u8;
                if signed {
                    let v = a.int();
                    if v < 0 {
                        longvalue = (!(v as u64)).wrapping_add(1);
                        prefix = b'-';
                    } else {
                        longvalue = v as u64;
                        prefix = flag_prefix;
                    }
                } else {
                    longvalue = a.int() as u64;
                    prefix = 0;
                }
                if longvalue == 0 {
                    alternate = false;
                }
                let has_prefix = i64::from(prefix != 0);
                if zeropad && precision < width - has_prefix {
                    precision = width - has_prefix;
                }
                if precision >= 37 {
                    // printfTempBuf(): a temporary buffer larger than the limit is refused.
                    let mut need = precision as u64 + 10;
                    if thousand {
                        need += precision as u64 / 3;
                    }
                    if need > MX_ALLOC as u64 {
                        return Err(PrintfTooBig);
                    }
                }
                // Digits, least significant first.
                let mut rev: Vec<u8> = Vec::with_capacity(32);
                if xtype == Et::Ordinal {
                    let mut x = (longvalue % 10) as usize;
                    if x >= 4 || (longvalue / 10) % 10 == 1 {
                        x = 0;
                    }
                    let ord = b"thstndrd";
                    rev.push(ord[x * 2 + 1]);
                    rev.push(ord[x * 2]);
                }
                let cset = if upper { DIGITS_UPPER } else { DIGITS_LOWER };
                let mut lv = longvalue;
                loop {
                    rev.push(cset[(lv % base) as usize]);
                    lv /= base;
                    if lv == 0 {
                        break;
                    }
                }
                while (rev.len() as i64) < precision {
                    rev.push(b'0');
                }
                if thousand {
                    let len = rev.len();
                    let mut with: Vec<u8> = Vec::with_capacity(len + len / 3);
                    for (k, &d) in rev.iter().enumerate() {
                        if k > 0 && k % 3 == 0 {
                            with.push(b',');
                        }
                        with.push(d);
                    }
                    rev = with;
                }
                if prefix != 0 {
                    rev.push(prefix);
                }
                if alternate {
                    for &x in alt_prefix.iter().rev() {
                        rev.push(x);
                    }
                }
                rev.reverse();
                body = rev;
            }
            Et::Float | Et::Exp | Et::Generic => {
                let realvalue = a.double();
                if precision < 0 {
                    precision = 6;
                }
                if precision > 100_000_000 {
                    precision = 100_000_000;
                }
                let i_round: i64 = match xtype {
                    Et::Float => -precision,
                    Et::Generic => {
                        if precision == 0 {
                            precision = 1;
                        }
                        precision
                    }
                    _ => precision + 1,
                };
                let s = fp_decode(realvalue, i_round as i32, if altform2 { 20 } else { 16 });
                let mut digits: &[u8] = s.digits();
                let mut dp = s.dp;
                let mut special_done = false;
                if s.special != 0 {
                    if s.special == 2 {
                        body = if zeropad {
                            b"null".to_vec()
                        } else {
                            b"NaN".to_vec()
                        };
                        special_done = true;
                    } else if zeropad {
                        digits = b"9";
                        dp = 1000;
                    } else {
                        body = if s.negative {
                            b"-Inf".to_vec()
                        } else if flag_prefix != 0 {
                            vec![flag_prefix, b'I', b'n', b'f']
                        } else {
                            b"Inf".to_vec()
                        };
                        special_done = true;
                    }
                }
                if !special_done {
                    let prefix = if s.negative {
                        if alternate
                            && flag_prefix == 0
                            && xtype == Et::Float
                            && i64::from(dp) <= i_round
                        {
                            0
                        } else {
                            b'-'
                        }
                    } else {
                        flag_prefix
                    };
                    let mut exp = dp - 1;
                    let flag_rtz;
                    if xtype == Et::Generic {
                        precision -= 1;
                        flag_rtz = !alternate;
                        if exp < -4 || i64::from(exp) > precision {
                            xtype = Et::Exp;
                        } else {
                            precision -= i64::from(exp);
                            xtype = Et::Float;
                        }
                    } else {
                        flag_rtz = altform2;
                    }
                    let mut e2: i64 = if xtype == Et::Exp {
                        0
                    } else {
                        i64::from(dp) - 1
                    };
                    let mut need = e2.max(0) + precision + width + 10;
                    if thousand && e2 > 0 {
                        need += (e2 + 2) / 3;
                    }
                    if !acc.room(usize::try_from(need).unwrap_or(usize::MAX)) {
                        // C: the conversion is dropped, the accumulator is in its error state.
                        continue 'outer;
                    }
                    let mut z: Vec<u8> =
                        Vec::with_capacity(usize::try_from(need.min(4096)).unwrap_or(64));
                    let flag_dp = precision > 0 || alternate || altform2;
                    if prefix != 0 {
                        z.push(prefix);
                    }
                    let sn = digits.len() as i64;
                    let mut j: i64 = 0;
                    if e2 < 0 {
                        z.push(b'0');
                    } else if thousand {
                        while e2 >= 0 {
                            if j < sn {
                                z.push(digits[j as usize]);
                                j += 1;
                            } else {
                                z.push(b'0');
                            }
                            if e2 % 3 == 0 && e2 > 1 {
                                z.push(b',');
                            }
                            e2 -= 1;
                        }
                    } else {
                        j = (e2 + 1).min(sn);
                        z.extend_from_slice(&digits[..j as usize]);
                        e2 -= j;
                        if e2 >= 0 {
                            z.resize(z.len() + (e2 + 1) as usize, b'0');
                            e2 = -1;
                        }
                    }
                    if flag_dp {
                        z.push(b'.');
                    }
                    if e2 < -1 && precision > 0 {
                        let nn = (-1 - e2).min(precision);
                        z.resize(z.len() + nn as usize, b'0');
                        precision -= nn;
                    }
                    if precision > 0 {
                        let nn = (sn - j).min(precision);
                        if nn > 0 {
                            z.extend_from_slice(&digits[j as usize..(j + nn) as usize]);
                            precision -= nn;
                        }
                        if precision > 0 && !flag_rtz {
                            z.resize(z.len() + precision as usize, b'0');
                        }
                    }
                    if flag_rtz && flag_dp {
                        while z.last() == Some(&b'0') {
                            z.pop();
                        }
                        if z.last() == Some(&b'.') {
                            if altform2 {
                                z.push(b'0');
                            } else {
                                z.pop();
                            }
                        }
                    }
                    if xtype == Et::Exp {
                        exp = dp - 1;
                        z.push(if upper { b'E' } else { b'e' });
                        if exp < 0 {
                            z.push(b'-');
                            exp = -exp;
                        } else {
                            z.push(b'+');
                        }
                        if exp >= 100 {
                            z.push(b'0' + (exp / 100) as u8);
                            exp %= 100;
                        }
                        z.push(b'0' + (exp / 10) as u8);
                        z.push(b'0' + (exp % 10) as u8);
                    }
                    let length = z.len() as i64;
                    if length < width {
                        let n_pad = (width - length) as usize;
                        if left_justify {
                            z.resize(z.len() + n_pad, b' ');
                        } else if !zeropad {
                            let mut p = vec![b' '; n_pad];
                            p.extend_from_slice(&z);
                            z = p;
                        } else {
                            let adj = usize::from(prefix != 0);
                            let mut p = Vec::with_capacity(z.len() + n_pad);
                            p.extend_from_slice(&z[..adj]);
                            p.resize(adj + n_pad, b'0');
                            p.extend_from_slice(&z[adj..]);
                            z = p;
                        }
                    }
                    acc.append(&z);
                    continue 'outer;
                }
            }
            Et::Size => {
                width = 0;
            }
            Et::Percent => {
                body.push(b'%');
            }
            Et::CharX => {
                let t = a.text();
                let mut buf: Vec<u8> = Vec::with_capacity(4);
                match t.as_deref() {
                    Some(z) if !z.is_empty() => {
                        buf.push(z[0]);
                        if z[0] & 0xc0 == 0xc0 {
                            let mut k = 1;
                            while buf.len() < 4 && k < z.len() && z[k] & 0xc0 == 0x80 {
                                buf.push(z[k]);
                                k += 1;
                            }
                        }
                    }
                    // NULL and the empty string both give the terminator byte itself.
                    _ => buf.push(0),
                }
                if precision > 1 {
                    width -= precision - 1;
                    if width > 1 && !left_justify {
                        acc.append_char(width - 1, b' ');
                        width = 0;
                    }
                    let copies = precision - 1;
                    let total = (buf.len() as i64).saturating_mul(copies);
                    if acc.room(usize::try_from(total).unwrap_or(usize::MAX)) {
                        for _ in 0..copies {
                            acc.out.extend_from_slice(&buf);
                        }
                    }
                }
                adjust_width_for_utf8(true, &mut width, &buf);
                body = buf;
            }
            Et::Str => {
                let t = a.text();
                let z: &[u8] = t.as_deref().unwrap_or(b"");
                let length = if precision >= 0 {
                    if altform2 {
                        let mut p = 0usize;
                        let mut left = precision;
                        while left > 0 && p < z.len() {
                            left -= 1;
                            let b = z[p];
                            p += 1;
                            if b >= 0xc0 {
                                while p < z.len() && z[p] & 0xc0 == 0x80 {
                                    p += 1;
                                }
                            }
                        }
                        p
                    } else {
                        z.len()
                            .min(usize::try_from(precision).unwrap_or(usize::MAX))
                    }
                } else {
                    z.len()
                };
                adjust_width_for_utf8(altform2, &mut width, &z[..length]);
                body = z[..length].to_vec();
            }
            Et::EscJ | Et::EscBigJ => {
                let t = a.text();
                let i_start = acc.out.len();
                match t.as_deref() {
                    None => {
                        if xtype == Et::EscBigJ {
                            acc.append(b"null");
                        }
                    }
                    Some(esc) => {
                        if xtype == Et::EscBigJ {
                            acc.append(b"\"");
                        }
                        let eat = |k: usize| -> u8 { if k < esc.len() { esc[k] } else { 0 } };
                        let mut px: i64 = precision;
                        if px < 0 {
                            px = 0x7fff_ffff;
                        } else if altform2 {
                            let mut k = 0i64;
                            while k < px && eat(k as usize) != 0 {
                                if eat(k as usize) & 0xc0 == 0x80 {
                                    px += 1;
                                }
                                k += 1;
                            }
                            if k == px {
                                while eat(px as usize) & 0xc0 == 0x80 {
                                    px += 1;
                                }
                            }
                        }
                        let mut j = 0usize;
                        let mut k = 0usize;
                        while (k as i64) < px {
                            let ch = eat(k);
                            if ch <= 0x1f || ch == b'"' || ch == b'\\' {
                                if j < k {
                                    acc.append(&esc[j..k]);
                                }
                                j = k + 1;
                                if ch == 0 {
                                    break;
                                }
                                acc.append(b"\\");
                                if ch > 0x1f {
                                    acc.append(&[ch]);
                                } else if (1u32 << ch) & 0x3700 != 0 {
                                    acc.append(&[b"btn?fr"[(ch - 8) as usize]]);
                                } else {
                                    acc.append(b"u00");
                                    acc.append(&[DIGITS_LOWER[(ch >> 4) as usize]]);
                                    acc.append(&[DIGITS_LOWER[(ch & 0xf) as usize]]);
                                }
                            }
                            k += 1;
                        }
                        if j < k && j < esc.len() {
                            acc.append(&esc[j..k.min(esc.len())]);
                        }
                        if xtype == Et::EscBigJ {
                            acc.append(b"\"");
                        }
                    }
                }
                if width > 0 && !acc.too_big {
                    let nn = (acc.out.len() - i_start) as i64;
                    let mut len = nn;
                    if altform2 && nn > 0 {
                        for &b in &acc.out[i_start..] {
                            if b & 0xc0 == 0x80 {
                                len -= 1;
                            }
                        }
                    }
                    if width > len {
                        let sp = width - len;
                        if !left_justify && nn > 0 {
                            if acc.room(sp as usize) {
                                let tail = acc.out.split_off(i_start);
                                acc.out.resize(i_start + sp as usize, b' ');
                                acc.out.extend_from_slice(&tail);
                            }
                        } else {
                            acc.append_char(sp, b' ');
                        }
                    }
                }
                continue 'outer;
            }
            Et::EscQ | Et::EscBigQ | Et::EscW => {
                let t = a.text();
                let mut need_quote = 0u8;
                let esc: &[u8] = match t.as_deref() {
                    None => {
                        if xtype == Et::EscBigQ {
                            b"NULL"
                        } else {
                            b"(NULL)"
                        }
                    }
                    Some(z) => {
                        if xtype == Et::EscBigQ {
                            need_quote = 1;
                        }
                        z
                    }
                };
                let q = if xtype == Et::EscW {
                    alternate = false;
                    b'"'
                } else {
                    b'\''
                };
                // Number of input bytes used: `precision` bytes (characters with `!`).
                let mut k = precision;
                let mut used = 0usize;
                while k != 0 && used < esc.len() {
                    let ch = esc[used];
                    if altform2 && ch & 0xc0 == 0xc0 {
                        while used + 1 < esc.len() && esc[used + 1] & 0xc0 == 0x80 {
                            used += 1;
                        }
                    }
                    used += 1;
                    k -= 1;
                }
                if alternate {
                    let n_ctrl = esc[..used].iter().filter(|&&b| b <= 0x1f).count();
                    if n_ctrl > 0 || xtype == Et::EscQ {
                        if xtype == Et::EscBigQ {
                            need_quote = 2;
                        }
                    } else {
                        alternate = false;
                    }
                }
                let mut z: Vec<u8> = Vec::with_capacity(used + 16);
                if need_quote == 2 {
                    z.extend_from_slice(b"unistr('");
                } else if need_quote == 1 {
                    z.push(b'\'');
                }
                for &ch in &esc[..used] {
                    z.push(ch);
                    if ch == q {
                        z.push(ch);
                    } else if alternate {
                        if ch == b'\\' {
                            z.push(b'\\');
                        } else if ch <= 0x1f {
                            z.pop();
                            z.extend_from_slice(b"\\u00");
                            z.push(if ch >= 0x10 { b'1' } else { b'0' });
                            z.push(DIGITS_LOWER[(ch & 0xf) as usize]);
                        }
                    }
                }
                if need_quote != 0 {
                    z.push(b'\'');
                    if need_quote == 2 {
                        z.push(b')');
                    }
                }
                adjust_width_for_utf8(altform2, &mut width, &z);
                body = z;
            }
            Et::Invalid => break 'outer,
        }
        let pad = width - body.len() as i64;
        if pad > 0 {
            if !left_justify {
                acc.append_char(pad, b' ');
            }
            acc.append(&body);
            if left_justify {
                acc.append_char(pad, b' ');
            }
        } else {
            acc.append(&body);
        }
    }
    if acc.too_big {
        return Err(PrintfTooBig);
    }
    Ok(acc.out)
}

#[cfg(test)]
mod tests {
    use super::sqlite_printf;
    use sqliters_types::Value;

    fn p(fmt: &str, args: &[Value]) -> String {
        String::from_utf8_lossy(&sqlite_printf(fmt.as_bytes(), args).unwrap()).into_owned()
    }

    // Expected strings are the output of C SQLite 3.54.1 `SELECT printf(...)`.
    #[test]
    fn printf_matches_c() {
        let r = |x: f64| Value::Real(x);
        let i = |x: i64| Value::Integer(x);
        let t = |x: &str| Value::Text(x.to_owned());
        assert_eq!(
            p(
                "%d|%5d|%-5d|%05d|%+d|%,d",
                &[i(42), i(42), i(42), i(42), i(42), i(1234567)]
            ),
            "42|   42|42   |00042|+42|1,234,567"
        );
        assert_eq!(
            p(
                "%x|%X|%#x|%o|%#o|%u",
                &[i(255), i(255), i(255), i(8), i(8), i(-1)]
            ),
            "ff|FF|0xff|10|010|18446744073709551615"
        );
        assert_eq!(
            p(
                "%f|%.2f|%10.3f|%-10.1f|%e|%g",
                &[
                    r(1.5),
                    r(2.345),
                    r(7.38951),
                    r(2.25),
                    r(12345.678),
                    r(0.0001)
                ]
            ),
            "1.500000|2.35|     7.390|2.3       |1.234568e+04|0.0001"
        );
        assert_eq!(
            p(
                "%g|%g|%G|%!g|%#g",
                &[r(1e20), r(123456789.0), r(1e-10), r(2.0), r(2.0)]
            ),
            "1e+20|1.23457e+08|1E-10|2.0|2.00000"
        );
        assert_eq!(
            p("%.17g|%!.17g|%.20e", &[r(0.1), r(0.1), r(0.1)]),
            "0.1|0.1|1.00000000000000000000e-01"
        );
        assert_eq!(
            p(
                "%s|%5s|%-5s|%.2s|%q|%Q|%w",
                &[
                    t("ab"),
                    t("ab"),
                    t("ab"),
                    t("abc"),
                    t("it's"),
                    t("it's"),
                    t("a\"b")
                ]
            ),
            "ab|   ab|ab   |ab|it''s|'it''s'|a\"\"b"
        );
        assert_eq!(
            p(
                "%Q|%q|%s|%d",
                &[Value::Null, Value::Null, Value::Null, Value::Null]
            ),
            "NULL|(NULL)||0"
        );
        assert_eq!(
            p(
                "%c|%3c|%r|%r|%r|%r",
                &[t("xyz"), t("a"), i(1), i(2), i(3), i(11)]
            ),
            "x|  a|1st|2nd|3rd|11th"
        );
        assert_eq!(p("%5%|%y|after", &[]), "    %|");
        assert_eq!(
            p("%f|%010.2f|%+.0f", &[r(f64::INFINITY), r(-1.5), r(2.5)]),
            "Inf|-000001.50|+3"
        );
        assert_eq!(
            p("%!0.17g|%!0.17g", &[r(f64::INFINITY), r(f64::NEG_INFINITY)]),
            "9.0e+999|-9.0e+999"
        );
        assert_eq!(
            p("%,d|%,d|%,.2f", &[i(-1234), i(999), r(1234567.891)]),
            "-1,234|999|1,234,567.89"
        );
    }
}
