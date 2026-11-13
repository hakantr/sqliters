//! Floating point <-> decimal text exactly as C SQLite 3.54.1 does it (util.c / printf.c /
//! vdbemem.c): `sqlite3FpDecode`, `sqlite3Fp2Convert10`, `sqlite3Fp10Convert2`, `sqlite3AtoF`,
//! `sqlite3Atoi64` and the `"%!.17g"` rendering of `vdbeMemRenderNum` that every REAL -> TEXT
//! conversion of the engine goes through (CAST, `||`, TEXT affinity, `quote()`, JSON, FTS5
//! tokens, CLI output).
#![forbid(unsafe_code)]

const POWERSOF10_FIRST: i32 = -348;
const POWERSOF10_LAST: i32 = 347;

const BASE: [u64; 27] = [
    0x8000000000000000,
    0xa000000000000000,
    0xc800000000000000,
    0xfa00000000000000,
    0x9c40000000000000,
    0xc350000000000000,
    0xf424000000000000,
    0x9896800000000000,
    0xbebc200000000000,
    0xee6b280000000000,
    0x9502f90000000000,
    0xba43b74000000000,
    0xe8d4a51000000000,
    0x9184e72a00000000,
    0xb5e620f480000000,
    0xe35fa931a0000000,
    0x8e1bc9bf04000000,
    0xb1a2bc2ec5000000,
    0xde0b6b3a76400000,
    0x8ac7230489e80000,
    0xad78ebc5ac620000,
    0xd8d726b7177a8000,
    0x878678326eac9000,
    0xa968163f0a57b400,
    0xd3c21bcecceda100,
    0x84595161401484a0,
    0xa56fa5b99019a5c8,
];

const SCALE: [u64; 26] = [
    0x8049a4ac0c5811ae,
    0xcf42894a5dce35ea,
    0xa76c582338ed2621,
    0x873e4f75e2224e68,
    0xda7f5bf590966848,
    0xb080392cc4349dec,
    0x8e938662882af53e,
    0xe65829b3046b0afa,
    0xba121a4650e4ddeb,
    0x964e858c91ba2655,
    0xf2d56790ab41c2a2,
    0xc428d05aa4751e4c,
    0x9e74d1b791e07e48,
    0xcccccccccccccccc,
    0xcecb8f27f4200f3a,
    0xa70c3c40a64e6c51,
    0x86f0ac99b4e8dafd,
    0xda01ee641a708de9,
    0xb01ae745b101e9e4,
    0x8e41ade9fbebc27d,
    0xe5d3ef282a242e81,
    0xb9a74a0637ce2ee1,
    0x95f83d0a1fb69cd9,
    0xf24a01a73cf2dccf,
    0xc3b8358109e84f07,
    0x9e19db92b4e31ba9,
];

const SCALE_LO: [u32; 26] = [
    0x205b896d, 0x52064cad, 0xaf2af2b8, 0x5a7744a7, 0xaf39a475, 0xbd8d794e, 0x547eb47b, 0x0cb4a5a3,
    0x92f34d62, 0x3a6a07f9, 0xfae27299, 0xaa97e14c, 0x775ea265, 0xcccccccc, 0x00000000, 0x999090b6,
    0x69a028bb, 0xe80e6f48, 0x5ec05dd0, 0x14588f14, 0x8f1668c9, 0x6d953e2c, 0x4abdaf10, 0xbc633b39,
    0x0a862f81, 0x6c07a2c2,
];

/// `sqlite3Multiply128`: `(high 64 bits, low 64 bits)` of `a * b`.
fn multiply128(a: u64, b: u64) -> (u64, u64) {
    let r = u128::from(a) * u128::from(b);
    ((r >> 64) as u64, r as u64)
}

/// `sqlite3Multiply160`: upper 96 bits of `((a << 32) + a_lo) * b` as `(high 64, middle 32)`.
fn multiply160(a: u64, a_lo: u32, b: u64) -> (u64, u32) {
    let mut r = u128::from(a) * u128::from(b);
    r += (u128::from(a_lo) * u128::from(b)) >> 32;
    ((r >> 64) as u64, ((r >> 32) & 0xffff_ffff) as u32)
}

/// `powerOfTen`: the most significant 64 bits of `10^p` and the next 32 bits.
fn power_of_ten(p: i32) -> (u64, u32) {
    let (g, n);
    if p < 0 {
        if p == -1 {
            return (SCALE[13], SCALE_LO[13]);
        }
        let mut gg = p / 27;
        let mut nn = p % 27;
        if nn != 0 {
            gg -= 1;
            nn += 27;
        }
        g = gg;
        n = nn;
    } else if p < 27 {
        return (BASE[p as usize], 0);
    } else {
        g = p / 27;
        n = p % 27;
    }
    let s = SCALE[(g + 13) as usize];
    let s_lo = SCALE_LO[(g + 13) as usize];
    if n == 0 {
        return (s, s_lo);
    }
    let (mut x, mut lo) = multiply160(s, s_lo, BASE[n as usize]);
    if (x & (1u64 << 63)) == 0 {
        x = (x << 1) | u64::from((lo >> 31) & 1);
        lo = (lo << 1) | 1;
    }
    (x, lo)
}

fn pwr10to2(p: i32) -> i32 {
    (p * 108853) >> 15
}

fn pwr2to10(p: i32) -> i32 {
    (p * 78913) >> 18
}

/// `sqlite3Fp2Convert10`: `m * 2^e ~= d * 10^p` with at least `n` significant digits.
fn fp2_convert10(m: u64, e: i32, n: i32) -> (u64, i32) {
    let p = n - 1 - pwr2to10(e + 63);
    let (pw, _) = power_of_ten(p);
    let (mut h, _) = multiply128(m, pw);
    let d = if n == 18 {
        h >>= -(e + pwr10to2(p) + 2);
        (h + ((h << 1) & 2)) >> 1
    } else {
        h >> -(e + pwr10to2(p) + 1)
    };
    (d, -p)
}

/// `sqlite3Fp10Convert2`: the double closest to `d * 10^p`.
fn fp10_convert2(d: u64, p: i32) -> f64 {
    if p < POWERSOF10_FIRST {
        return 0.0;
    }
    if p > POWERSOF10_LAST {
        return f64::INFINITY;
    }
    let b = 64 - d.leading_zeros() as i32;
    let lp = pwr10to2(p);
    let mut e = 53 - b - lp;
    if e > 1074 {
        if e >= 1130 {
            return 0.0;
        }
        e = 1074;
    }
    let s = -(e - (64 - b) + lp + 3);
    let (mut pwr10h, mut pwr10l) = power_of_ten(p);
    if pwr10l != 0 {
        pwr10h = pwr10h.wrapping_add(1);
        pwr10l = !pwr10l;
    }
    let x = d << (64 - b);
    let (mut hi, lo) = multiply128(x, pwr10h);
    let mid1 = (lo >> 32) as u32;
    let mut sticky = 1u64;
    if (hi & ((1u64 << s) - 1)) == 0 {
        let mid2 = (multiply128(x, u64::from(pwr10l) << 32).0 >> 32) as u32;
        sticky = u64::from(mid1.wrapping_sub(mid2) > 1);
        hi = hi.wrapping_sub(u64::from(mid1 < mid2));
    }
    let mut u = (hi >> s) | sticky;
    let adj = u >= (1u64 << 55) - 2;
    if adj {
        u = (u >> 1) | (u & 1);
        e -= 1;
    }
    let mut m = (u + 1 + ((u >> 2) & 1)) >> 2;
    if e <= -972 {
        return f64::INFINITY;
    }
    if (m & (1u64 << 52)) != 0 {
        m = (m & !(1u64 << 52)) | (((1075 - e) as u64) << 52);
    }
    f64::from_bits(m)
}

/// Result of [`fp_decode`] (`struct FpDecode` of sqliteInt.h).
#[derive(Clone, Copy)]
pub struct FpDecode {
    /// `sign == '-'`.
    pub negative: bool,
    /// 0 = finite, 1 = infinity, 2 = NaN (`isSpecial`).
    pub special: u8,
    /// Position of the decimal point relative to the first digit (`iDP`).
    pub dp: i32,
    buf: [u8; 24],
    start: u8,
    n: u8,
}

impl FpDecode {
    /// The significant digits (ASCII, no trailing zeros, at least one unless special).
    #[inline]
    #[must_use]
    pub fn digits(&self) -> &[u8] {
        &self.buf[self.start as usize..(self.start + self.n) as usize]
    }
}

/// `sqlite3FpDecode(&s, r, iRound, mxRound)`.
///
/// `i_round <= 0`: round to `-i_round` digits right of the decimal point (at most `mx_round`
/// significant digits); `i_round > 0`: round to `min(i_round, mx_round)` significant digits.
#[must_use]
pub fn fp_decode(mut r: f64, mut i_round: i32, mx_round: i32) -> FpDecode {
    // Digits are laid down right-aligned so that they end at END; the slots before leave room
    // for the two "one more digit in front" cases of the C routine.
    const END: usize = 22;
    let mut out = FpDecode {
        negative: false,
        special: 0,
        dp: 0,
        buf: [b'0'; 24],
        start: 0,
        n: 0,
    };
    if r < 0.0 {
        out.negative = true;
        r = -r;
    } else if r == 0.0 {
        out.n = 1;
        out.dp = 1;
        return out;
    }
    let bits = r.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i32;
    if e == 0x7ff {
        out.special = if bits == 0x7ff0_0000_0000_0000 { 1 } else { 2 };
        return out;
    }
    let mut v = bits & 0x000f_ffff_ffff_ffff;
    if e == 0 {
        let nn = v.leading_zeros() as i32;
        v <<= nn;
        e = -1074 - nn;
    } else {
        v = (v << 11) | (1u64 << 63);
        e -= 1086;
    }
    let want = if i_round <= 0 || i_round >= 18 {
        18
    } else {
        i_round + 1
    };
    let (mut v10, exp) = fp2_convert10(v, e, want);

    let mut i = END;
    while v10 >= 10 {
        i -= 1;
        out.buf[i] = b'0' + (v10 % 10) as u8;
        v10 /= 10;
    }
    if v10 != 0 {
        i -= 1;
        out.buf[i] = b'0' + v10 as u8;
    }
    let mut n = (END - i) as i32;
    out.dp = n + exp;
    if i_round <= 0 {
        i_round = out.dp - i_round;
        if i_round == 0 && out.buf[i] >= b'5' {
            i_round = 1;
            i -= 1;
            out.buf[i] = b'0';
            n += 1;
            out.dp += 1;
        }
    }
    let mut z = i; // index of the first digit
    if i_round > 0 && (i_round < n || n > mx_round) {
        if i_round > mx_round {
            i_round = mx_round;
        }
        if i_round == 17 && n >= 16 {
            // "%!.17g": try a shorter text that still round-trips (49.47 rather than
            // 49.469999999999999).
            let buf = &out.buf;
            if buf[z + 15] == b'9' && buf[z + 14] == b'9' {
                let mut jj = 14usize;
                while jj > 0 && buf[z + jj - 1] == b'9' {
                    jj -= 1;
                }
                let v2 = if jj == 0 {
                    1u64
                } else {
                    let mut x = u64::from(buf[z] - b'0');
                    for kk in 1..jj {
                        x = x * 10 + u64::from(buf[z + kk] - b'0');
                    }
                    x + 1
                };
                if r == fp10_convert2(v2, exp + n - jj as i32) {
                    i_round = jj as i32 + 1;
                }
            } else if out.dp >= n
                || (buf[z + 15] == b'0' && buf[z + 14] == b'0' && buf[z + 13] == b'0')
            {
                let mut jj = 13usize;
                while jj > 1 && buf[z + jj - 1] == b'0' {
                    jj -= 1;
                }
                let mut v2 = u64::from(buf[z] - b'0');
                for kk in 1..jj {
                    v2 = v2 * 10 + u64::from(buf[z + kk] - b'0');
                }
                if r == fp10_convert2(v2, exp + n - jj as i32) {
                    i_round = jj as i32 + 1;
                }
            }
        }
        n = i_round;
        if out.buf[z + i_round as usize] >= b'5' {
            let mut j = i_round as usize - 1;
            loop {
                out.buf[z + j] += 1;
                if out.buf[z + j] <= b'9' {
                    break;
                }
                out.buf[z + j] = b'0';
                if j == 0 {
                    z -= 1;
                    out.buf[z] = b'1';
                    n += 1;
                    out.dp += 1;
                    break;
                }
                j -= 1;
            }
        }
    }
    while n > 1 && out.buf[z + n as usize - 1] == b'0' {
        n -= 1;
    }
    out.start = z as u8;
    out.n = n as u8;
    out
}

/// Longest text [`write_real_text`] produces (sign, 17 digits, point, `e+308`), rounded up.
pub const REAL_TEXT_MAX: usize = 32;

/// Renders `r` the way `sqlite3_value_text()` does for a REAL (`"%!.17g"`, `vdbeMemRenderNum`)
/// into `out` and returns the number of bytes written: `1.0`, `0.1`, `1.0e+100`,
/// `1000000000000000.0`, `-2.5e-07`, `Inf`, `-Inf`, `NaN`.
#[must_use]
pub fn write_real_text(r: f64, out: &mut [u8; REAL_TEXT_MAX]) -> usize {
    let s = fp_decode(r, 17, 20);
    let mut w = 0usize;
    if s.special != 0 {
        let t: &[u8] = if s.special == 2 {
            b"NaN"
        } else if s.negative {
            b"-Inf"
        } else {
            b"Inf"
        };
        out[..t.len()].copy_from_slice(t);
        return t.len();
    }
    if s.negative {
        out[w] = b'-';
        w += 1;
    }
    let digits = s.digits();
    let n = digits.len();
    let exp = s.dp - 1;
    // etGENERIC with precision 17 and the '!' flag: exponent form outside 1e-4 .. 1e17.
    if !(-4..=16).contains(&exp) {
        out[w] = digits[0];
        out[w + 1] = b'.';
        w += 2;
        if n > 1 {
            out[w..w + n - 1].copy_from_slice(&digits[1..]);
            w += n - 1;
        } else {
            out[w] = b'0';
            w += 1;
        }
        out[w] = b'e';
        w += 1;
        let mut ex = exp;
        if ex < 0 {
            out[w] = b'-';
            ex = -ex;
        } else {
            out[w] = b'+';
        }
        w += 1;
        if ex >= 100 {
            out[w] = b'0' + (ex / 100) as u8;
            w += 1;
            ex %= 100;
        }
        out[w] = b'0' + (ex / 10) as u8;
        out[w + 1] = b'0' + (ex % 10) as u8;
        return w + 2;
    }
    if exp < 0 {
        // 0.000ddd
        out[w] = b'0';
        out[w + 1] = b'.';
        w += 2;
        for _ in 0..(-1 - exp) {
            out[w] = b'0';
            w += 1;
        }
        out[w..w + n].copy_from_slice(digits);
        return w + n;
    }
    let int_len = exp as usize + 1;
    if n <= int_len {
        out[w..w + n].copy_from_slice(digits);
        w += n;
        for _ in n..int_len {
            out[w] = b'0';
            w += 1;
        }
        out[w] = b'.';
        out[w + 1] = b'0';
        return w + 2;
    }
    out[w..w + int_len].copy_from_slice(&digits[..int_len]);
    w += int_len;
    out[w] = b'.';
    w += 1;
    out[w..w + n - int_len].copy_from_slice(&digits[int_len..]);
    w + n - int_len
}

/// The text C SQLite produces for a REAL value (see [`write_real_text`]).
#[must_use]
pub fn real_to_text(r: f64) -> String {
    let mut buf = [0u8; REAL_TEXT_MAX];
    let n = write_real_text(r, &mut buf);
    // Only ASCII is ever written.
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// `sqlite3Isspace`.
#[inline]
const fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// `sqlite3AtoF(z, &r)`: the value of the longest numeric prefix of `z` and C's return code.
///
/// The end of the slice and an embedded NUL both act as the C string terminator.
/// Return code: `> 0` the whole text is a number; `0` no prefix is a number; `< 0` a prefix is a
/// number but other text follows. Low bits (also of the negative codes): 1 = a prefix is
/// valid, 2 = has a decimal point or exponent, 4 = exact zero, 8 = more than ~19 digits.
#[must_use]
pub fn sqlite_atof(z: &[u8]) -> (f64, i32) {
    let at = |i: usize| -> u8 { z.get(i).copied().unwrap_or(0) };
    let mut i = 0usize;
    let mut neg = false;
    let mut s: u64 = 0;
    let mut d: i32 = 0;
    let mut m_state: i32 = 0;
    const LIMIT: u64 = (u64::MAX - 9) / 10;

    while is_space(at(i)) {
        i += 1;
    }
    let mut c = at(i);
    if c == b'-' {
        if at(i + 1).is_ascii_digit() {
            neg = true;
            i += 1;
            c = at(i);
        } else {
            neg = true;
            i += 1;
            c = 0xff; // not a digit: fall through to the '.' test at z[i]
        }
    } else if c == b'+' {
        i += 1;
        c = if at(i).is_ascii_digit() { at(i) } else { 0xff };
    }
    if c.is_ascii_digit() {
        m_state = 1;
        s = u64::from(c - b'0');
        i += 1;
        while at(i).is_ascii_digit() {
            s = s * 10 + u64::from(at(i) - b'0');
            i += 1;
            if s >= LIMIT {
                m_state = 9;
                while at(i).is_ascii_digit() {
                    i += 1;
                    d += 1;
                }
                break;
            }
        }
    }

    if at(i) == b'.' {
        i += 1;
        if at(i).is_ascii_digit() {
            m_state |= 1;
            loop {
                if s < LIMIT {
                    s = s * 10 + u64::from(at(i) - b'0');
                    d -= 1;
                } else {
                    m_state = 11;
                }
                i += 1;
                if !at(i).is_ascii_digit() {
                    break;
                }
            }
        } else if m_state == 0 {
            return (0.0, 0);
        }
        m_state |= 2;
    } else if m_state == 0 {
        return (0.0, 0);
    }

    if at(i) == b'e' || at(i) == b'E' {
        i += 1;
        let esign;
        if at(i) == b'-' {
            esign = -1;
            i += 1;
        } else {
            esign = 1;
            if at(i) == b'+' {
                i += 1;
            }
        }
        if at(i).is_ascii_digit() {
            let mut exp = i32::from(at(i) - b'0');
            i += 1;
            m_state |= 2;
            while at(i).is_ascii_digit() {
                exp = if exp < 10000 {
                    exp * 10 + i32::from(at(i) - b'0')
                } else {
                    10000
                };
                i += 1;
            }
            d += esign * exp;
        } else {
            // Leave z[i] at 'e', '+' or '-' so that the return code is negative.
            i -= 1;
        }
    }

    let mut r;
    if s == 0 {
        r = 0.0;
        m_state |= 4;
    } else {
        r = fp10_convert2(s, d);
    }
    if neg {
        r = -r;
    }
    if at(i) == 0 {
        return (r, m_state);
    }
    if is_space(at(i)) {
        while is_space(at(i)) {
            i += 1;
        }
        if at(i) == 0 {
            return (r, m_state);
        }
    }
    (r, (0xffff_fff0u32 | m_state as u32) as i32)
}

/// `sqlite3Atoi64(z, &v, n, SQLITE_UTF8)`: value and return code
/// (-1 no integer prefix, 0 ok, 1 trailing non-space text, 2 too large / malformed,
/// 3 exactly 9223372036854775808 without a minus sign). The whole slice is the string
/// (a NUL byte is ordinary trailing text, as in C).
#[must_use]
pub fn sqlite_atoi64(z: &[u8]) -> (i64, i32) {
    let end = z.len();
    let mut p = 0usize;
    while p < end && is_space(z[p]) {
        p += 1;
    }
    let mut neg = false;
    if p < end {
        if z[p] == b'-' {
            neg = true;
            p += 1;
        } else if z[p] == b'+' {
            p += 1;
        }
    }
    let z_start = p;
    while p < end && z[p] == b'0' {
        p += 1;
    }
    let mut u: u64 = 0;
    let mut i = 0usize;
    while p + i < end && z[p + i].is_ascii_digit() {
        u = u.wrapping_mul(10).wrapping_add(u64::from(z[p + i] - b'0'));
        i += 1;
    }
    let mut num = if u > i64::MAX as u64 {
        if neg { i64::MIN } else { i64::MAX }
    } else if neg {
        -(u as i64)
    } else {
        u as i64
    };
    let mut rc = 0;
    if i == 0 && z_start == p {
        rc = -1;
    } else if p + i < end {
        let mut jj = p + i;
        while jj < end {
            if !is_space(z[jj]) {
                rc = 1;
                break;
            }
            jj += 1;
        }
    }
    if i < 19 {
        return (num, rc);
    }
    let j = if i > 19 {
        1
    } else {
        // compare2pow63
        let pow63 = b"922337203685477580";
        let mut c = 0i32;
        let mut k = 0;
        while c == 0 && k < 18 {
            c = (i32::from(z[p + k]) - i32::from(pow63[k])) * 10;
            k += 1;
        }
        if c == 0 {
            c = i32::from(z[p + 18]) - i32::from(b'8');
        }
        c
    };
    if j < 0 {
        return (num, rc);
    }
    num = if neg { i64::MIN } else { i64::MAX };
    if j > 0 {
        (num, 2)
    } else if neg {
        (num, rc)
    } else {
        (num, 3)
    }
}

#[cfg(test)]
mod tests {
    use super::{real_to_text, sqlite_atof, sqlite_atoi64};

    // Expected strings are `CAST(x AS TEXT)` of C SQLite 3.54.1.
    #[test]
    fn real_to_text_matches_c() {
        for (r, want) in [
            (1e15, "1000000000000000.0"),
            (1e16, "10000000000000000.0"),
            (1e17, "1.0e+17"),
            (1e100, "1.0e+100"),
            (0.1, "0.1"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-2.5e-7, "-2.5e-07"),
            (1e-5, "1.0e-05"),
            (0.0001, "0.0001"),
            (123456789.125, "123456789.125"),
            (3.0e20, "3.0e+20"),
            (100.0, "100.0"),
            (2.0, "2.0"),
            (1.5, "1.5"),
            (999999999999999.9, "999999999999999.88"),
            (1e15 + 0.5, "1000000000000000.5"),
            (1234567890123456.0, "1234567890123456.0"),
            (49.47, "49.47"),
            (0.0, "0.0"),
            (-0.0, "0.0"),
            (5e-324, "4.9406564584124654e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (2.0 / 3.0, "0.66666666666666663"),
            (4611686018427387904.0, "4.6116860184273879e+18"),
            (f64::INFINITY, "Inf"),
            (f64::NEG_INFINITY, "-Inf"),
        ] {
            assert_eq!(real_to_text(r), want, "{r:e}");
        }
    }

    #[test]
    fn atof_and_atoi64_return_codes_match_c() {
        assert_eq!(sqlite_atof(b"1.5"), (1.5, 3));
        assert_eq!(sqlite_atof(b"  12  "), (12.0, 1));
        assert_eq!(sqlite_atof(b"12abc").1, -15);
        assert_eq!(sqlite_atof(b"abc"), (0.0, 0));
        assert_eq!(sqlite_atof(b"."), (0.0, 0));
        assert_eq!(sqlite_atof(b"1e"), (1.0, -15));
        assert_eq!(sqlite_atof(b"0.0"), (0.0, 7));
        assert_eq!(sqlite_atof(b"-.5e1"), (-5.0, 3));
        // More than 19 digits: the tail is ignored (C: 3500000000000000.0, not ...0.5).
        assert_eq!(
            sqlite_atof(b"3500000000000000.2500001").0,
            3_500_000_000_000_000.0
        );
        assert_eq!(sqlite_atoi64(b"123"), (123, 0));
        assert_eq!(sqlite_atoi64(b" -7 "), (-7, 0));
        assert_eq!(sqlite_atoi64(b"12x"), (12, 1));
        assert_eq!(sqlite_atoi64(b"x"), (0, -1));
        assert_eq!(sqlite_atoi64(b"9223372036854775808"), (i64::MAX, 3));
        assert_eq!(sqlite_atoi64(b"-9223372036854775808"), (i64::MIN, 0));
        assert_eq!(sqlite_atoi64(b"9223372036854775809"), (i64::MAX, 2));
    }
}
