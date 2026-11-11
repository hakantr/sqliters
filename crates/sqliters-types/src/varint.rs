//! SQLite variable-length integer (varint) encoding and decoding,
//! ported from `sqlite3PutVarint` and `sqlite3GetVarint` in `sqlite/src/util.c`.
//!
//! Format:
//! - Bytes 1..=8 carry 7 data bits (low bits) and 1 continuation flag (MSB `0x80`).
//! - Byte 9 (when reached) uses all 8 bits as data with no continuation flag.
//! - Big-endian (most significant bits appear first).

use crate::error::{Result, SqliteError};

/// Returns the encoded byte length (1..=9) of `v` as an SQLite varint.
#[inline(always)]
pub const fn varint_len(v: u64) -> usize {
    if v <= 0x7f {
        1
    } else if v <= 0x3fff {
        2
    } else if v <= 0x1f_ffff {
        3
    } else if v <= 0x0fff_ffff {
        4
    } else if v <= 0x07_ffff_ffff {
        5
    } else if v <= 0x03ff_ffff_ffff {
        6
    } else if v <= 0x01_ffff_ffff_ffff {
        7
    } else if v <= 0x00ff_ffff_ffff_ffff {
        8
    } else {
        9
    }
}

/// Writes `v` as a 1..=9 byte SQLite varint into `out[0..]`, returning the number of bytes written.
///
/// Panics if `out.len() < varint_len(v)`.
#[inline(always)]
pub fn put_varint(out: &mut [u8], v: u64) -> usize {
    if v <= 0x7f {
        out[0] = v as u8;
        return 1;
    }
    if v <= 0x3fff {
        out[0] = (((v >> 7) as u8) & 0x7f) | 0x80;
        out[1] = (v as u8) & 0x7f;
        return 2;
    }
    put_varint_slow(out, v)
}

#[inline(never)]
fn put_varint_slow(out: &mut [u8], mut v: u64) -> usize {
    if (v & 0xff00_0000_0000_0000) != 0 {
        out[8] = (v & 0xff) as u8;
        v >>= 8;
        for i in (0..8).rev() {
            out[i] = ((v & 0x7f) | 0x80) as u8;
            v >>= 7;
        }
        return 9;
    }
    let mut buf = [0u8; 9];
    let mut n = 0usize;
    while {
        buf[n] = ((v & 0x7f) | 0x80) as u8;
        n += 1;
        v >>= 7;
        v != 0
    } {}
    buf[0] &= 0x7f;
    for i in 0..n {
        out[i] = buf[n - 1 - i];
    }
    n
}

/// Appends `v` as an SQLite varint onto a `Vec<u8>`, returning the number of bytes written.
#[inline(always)]
pub fn push_varint(out: &mut Vec<u8>, v: u64) -> usize {
    if v <= 0x7f {
        out.push(v as u8);
        return 1;
    }
    if v <= 0x3fff {
        out.extend_from_slice(&[(((v >> 7) as u8) & 0x7f) | 0x80, (v as u8) & 0x7f]);
        return 2;
    }
    let mut tmp = [0u8; 9];
    let n = put_varint_slow(&mut tmp, v);
    out.extend_from_slice(&tmp[..n]);
    n
}

/// Reads an SQLite varint from the beginning of `input`, returning `(value, bytes_read)`.
///
/// Returns `ErrorCode::Corrupt` if `input` ends mid-varint.
#[inline(always)]
pub fn get_varint(input: &[u8]) -> Result<(u64, usize)> {
    let Some(&b0) = input.first() else {
        return Err(SqliteError::corrupt("truncated varint (empty slice)"));
    };
    if b0 < 0x80 {
        return Ok((u64::from(b0), 1));
    }
    let Some(&b1) = input.get(1) else {
        return Err(SqliteError::corrupt("truncated varint"));
    };
    let val = (u64::from(b0 & 0x7f) << 7) | u64::from(b1 & 0x7f);
    if b1 < 0x80 {
        return Ok((val, 2));
    }
    get_varint_slow(input, val)
}

#[inline(never)]
fn get_varint_slow(input: &[u8], mut val: u64) -> Result<(u64, usize)> {
    for idx in 2..8 {
        let Some(&b) = input.get(idx) else {
            return Err(SqliteError::corrupt("truncated varint"));
        };
        val = (val << 7) | u64::from(b & 0x7f);
        if b < 0x80 {
            return Ok((val, idx + 1));
        }
    }
    let Some(&b8) = input.get(8) else {
        return Err(SqliteError::corrupt("truncated 9-byte varint"));
    };
    val = (val << 8) | u64::from(b8);
    Ok((val, 9))
}

/// Encodes a signed 64-bit integer (such as an SQLite `rowid`) as a varint.
#[inline(always)]
pub fn put_varint_i64(out: &mut [u8], v: i64) -> usize {
    put_varint(out, v as u64)
}

/// Reads a signed 64-bit integer (such as an SQLite `rowid`) from a varint.
#[inline(always)]
pub fn get_varint_i64(input: &[u8]) -> Result<(i64, usize)> {
    let (u, n) = get_varint(input)?;
    Ok((u as i64, n))
}
