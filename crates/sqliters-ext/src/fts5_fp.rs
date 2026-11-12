//! REAL -> TEXT conversion exactly as C SQLite 3.54.1 does it for `sqlite3_value_text()`
//! (`vdbeMemRenderNum`: `"%!.17g"` through `sqlite3FpDecode`, util.c / printf.c).
//!
//! FTS5 tokenizes the text form of a REAL value, so the digits produced here end up as
//! terms in the index and must match C for the two engines to agree on a file.
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

/// Result of `sqlite3FpDecode(r, 17, 20)`.
struct FpDecode {
    negative: bool,
    /// 0 = finite, 1 = infinity, 2 = NaN.
    special: u8,
    digits: Vec<u8>,
    /// Position of the decimal point relative to the first digit (`iDP`).
    dp: i32,
}

/// `sqlite3FpDecode` for `iRound == 17`, `mxRound == 20` (the `"%!.17g"` case).
fn fp_decode17(mut r: f64) -> FpDecode {
    const ROUND: usize = 17;
    let mut out = FpDecode {
        negative: false,
        special: 0,
        digits: Vec::new(),
        dp: 0,
    };
    if r < 0.0 {
        out.negative = true;
        r = -r;
    } else if r == 0.0 {
        out.digits = vec![b'0'];
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
    let (v10, exp) = fp2_convert10(v, e, 18);

    // z[0] is a spare slot for a carry out of the first digit.
    let mut z: Vec<u8> = Vec::with_capacity(24);
    z.push(b'0');
    z.extend_from_slice(v10.to_string().as_bytes());
    let mut start = 1usize;
    let mut n = z.len() - 1;
    out.dp = n as i32 + exp;
    let mut i_round = ROUND;
    if i_round < n {
        let d = |k: usize| z[start + k];
        if d(15) == b'9' && d(14) == b'9' {
            let mut jj = 14usize;
            while jj > 0 && d(jj - 1) == b'9' {
                jj -= 1;
            }
            let v2 = if jj == 0 {
                1u64
            } else {
                let mut x = u64::from(d(0) - b'0');
                for kk in 1..jj {
                    x = x * 10 + u64::from(d(kk) - b'0');
                }
                x + 1
            };
            if r == fp10_convert2(v2, exp + n as i32 - jj as i32) {
                i_round = jj + 1;
            }
        } else if out.dp >= n as i32 || (d(15) == b'0' && d(14) == b'0' && d(13) == b'0') {
            let mut jj = 13usize;
            while d(jj - 1) == b'0' {
                jj -= 1;
            }
            let mut v2 = u64::from(d(0) - b'0');
            for kk in 1..jj {
                v2 = v2 * 10 + u64::from(d(kk) - b'0');
            }
            if r == fp10_convert2(v2, exp + n as i32 - jj as i32) {
                i_round = jj + 1;
            }
        }
        n = i_round;
        if z[start + i_round] >= b'5' {
            let mut j = i_round - 1;
            loop {
                z[start + j] += 1;
                if z[start + j] <= b'9' {
                    break;
                }
                z[start + j] = b'0';
                if j == 0 {
                    start -= 1;
                    z[start] = b'1';
                    n += 1;
                    out.dp += 1;
                    break;
                }
                j -= 1;
            }
        }
    }
    while n > 1 && z[start + n - 1] == b'0' {
        n -= 1;
    }
    out.digits = z[start..start + n].to_vec();
    out
}

/// The text C SQLite produces for a REAL value (`"%!.17g"`): `1.0`, `0.1`, `1.0e+100`,
/// `1000000000000000.0`, `-2.5e-07`, `Inf`.
pub(crate) fn real_to_text(r: f64) -> String {
    let s = fp_decode17(r);
    if s.special == 2 {
        return "NaN".to_owned();
    }
    if s.special == 1 {
        return if s.negative { "-Inf" } else { "Inf" }.to_owned();
    }
    let mut out = String::with_capacity(26);
    if s.negative {
        out.push('-');
    }
    let exp = s.dp - 1;
    // etGENERIC with precision 17 and the '!' flag.
    let mut precision: i32 = 16;
    let is_exp = exp < -4 || exp > precision;
    if !is_exp {
        precision -= exp;
    }
    let mut e2 = if is_exp { 0 } else { s.dp - 1 };
    let n = s.digits.len() as i32;
    let mut j: i32 = 0;
    if e2 < 0 {
        out.push('0');
    } else {
        j = (e2 + 1).min(n);
        out.push_str(std::str::from_utf8(&s.digits[..j as usize]).unwrap_or("0"));
        e2 -= j;
        if e2 >= 0 {
            for _ in 0..=e2 {
                out.push('0');
            }
            e2 = -1;
        }
    }
    out.push('.');
    if e2 < -1 && precision > 0 {
        let nn = (-1 - e2).min(precision);
        for _ in 0..nn {
            out.push('0');
        }
        precision -= nn;
    }
    if precision > 0 {
        let nn = (n - j).min(precision);
        if nn > 0 {
            out.push_str(
                std::str::from_utf8(&s.digits[j as usize..(j + nn) as usize]).unwrap_or(""),
            );
        }
    }
    while out.ends_with('0') {
        out.pop();
    }
    if out.ends_with('.') {
        out.push('0');
    }
    if is_exp {
        let mut ex = s.dp - 1;
        out.push('e');
        if ex < 0 {
            out.push('-');
            ex = -ex;
        } else {
            out.push('+');
        }
        if ex >= 100 {
            out.push((b'0' + (ex / 100) as u8) as char);
            ex %= 100;
        }
        out.push((b'0' + (ex / 10) as u8) as char);
        out.push((b'0' + (ex % 10) as u8) as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::real_to_text;

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
            (f64::INFINITY, "Inf"),
            (f64::NEG_INFINITY, "-Inf"),
        ] {
            assert_eq!(real_to_text(r), want, "{r:e}");
        }
    }
}
