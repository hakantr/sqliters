//! SQLite shell collations: ext/misc/uint.c and decimal.c (3.54.1).
use std::cmp::Ordering;

/// uint.c:41–85: digit runs have arbitrary precision; leading zeros do not count.
pub(crate) fn uint(left: &str, right: &str) -> Ordering {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let x = a[i].cmp(&b[j]);
        if a[i].is_ascii_digit() {
            if !b[j].is_ascii_digit() {
                return x;
            }
            while i < a.len() && a[i] == b'0' {
                i += 1;
            }
            while j < b.len() && b[j] == b'0' {
                j += 1;
            }
            let mut k = 0;
            while i + k < a.len()
                && a[i + k].is_ascii_digit()
                && j + k < b.len()
                && b[j + k].is_ascii_digit()
            {
                k += 1;
            }
            if i + k < a.len() && a[i + k].is_ascii_digit() {
                return Ordering::Greater;
            }
            if j + k < b.len() && b[j + k].is_ascii_digit() {
                return Ordering::Less;
            }
            let x = a[i..i + k].cmp(&b[j..j + k]);
            if x != Ordering::Equal {
                return x;
            }
            i += k;
            j += k;
        } else if x != Ordering::Equal {
            return x;
        } else {
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

struct Decimal {
    negative: bool,
    digits: Vec<u8>,
    frac: i32,
}

impl Decimal {
    /// decimal.c:74–185. Non-digits, repeated dots and exponent tails follow C.
    fn parse(text: &[u8]) -> Option<Self> {
        let n = i32::try_from(text.len()).ok()?;
        let mut digits = Vec::new();
        digits.try_reserve_exact(n as usize + 1).ok()?;
        let (mut i, mut frac, mut exp) = (0usize, 0i32, 0i32);
        while i < text.len() && matches!(text[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
            i += 1;
        }
        let mut negative = false;
        if i < text.len() && text[i] == b'-' {
            negative = true;
            i += 1;
        } else if i < text.len() && text[i] == b'+' {
            i += 1;
        }
        while i < text.len() && text[i] == b'0' {
            i += 1;
        }
        while i < text.len() {
            let c = text[i];
            if c.is_ascii_digit() {
                digits.push(c - b'0');
            } else if c == b'.' {
                frac = digits.len() as i32 + 1;
            } else if c == b'e' || c == b'E' {
                let mut j = i + 1;
                if j >= text.len() {
                    break;
                }
                let mut neg = false;
                if text[j] == b'-' {
                    neg = true;
                    j += 1;
                } else if text[j] == b'+' {
                    j += 1;
                }
                while j < text.len() && exp < 1_000_000 {
                    if text[j].is_ascii_digit() {
                        exp = exp * 10 + i32::from(text[j] - b'0');
                    }
                    j += 1;
                }
                if neg {
                    exp = -exp;
                }
                break;
            }
            i += 1;
        }
        if frac != 0 {
            frac = digits.len() as i32 - (frac - 1);
        }
        if exp > 0 {
            if frac > 0 {
                if exp <= frac {
                    frac -= exp;
                    exp = 0;
                } else {
                    exp -= frac;
                    frac = 0;
                }
            }
            if exp > 0 {
                digits.try_reserve_exact(exp as usize + 1).ok()?;
                digits.resize(digits.len() + exp as usize, 0);
            }
        } else if exp < 0 {
            exp = -exp;
            let extra = digits.len() as i32 - frac - 1;
            if extra != 0 {
                if extra >= exp {
                    frac += exp;
                    exp = 0;
                } else {
                    exp -= extra;
                    frac = digits.len() as i32 - 1;
                }
            }
            if exp > 0 {
                let old = digits.len();
                digits.try_reserve_exact(exp as usize + 1).ok()?;
                digits.resize(old + exp as usize, 0);
                digits.copy_within(0..old, exp as usize);
                digits[..exp as usize].fill(0);
                frac += exp;
            }
        }
        if negative && digits.iter().all(|&d| d == 0) {
            negative = false;
        }
        if digits.len() > 10_000_000 {
            return None;
        }
        Some(Self {
            negative,
            digits,
            frac,
        })
    }

    fn trim(&mut self) {
        while self.frac > 0 && self.digits.last() == Some(&0) {
            self.digits.pop();
            self.frac -= 1;
        }
    }
}

/// decimal.c:413–443,755–774: compare parsed digits; a parse/allocation failure ties.
pub(crate) fn decimal(left: &str, right: &str) -> Ordering {
    let (Some(mut a), Some(mut b)) = (
        Decimal::parse(left.as_bytes()),
        Decimal::parse(right.as_bytes()),
    ) else {
        return Ordering::Equal;
    };
    a.trim();
    b.trim();
    if a.negative != b.negative {
        return if a.negative {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    if a.negative {
        std::mem::swap(&mut a, &mut b);
    }
    let x = (a.digits.len() as i32 - a.frac).cmp(&(b.digits.len() as i32 - b.frac));
    if x != Ordering::Equal {
        return x;
    }
    let n = a.digits.len().min(b.digits.len());
    a.digits[..n]
        .cmp(&b.digits[..n])
        .then_with(|| a.digits.len().cmp(&b.digits.len()))
}
