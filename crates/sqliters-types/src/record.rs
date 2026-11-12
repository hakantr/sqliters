//! On-disk Record codecs (`P3v1` direct-offset little-endian records + Format-3 fallback),
//! ported from `OP_MakeRecord`, `sqlite3VdbeSerialTypeLen`, and `sqlite3VdbeSerialGet`
//! in `sqlite/src/vdbe.c` and `sqlite/src/vdbeaux.c`.
//!
//! `P3v1` compact record binary layout (`col_count < 128`, `body_len < 4096`):
//! ```text
//! [ 0x00 : magic ] [ col_count : u8 ] [ desc_0 : u16_le ] ... [ desc_{N-1} : u16_le ]
//! [ body_0 ] ... [ body_{N-1} ]
//! ```
//! where `desc_k = ((body_end_k as u16) << 4) | (tag_k as u16)`.
//!
//! `P3v1` wide record binary layout (`col_count >= 128` or `body_len >= 4096`):
//! ```text
//! [ 0x00 : magic ] [ 0x80 : wide ] [ col_count : u16_le ]
//! [ tag_0 .. tag_{N-1} : u8 ] [ body_end_0 .. body_end_{N-1} : u32_le ]
//! [ body_0 ] ... [ body_{N-1} ]
//! ```
//!
//! Format-3 fallback binary layout (`record[0] != 0x00`):
//! ```text
//! [ header_size : varint ] [ serial_type_0 : varint ] ... [ serial_type_{N-1} : varint ]
//! [ body_0 ] ... [ body_{N-1} ]
//! ```

use std::cmp::Ordering;

use crate::error::{Result, SqliteError};
use crate::runtime_api::RuntimeValue;
#[cfg(not(target_arch = "x86_64"))]
use crate::value::swar_fold_ascii_lower_u64;
use crate::value::{Collation, CustomCollation, InlineVal, TextEncoding, Value, cmp_int_float};
use crate::varint::{get_varint, push_varint, varint_len};

const MAX_6BYTE_INT: u64 = (0x0000_8000_u64 << 32) - 1; // 140_737_488_355_327

/// Leading magic byte identifying a `P3v1` direct-offset little-endian record.
/// Valid Format-3 records always begin with `header_size >= 1`, so `0x00` is disjoint.
pub const P3V1_RECORD_MAGIC: u8 = 0x00;

/// Flag in `record[1]` marking a wide `P3v1` header (`col_count >= 128` or `body_len >= 4096`).
pub const P3V1_WIDE_FLAG: u8 = 0x80;

pub const P3V1_TAG_NULL: u8 = 0;
pub const P3V1_TAG_ZERO: u8 = 1;
pub const P3V1_TAG_ONE: u8 = 2;
pub const P3V1_TAG_I8: u8 = 3;
pub const P3V1_TAG_I16_LE: u8 = 4;
pub const P3V1_TAG_I32_LE: u8 = 5;
pub const P3V1_TAG_I64_LE: u8 = 6;
pub const P3V1_TAG_F64_LE: u8 = 7;
pub const P3V1_TAG_EMPTY_BLOB: u8 = 8;
pub const P3V1_TAG_EMPTY_TEXT: u8 = 9;
pub const P3V1_TAG_BLOB: u8 = 10;
pub const P3V1_TAG_TEXT: u8 = 11;

/// Packed 64-bit nibble LUT mapping fixed-width `P3v1` tags `0..=9` to byte widths
/// (`0, 0, 0, 1, 2, 4, 8, 8, 0, 0`).
const P3V1_FIXED_WIDTH_NIBBLES: u64 = 0x0000_0000_8842_1000;

#[inline(always)]
pub const fn is_p3v1_record(rec: &[u8]) -> bool {
    !rec.is_empty() && rec[0] == P3V1_RECORD_MAGIC
}

#[inline(always)]
pub const fn p3v1_fixed_tag_width(tag: u8) -> usize {
    ((P3V1_FIXED_WIDTH_NIBBLES >> ((tag as u32) << 2)) & 0xf) as usize
}

#[cold]
#[inline(never)]
fn err_corrupt_p3v1_record() -> SqliteError {
    SqliteError::corrupt("corrupt P3v1 record")
}

/// Parses the leading header metadata `(col_count, hdr_len, is_wide)` of a `P3v1` record prefix.
#[inline(always)]
pub fn p3v1_header_info(rec_prefix: &[u8]) -> Result<(usize, usize, bool)> {
    if rec_prefix.len() < 2 || rec_prefix[0] != P3V1_RECORD_MAGIC {
        return Err(err_corrupt_p3v1_record());
    }
    let b1 = rec_prefix[1];
    if b1 < P3V1_WIDE_FLAG {
        let n = usize::from(b1);
        let hdr_len = 2 + 2 * n;
        if hdr_len > rec_prefix.len() {
            return Err(err_corrupt_p3v1_record());
        }
        Ok((n, hdr_len, false))
    } else if b1 == P3V1_WIDE_FLAG {
        if rec_prefix.len() < 4 {
            return Err(err_corrupt_p3v1_record());
        }
        let n = usize::from(u16::from_le_bytes([rec_prefix[2], rec_prefix[3]]));
        let hdr_len = 4 + 5 * n;
        if hdr_len > rec_prefix.len() {
            return Err(err_corrupt_p3v1_record());
        }
        Ok((n, hdr_len, true))
    } else {
        Err(err_corrupt_p3v1_record())
    }
}

/// Computes `(abs_start, abs_end, tag)` for column `col_idx` in true `O(1)` time from a `P3v1`
/// record header prefix (`rec_prefix.len() >= hdr_len`), without scanning columns `0..col_idx`.
#[inline(always)]
pub fn p3v1_column_offset_and_tag(
    rec_prefix: &[u8],
    col_idx: usize,
) -> Result<Option<(usize, usize, u8)>> {
    if rec_prefix.len() < 2 || rec_prefix[0] != P3V1_RECORD_MAGIC {
        return Err(err_corrupt_p3v1_record());
    }
    let b1 = rec_prefix[1];
    if b1 < P3V1_WIDE_FLAG {
        let n = usize::from(b1);
        let hdr_len = 2 + 2 * n;
        if hdr_len > rec_prefix.len() {
            return Err(err_corrupt_p3v1_record());
        }
        if col_idx >= n {
            return Ok(None);
        }
        let (start_rel, end_rel, tag) = if col_idx == 0 {
            let d = u16::from_le_bytes([rec_prefix[2], rec_prefix[3]]);
            (0usize, usize::from(d >> 4), (d & 0x0f) as u8)
        } else {
            let off = 2 * col_idx;
            let pair = u32::from_le_bytes([
                rec_prefix[off],
                rec_prefix[off + 1],
                rec_prefix[off + 2],
                rec_prefix[off + 3],
            ]);
            (
                ((pair & 0xffff) >> 4) as usize,
                (pair >> 20) as usize,
                ((pair >> 16) & 0x0f) as u8,
            )
        };
        if tag > P3V1_TAG_TEXT || start_rel > end_rel {
            return Err(err_corrupt_p3v1_record());
        }
        if tag < P3V1_TAG_BLOB && (end_rel - start_rel) != p3v1_fixed_tag_width(tag) {
            return Err(err_corrupt_p3v1_record());
        }
        Ok(Some((hdr_len + start_rel, hdr_len + end_rel, tag)))
    } else if b1 == P3V1_WIDE_FLAG {
        if rec_prefix.len() < 4 {
            return Err(err_corrupt_p3v1_record());
        }
        let n = usize::from(u16::from_le_bytes([rec_prefix[2], rec_prefix[3]]));
        let hdr_len = 4 + 5 * n;
        if hdr_len > rec_prefix.len() {
            return Err(err_corrupt_p3v1_record());
        }
        if col_idx >= n {
            return Ok(None);
        }
        let tag = rec_prefix[4 + col_idx];
        let ends_base = 4 + n;
        let end_off = ends_base + 4 * col_idx;
        let end_rel = u32::from_le_bytes([
            rec_prefix[end_off],
            rec_prefix[end_off + 1],
            rec_prefix[end_off + 2],
            rec_prefix[end_off + 3],
        ]) as usize;
        let start_rel = if col_idx == 0 {
            0usize
        } else {
            let prev_off = end_off - 4;
            u32::from_le_bytes([
                rec_prefix[prev_off],
                rec_prefix[prev_off + 1],
                rec_prefix[prev_off + 2],
                rec_prefix[prev_off + 3],
            ]) as usize
        };
        if tag > P3V1_TAG_TEXT || start_rel > end_rel {
            return Err(err_corrupt_p3v1_record());
        }
        if tag < P3V1_TAG_BLOB && (end_rel - start_rel) != p3v1_fixed_tag_width(tag) {
            return Err(err_corrupt_p3v1_record());
        }
        let abs_start = hdr_len
            .checked_add(start_rel)
            .ok_or_else(err_corrupt_p3v1_record)?;
        let abs_end = hdr_len
            .checked_add(end_rel)
            .ok_or_else(err_corrupt_p3v1_record)?;
        Ok(Some((abs_start, abs_end, tag)))
    } else {
        Err(err_corrupt_p3v1_record())
    }
}

/// Fast `O(1)` column span extractor on an already-validated compact `P3v1` header slice
/// (`hdr == &payload[..hdr_len]` with `hdr_len == 2 + 2 * n` and `col_idx < n`).
#[inline(always)]
pub fn p3v1_compact_col_span(
    hdr: &[u8],
    hdr_len: usize,
    col_idx: usize,
) -> Result<(usize, usize, u8)> {
    let (start_rel, end_rel, tag) = if col_idx == 0 {
        let d = u16::from_le_bytes([hdr[2], hdr[3]]);
        (0usize, usize::from(d >> 4), (d & 0x0f) as u8)
    } else {
        let off = 2 * col_idx;
        let pair = u32::from_le_bytes([hdr[off], hdr[off + 1], hdr[off + 2], hdr[off + 3]]);
        (
            ((pair & 0xffff) >> 4) as usize,
            (pair >> 20) as usize,
            ((pair >> 16) & 0x0f) as u8,
        )
    };
    if tag > P3V1_TAG_TEXT || start_rel > end_rel {
        return Err(err_corrupt_p3v1_record());
    }
    if tag < P3V1_TAG_BLOB && (end_rel - start_rel) != p3v1_fixed_tag_width(tag) {
        return Err(err_corrupt_p3v1_record());
    }
    Ok((hdr_len + start_rel, hdr_len + end_rel, tag))
}

/// Decodes an integer `P3v1` column payload (`ZERO..=I64_LE`) in native little-endian.
#[inline(always)]
pub fn decode_p3v1_int_tag(tag: u8, bytes: &[u8]) -> Option<i64> {
    match tag {
        P3V1_TAG_ZERO => Some(0),
        P3V1_TAG_ONE => Some(1),
        P3V1_TAG_I8 => Some(i64::from(bytes[0] as i8)),
        P3V1_TAG_I16_LE => Some(i64::from(i16::from_le_bytes([bytes[0], bytes[1]]))),
        P3V1_TAG_I32_LE => Some(i64::from(i32::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]))),
        P3V1_TAG_I64_LE => Some(i64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        _ => None,
    }
}

/// Packed 64-bit SWAR nibble LUT mapping serial types `0..=11` to their byte lengths
/// (`0, 1, 2, 3, 4, 6, 8, 8, 0, 0`, with `0xf` marking reserved corrupt serial types `10 | 11`).
const ST_SMALL_NIBBLES: u64 = 0x0000_ff00_8864_3210;

/// Computes the payload byte length of a 1-byte serial type (`< 128`) in pure register SWAR
/// without L1 table loads or jump tables; returns `0xff` for reserved corrupt types `10 | 11`.
#[inline(always)]
pub const fn swar_serial_len_1b(st: u8) -> u8 {
    if st >= 12 {
        (st - 12) >> 1
    } else if (st & !1) == 10 {
        0xff
    } else {
        ((ST_SMALL_NIBBLES >> ((st as u32) << 2)) & 0xf) as u8
    }
}

/// SWAR 32/64-bit parallel MSB continuation-bit header decoder for Format-3 records.
///
/// When `rec` starts with a single-byte `hdr_size` (`2 <= hdr_size <= rec.len()`) and all bytes
/// in the leading 32-bit or 64-bit header word have bit 7 clear (`0x8080... == 0`), returns
/// `Some((hdr_size, packed_serial_types_le, swar_col_count))` in a single word test.
#[inline(always)]
pub fn swar_decode_hdr(rec: &[u8]) -> Option<(usize, u64, usize)> {
    if rec.len() >= 8 {
        let w = u64::from_le_bytes([
            rec[0], rec[1], rec[2], rec[3], rec[4], rec[5], rec[6], rec[7],
        ]);
        let hdr_size = (w & 0xff) as usize;
        if hdr_size >= 2 && hdr_size <= rec.len() {
            let check = hdr_size.min(8);
            let msb_mask = 0x8080_8080_8080_8080u64 >> ((8 - check) * 8);
            if (w & msb_mask) == 0 {
                return Some((hdr_size, w >> 8, check - 1));
            }
        }
    } else if rec.len() >= 4 {
        let w32 = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
        let hdr_size = (w32 & 0xff) as usize;
        if (2..=4).contains(&hdr_size) && hdr_size <= rec.len() {
            let msb_mask = 0x8080_8080u32 >> ((4 - hdr_size) * 8);
            if (w32 & msb_mask) == 0 {
                return Some((hdr_size, u64::from(w32 >> 8), hdr_size - 1));
            }
        }
    }
    None
}

/// SWAR pure-ASCII big-endian word comparator (`0x8080_8080_8080_8080` MSB mask + `u64`/`u32`
/// big-endian word compare) for `Collation::Binary` under UTF-8. Returns `None` if any inspected
/// byte is non-ASCII (`>= 0x80`) so the caller can fall back to full UTF-8 decoding.
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn swar_cmp_ascii_binary(a: &[u8], b: &[u8]) -> Option<Ordering> {
    let min_len = a.len().min(b.len());
    if min_len >= 8 {
        let mut pos = 0usize;
        while pos + 8 <= min_len {
            let sa = &a[pos..pos + 8];
            let sb = &b[pos..pos + 8];
            let wa = u64::from_be_bytes([sa[0], sa[1], sa[2], sa[3], sa[4], sa[5], sa[6], sa[7]]);
            let wb = u64::from_be_bytes([sb[0], sb[1], sb[2], sb[3], sb[4], sb[5], sb[6], sb[7]]);
            if ((wa | wb) & 0x8080_8080_8080_8080) != 0 {
                return None;
            }
            if wa != wb {
                return Some(wa.cmp(&wb));
            }
            pos += 8;
        }
        if pos < min_len {
            let t = min_len - 8;
            let sa = &a[t..min_len];
            let sb = &b[t..min_len];
            let wa = u64::from_be_bytes([sa[0], sa[1], sa[2], sa[3], sa[4], sa[5], sa[6], sa[7]]);
            let wb = u64::from_be_bytes([sb[0], sb[1], sb[2], sb[3], sb[4], sb[5], sb[6], sb[7]]);
            if ((wa | wb) & 0x8080_8080_8080_8080) != 0 {
                return None;
            }
            if wa != wb {
                return Some(wa.cmp(&wb));
            }
        }
        return Some(a.len().cmp(&b.len()));
    }
    if min_len >= 4 {
        let t = min_len - 4;
        let wa0 = u32::from_be_bytes([a[0], a[1], a[2], a[3]]);
        let wb0 = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        let wa1 = u32::from_be_bytes([a[t], a[t + 1], a[t + 2], a[t + 3]]);
        let wb1 = u32::from_be_bytes([b[t], b[t + 1], b[t + 2], b[t + 3]]);
        let pa = (u64::from(wa0) << 32) | u64::from(wa1);
        let pb = (u64::from(wb0) << 32) | u64::from(wb1);
        if ((pa | pb) & 0x8080_8080_8080_8080) != 0 {
            return None;
        }
        if pa != pb {
            return Some(pa.cmp(&pb));
        }
        return Some(a.len().cmp(&b.len()));
    }
    let mut i = 0usize;
    while i < min_len {
        let ba = a[i];
        let bb = b[i];
        if ((ba | bb) & 0x80) != 0 {
            return None;
        }
        if ba != bb {
            return Some(ba.cmp(&bb));
        }
        i += 1;
    }
    Some(a.len().cmp(&b.len()))
}

/// SWAR pure-ASCII big-endian word comparator for `Collation::NoCase` under UTF-8. Returns `None`
/// if any inspected byte is non-ASCII (`>= 0x80`) so the caller can fall back to UTF-8 decoding.
#[cfg(not(target_arch = "x86_64"))]
#[inline(always)]
fn swar_cmp_ascii_nocase(a: &[u8], b: &[u8]) -> Option<Ordering> {
    let min_len = a.len().min(b.len());
    if min_len >= 8 {
        let mut pos = 0usize;
        while pos + 8 <= min_len {
            let sa = &a[pos..pos + 8];
            let sb = &b[pos..pos + 8];
            let wa = u64::from_be_bytes([sa[0], sa[1], sa[2], sa[3], sa[4], sa[5], sa[6], sa[7]]);
            let wb = u64::from_be_bytes([sb[0], sb[1], sb[2], sb[3], sb[4], sb[5], sb[6], sb[7]]);
            if ((wa | wb) & 0x8080_8080_8080_8080) != 0 {
                return None;
            }
            if wa != wb {
                let fa = swar_fold_ascii_lower_u64(wa);
                let fb = swar_fold_ascii_lower_u64(wb);
                if fa != fb {
                    return Some(fa.cmp(&fb));
                }
            }
            pos += 8;
        }
        if pos < min_len {
            let t = min_len - 8;
            let sa = &a[t..min_len];
            let sb = &b[t..min_len];
            let wa = u64::from_be_bytes([sa[0], sa[1], sa[2], sa[3], sa[4], sa[5], sa[6], sa[7]]);
            let wb = u64::from_be_bytes([sb[0], sb[1], sb[2], sb[3], sb[4], sb[5], sb[6], sb[7]]);
            if ((wa | wb) & 0x8080_8080_8080_8080) != 0 {
                return None;
            }
            if wa != wb {
                let fa = swar_fold_ascii_lower_u64(wa);
                let fb = swar_fold_ascii_lower_u64(wb);
                if fa != fb {
                    return Some(fa.cmp(&fb));
                }
            }
        }
        return Some(a.len().cmp(&b.len()));
    }
    if min_len >= 4 {
        let t = min_len - 4;
        let wa0 = u32::from_be_bytes([a[0], a[1], a[2], a[3]]);
        let wb0 = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        let wa1 = u32::from_be_bytes([a[t], a[t + 1], a[t + 2], a[t + 3]]);
        let wb1 = u32::from_be_bytes([b[t], b[t + 1], b[t + 2], b[t + 3]]);
        let pa = (u64::from(wa0) << 32) | u64::from(wa1);
        let pb = (u64::from(wb0) << 32) | u64::from(wb1);
        if ((pa | pb) & 0x8080_8080_8080_8080) != 0 {
            return None;
        }
        if pa != pb {
            let fa = swar_fold_ascii_lower_u64(pa);
            let fb = swar_fold_ascii_lower_u64(pb);
            if fa != fb {
                return Some(fa.cmp(&fb));
            }
        }
        return Some(a.len().cmp(&b.len()));
    }
    let mut i = 0usize;
    while i < min_len {
        let ba = a[i];
        let bb = b[i];
        if ((ba | bb) & 0x80) != 0 {
            return None;
        }
        let la = ba.to_ascii_lowercase();
        let lb = bb.to_ascii_lowercase();
        if la != lb {
            return Some(la.cmp(&lb));
        }
        i += 1;
    }
    Some(a.len().cmp(&b.len()))
}

/// Computes `(serial_type, data_byte_len)` for `value` under UTF-8 SQLite file format 4 (`vdbeaux.c:3919`).
#[inline]
pub fn serial_type_for(value: &Value) -> (u64, usize) {
    serial_type_for_with_encoding(value, TextEncoding::Utf8)
}

/// Computes `(serial_type, data_byte_len)` for `value` under `enc` (`UTF-8`, `UTF-16le`, or `UTF-16be`).
#[inline]
pub fn serial_type_for_with_encoding(value: &Value, enc: TextEncoding) -> (u64, usize) {
    match value {
        Value::Null => (0, 0),
        Value::Integer(i) => {
            let i = *i;
            if i == 0 {
                return (8, 0);
            }
            if i == 1 {
                return (9, 0);
            }
            let u = if i < 0 { (!i) as u64 } else { i as u64 };
            if u <= 127 {
                (1, 1)
            } else if u <= 32_767 {
                (2, 2)
            } else if u <= 8_388_607 {
                (3, 3)
            } else if u <= 2_147_483_647 {
                (4, 4)
            } else if u <= MAX_6BYTE_INT {
                (5, 6)
            } else {
                (6, 8)
            }
        }
        Value::Real(r) => {
            if r.is_nan() {
                (0, 0)
            } else {
                (7, 8)
            }
        }
        Value::Blob(b) => {
            let n = b.len();
            ((n as u64) * 2 + 12, n)
        }
        Value::Text(s) => {
            let n = enc.encoded_byte_len(s);
            ((n as u64) * 2 + 13, n)
        }
    }
}

/// Returns the payload byte length corresponding to `serial_type` (`sqlite3VdbeSerialTypeLen`).
#[cold]
#[inline(never)]
fn err_reserved_serial(serial_type: u64) -> SqliteError {
    SqliteError::corrupt(format!(
        "reserved serial type {serial_type} encountered in record"
    ))
}

#[inline]
pub fn serial_type_len(serial_type: u64) -> Result<usize> {
    if serial_type >= 12 {
        return Ok(((serial_type - 12) >> 1) as usize);
    }
    let nib = (ST_SMALL_NIBBLES >> ((serial_type as u32) << 2)) & 0xf;
    if nib != 0xf {
        Ok(nib as usize)
    } else {
        Err(err_reserved_serial(serial_type))
    }
}

#[inline(always)]
fn p3v1_tag_for_value(value: &Value, enc: TextEncoding) -> (u8, usize) {
    match value {
        Value::Null => (P3V1_TAG_NULL, 0),
        Value::Integer(i) => {
            let x = *i;
            if x == 0 {
                (P3V1_TAG_ZERO, 0)
            } else if x == 1 {
                (P3V1_TAG_ONE, 0)
            } else if i64::from(x as i8) == x {
                (P3V1_TAG_I8, 1)
            } else if i64::from(x as i16) == x {
                (P3V1_TAG_I16_LE, 2)
            } else if i64::from(x as i32) == x {
                (P3V1_TAG_I32_LE, 4)
            } else {
                (P3V1_TAG_I64_LE, 8)
            }
        }
        Value::Real(r) => {
            if r.is_nan() {
                (P3V1_TAG_NULL, 0)
            } else {
                (P3V1_TAG_F64_LE, 8)
            }
        }
        Value::Blob(b) => {
            if b.is_empty() {
                (P3V1_TAG_EMPTY_BLOB, 0)
            } else {
                (P3V1_TAG_BLOB, b.len())
            }
        }
        Value::Text(s) => {
            let n = enc.encoded_byte_len(s);
            if n == 0 {
                (P3V1_TAG_EMPTY_TEXT, 0)
            } else {
                (P3V1_TAG_TEXT, n)
            }
        }
    }
}

#[inline(always)]
fn write_p3v1_value(out: &mut Vec<u8>, value: &Value, tag: u8, enc: TextEncoding) {
    match (value, tag) {
        (Value::Null, _)
        | (
            _,
            P3V1_TAG_NULL | P3V1_TAG_ZERO | P3V1_TAG_ONE | P3V1_TAG_EMPTY_BLOB
            | P3V1_TAG_EMPTY_TEXT,
        ) => {}
        (Value::Integer(i), P3V1_TAG_I8) => out.push(*i as u8),
        (Value::Integer(i), P3V1_TAG_I16_LE) => out.extend_from_slice(&(*i as i16).to_le_bytes()),
        (Value::Integer(i), P3V1_TAG_I32_LE) => out.extend_from_slice(&(*i as i32).to_le_bytes()),
        (Value::Integer(i), P3V1_TAG_I64_LE) => out.extend_from_slice(&i.to_le_bytes()),
        (Value::Real(r), P3V1_TAG_F64_LE) => out.extend_from_slice(&r.to_bits().to_le_bytes()),
        (Value::Blob(b), P3V1_TAG_BLOB) => out.extend_from_slice(b),
        (Value::Text(s), P3V1_TAG_TEXT) => enc.encode_into(s, out),
        _ => unreachable!("p3v1 tag matched value in encode_record"),
    }
}

#[inline(never)]
fn encode_p3v1_wide_values_into(values: &[Value], enc: TextEncoding, out: &mut Vec<u8>) {
    if values.len() > usize::from(u16::MAX) {
        encode_record_format3_into(values, enc, out);
        return;
    }
    let n = values.len();
    let hdr_len = 4 + 5 * n;
    let mut tags = Vec::with_capacity(n);
    let mut ends = Vec::with_capacity(n);
    let mut body_len = 0usize;
    for v in values {
        let (tag, len) = p3v1_tag_for_value(v, enc);
        body_len += len;
        tags.push(tag);
        ends.push(body_len as u32);
    }
    out.clear();
    out.reserve(hdr_len + body_len);
    out.push(P3V1_RECORD_MAGIC);
    out.push(P3V1_WIDE_FLAG);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&tags);
    for &end in &ends {
        out.extend_from_slice(&end.to_le_bytes());
    }
    for (v, &tag) in values.iter().zip(tags.iter()) {
        write_p3v1_value(out, v, tag, enc);
    }
}

#[inline(never)]
fn encode_p3v1_wide_value_refs_into(values: &[&Value], enc: TextEncoding, out: &mut Vec<u8>) {
    if values.len() > usize::from(u16::MAX) {
        encode_value_refs_record_format3_into(values, enc, out);
        return;
    }
    let n = values.len();
    let hdr_len = 4 + 5 * n;
    let mut tags = Vec::with_capacity(n);
    let mut ends = Vec::with_capacity(n);
    let mut body_len = 0usize;
    for &v in values {
        let (tag, len) = p3v1_tag_for_value(v, enc);
        body_len += len;
        tags.push(tag);
        ends.push(body_len as u32);
    }
    out.clear();
    out.reserve(hdr_len + body_len);
    out.push(P3V1_RECORD_MAGIC);
    out.push(P3V1_WIDE_FLAG);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&tags);
    for &end in &ends {
        out.extend_from_slice(&end.to_le_bytes());
    }
    for (&v, &tag) in values.iter().zip(tags.iter()) {
        write_p3v1_value(out, v, tag, enc);
    }
}

#[inline(never)]
fn encode_p3v1_wide_runtime_into(values: &[RuntimeValue], enc: TextEncoding, out: &mut Vec<u8>) {
    if values.len() > usize::from(u16::MAX) {
        encode_runtime_record_format3_into(values, enc, out);
        return;
    }
    let n = values.len();
    let hdr_len = 4 + 5 * n;
    let mut tags = Vec::with_capacity(n);
    let mut ends = Vec::with_capacity(n);
    let mut body_len = 0usize;
    for rv in values {
        let (tag, len) = p3v1_tag_for_value(&rv.value, enc);
        body_len += len;
        tags.push(tag);
        ends.push(body_len as u32);
    }
    out.clear();
    out.reserve(hdr_len + body_len);
    out.push(P3V1_RECORD_MAGIC);
    out.push(P3V1_WIDE_FLAG);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&tags);
    for &end in &ends {
        out.extend_from_slice(&end.to_le_bytes());
    }
    for (rv, &tag) in values.iter().zip(tags.iter()) {
        write_p3v1_value(out, &rv.value, tag, enc);
    }
}

/// Serializes a slice of [`Value`]s into a `P3v1` direct-offset little-endian record using UTF-8.
pub fn encode_record(values: &[Value]) -> Vec<u8> {
    encode_record_with_encoding(values, TextEncoding::Utf8)
}

/// Serializes a slice of [`Value`]s into a `P3v1` direct-offset little-endian record using `enc`,
/// reusing `out`'s existing allocation.
pub fn encode_record_into(values: &[Value], enc: TextEncoding, out: &mut Vec<u8>) {
    let n = values.len();
    if n <= 16 {
        let mut hdr_buf = [0u8; 34];
        let mut tags = [0u8; 16];
        let mut body_len = 0usize;
        for (i, v) in values.iter().enumerate() {
            let (tag, len) = p3v1_tag_for_value(v, enc);
            body_len += len;
            tags[i] = tag;
            if body_len < 4096 {
                let desc = ((body_len as u16) << 4) | u16::from(tag);
                let db = desc.to_le_bytes();
                hdr_buf[2 + 2 * i] = db[0];
                hdr_buf[2 + 2 * i + 1] = db[1];
            }
        }
        if body_len >= 4096 {
            encode_p3v1_wide_values_into(values, enc, out);
            return;
        }
        hdr_buf[0] = P3V1_RECORD_MAGIC;
        hdr_buf[1] = n as u8;
        let hdr_len = 2 + 2 * n;
        out.clear();
        out.reserve(hdr_len + body_len);
        out.extend_from_slice(&hdr_buf[..hdr_len]);
        for (v, &tag) in values.iter().zip(tags[..n].iter()) {
            write_p3v1_value(out, v, tag, enc);
        }
        return;
    }
    if n < 128 {
        let mut hdr_buf = [0u8; 258];
        let mut tags = [0u8; 128];
        let mut body_len = 0usize;
        for (i, v) in values.iter().enumerate() {
            let (tag, len) = p3v1_tag_for_value(v, enc);
            body_len += len;
            tags[i] = tag;
            if body_len < 4096 {
                let desc = ((body_len as u16) << 4) | u16::from(tag);
                let db = desc.to_le_bytes();
                hdr_buf[2 + 2 * i] = db[0];
                hdr_buf[2 + 2 * i + 1] = db[1];
            }
        }
        if body_len >= 4096 {
            encode_p3v1_wide_values_into(values, enc, out);
            return;
        }
        hdr_buf[0] = P3V1_RECORD_MAGIC;
        hdr_buf[1] = n as u8;
        let hdr_len = 2 + 2 * n;
        out.clear();
        out.reserve(hdr_len + body_len);
        out.extend_from_slice(&hdr_buf[..hdr_len]);
        for (v, &tag) in values.iter().zip(tags[..n].iter()) {
            write_p3v1_value(out, v, tag, enc);
        }
        return;
    }
    encode_p3v1_wide_values_into(values, enc, out);
}

/// Serializes a slice of [`Value`]s into an explicit Format-3 record using UTF-8.
pub fn encode_record_format3(values: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_record_format3_into(values, TextEncoding::Utf8, &mut out);
    out
}

/// Serializes a slice of [`Value`]s into an explicit Format-3 record using `enc`.
pub fn encode_record_format3_into(values: &[Value], enc: TextEncoding, out: &mut Vec<u8>) {
    out.clear();
    if values.len() <= 16 {
        let mut serials = [0u64; 16];
        let mut serial_bytes_len = 0usize;
        let mut body_len = 0usize;
        for (i, v) in values.iter().enumerate() {
            let (st, len) = serial_type_for_with_encoding(v, enc);
            serial_bytes_len += varint_len(st);
            body_len += len;
            serials[i] = st;
        }
        let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
        if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
            hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
        }
        out.reserve(hdr_len + body_len);
        push_varint(out, hdr_len as u64);
        for &st in &serials[..values.len()] {
            push_varint(out, st);
        }
        debug_assert_eq!(out.len(), hdr_len);
        for (v, &st) in values.iter().zip(serials[..values.len()].iter()) {
            write_serial_value(out, v, st, enc);
        }
        return;
    }

    let mut serials = Vec::with_capacity(values.len());
    let mut serial_bytes_len = 0usize;
    let mut body_len = 0usize;

    for v in values {
        let (st, len) = serial_type_for_with_encoding(v, enc);
        serial_bytes_len += varint_len(st);
        body_len += len;
        serials.push(st);
    }

    let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
    if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
        hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
    }

    out.reserve(hdr_len + body_len);
    push_varint(out, hdr_len as u64);
    for st in &serials {
        push_varint(out, *st);
    }
    debug_assert_eq!(out.len(), hdr_len);

    for (v, &st) in values.iter().zip(serials.iter()) {
        write_serial_value(out, v, st, enc);
    }
}

/// Serializes [`Value`]s into `out` as `P3v1` (`is_p3v1 == true`) or Format-3 (`is_p3v1 == false`).
#[inline]
pub fn encode_record_for_format_into(
    values: &[Value],
    enc: TextEncoding,
    is_p3v1: bool,
    out: &mut Vec<u8>,
) {
    if is_p3v1 {
        encode_record_into(values, enc, out);
    } else {
        encode_record_format3_into(values, enc, out);
    }
}

/// Serializes a slice of [`Value`] references into a `P3v1` record using `enc`,
/// reusing `out`'s existing allocation without cloning [`Value`]s.
pub fn encode_value_refs_record_into(values: &[&Value], enc: TextEncoding, out: &mut Vec<u8>) {
    let n = values.len();
    if n <= 16 {
        let mut hdr_buf = [0u8; 34];
        let mut tags = [0u8; 16];
        let mut body_len = 0usize;
        for (i, &v) in values.iter().enumerate() {
            let (tag, len) = p3v1_tag_for_value(v, enc);
            body_len += len;
            tags[i] = tag;
            if body_len < 4096 {
                let desc = ((body_len as u16) << 4) | u16::from(tag);
                let db = desc.to_le_bytes();
                hdr_buf[2 + 2 * i] = db[0];
                hdr_buf[2 + 2 * i + 1] = db[1];
            }
        }
        if body_len >= 4096 {
            encode_p3v1_wide_value_refs_into(values, enc, out);
            return;
        }
        hdr_buf[0] = P3V1_RECORD_MAGIC;
        hdr_buf[1] = n as u8;
        let hdr_len = 2 + 2 * n;
        out.clear();
        out.reserve(hdr_len + body_len);
        out.extend_from_slice(&hdr_buf[..hdr_len]);
        for (&v, &tag) in values.iter().zip(tags[..n].iter()) {
            write_p3v1_value(out, v, tag, enc);
        }
        return;
    }
    if n < 128 {
        let mut hdr_buf = [0u8; 258];
        let mut tags = [0u8; 128];
        let mut body_len = 0usize;
        for (i, &v) in values.iter().enumerate() {
            let (tag, len) = p3v1_tag_for_value(v, enc);
            body_len += len;
            tags[i] = tag;
            if body_len < 4096 {
                let desc = ((body_len as u16) << 4) | u16::from(tag);
                let db = desc.to_le_bytes();
                hdr_buf[2 + 2 * i] = db[0];
                hdr_buf[2 + 2 * i + 1] = db[1];
            }
        }
        if body_len >= 4096 {
            encode_p3v1_wide_value_refs_into(values, enc, out);
            return;
        }
        hdr_buf[0] = P3V1_RECORD_MAGIC;
        hdr_buf[1] = n as u8;
        let hdr_len = 2 + 2 * n;
        out.clear();
        out.reserve(hdr_len + body_len);
        out.extend_from_slice(&hdr_buf[..hdr_len]);
        for (&v, &tag) in values.iter().zip(tags[..n].iter()) {
            write_p3v1_value(out, v, tag, enc);
        }
        return;
    }
    encode_p3v1_wide_value_refs_into(values, enc, out);
}

/// Serializes a slice of [`Value`] references into an explicit Format-3 record using `enc`.
pub fn encode_value_refs_record_format3_into(
    values: &[&Value],
    enc: TextEncoding,
    out: &mut Vec<u8>,
) {
    out.clear();
    if values.len() <= 16 {
        let mut serials = [0u64; 16];
        let mut serial_bytes_len = 0usize;
        let mut body_len = 0usize;
        for (i, &v) in values.iter().enumerate() {
            let (st, len) = serial_type_for_with_encoding(v, enc);
            serial_bytes_len += varint_len(st);
            body_len += len;
            serials[i] = st;
        }
        let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
        if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
            hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
        }
        out.reserve(hdr_len + body_len);
        push_varint(out, hdr_len as u64);
        for &st in &serials[..values.len()] {
            push_varint(out, st);
        }
        debug_assert_eq!(out.len(), hdr_len);
        for (&v, &st) in values.iter().zip(serials[..values.len()].iter()) {
            write_serial_value(out, v, st, enc);
        }
        return;
    }

    let mut serials = Vec::with_capacity(values.len());
    let mut serial_bytes_len = 0usize;
    let mut body_len = 0usize;
    for &v in values {
        let (st, len) = serial_type_for_with_encoding(v, enc);
        serial_bytes_len += varint_len(st);
        body_len += len;
        serials.push(st);
    }
    let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
    if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
        hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
    }
    out.reserve(hdr_len + body_len);
    push_varint(out, hdr_len as u64);
    for st in &serials {
        push_varint(out, *st);
    }
    debug_assert_eq!(out.len(), hdr_len);
    for (&v, &st) in values.iter().zip(serials.iter()) {
        write_serial_value(out, v, st, enc);
    }
}

/// Serializes [`Value`] references into `out` as `P3v1` or Format-3.
#[inline]
pub fn encode_value_refs_record_for_format_into(
    values: &[&Value],
    enc: TextEncoding,
    is_p3v1: bool,
    out: &mut Vec<u8>,
) {
    if is_p3v1 {
        encode_value_refs_record_into(values, enc, out);
    } else {
        encode_value_refs_record_format3_into(values, enc, out);
    }
}

/// Serializes a slice of [`Value`]s into a `P3v1` record using `enc` (`OP_MakeRecord`).
pub fn encode_record_with_encoding(values: &[Value], enc: TextEncoding) -> Vec<u8> {
    let mut out = Vec::new();
    encode_record_into(values, enc, &mut out);
    out
}

/// Serializes a slice of [`RuntimeValue`] registers directly into `out` as a `P3v1` record,
/// reusing `out`'s capacity without cloning intermediate [`Value`] elements (`OP_MakeRecord`).
pub fn encode_runtime_record_into(values: &[RuntimeValue], enc: TextEncoding, out: &mut Vec<u8>) {
    let n = values.len();
    if n <= 16 {
        let mut hdr_buf = [0u8; 34];
        let mut tags = [0u8; 16];
        let mut body_len = 0usize;
        for (i, rv) in values.iter().enumerate() {
            let (tag, len) = p3v1_tag_for_value(&rv.value, enc);
            body_len += len;
            tags[i] = tag;
            if body_len < 4096 {
                let desc = ((body_len as u16) << 4) | u16::from(tag);
                let db = desc.to_le_bytes();
                hdr_buf[2 + 2 * i] = db[0];
                hdr_buf[2 + 2 * i + 1] = db[1];
            }
        }
        if body_len >= 4096 {
            encode_p3v1_wide_runtime_into(values, enc, out);
            return;
        }
        hdr_buf[0] = P3V1_RECORD_MAGIC;
        hdr_buf[1] = n as u8;
        let hdr_len = 2 + 2 * n;
        out.clear();
        out.reserve(hdr_len + body_len);
        out.extend_from_slice(&hdr_buf[..hdr_len]);
        for (rv, &tag) in values.iter().zip(tags[..n].iter()) {
            write_p3v1_value(out, &rv.value, tag, enc);
        }
        return;
    }
    if n < 128 {
        let mut hdr_buf = [0u8; 258];
        let mut tags = [0u8; 128];
        let mut body_len = 0usize;
        for (i, rv) in values.iter().enumerate() {
            let (tag, len) = p3v1_tag_for_value(&rv.value, enc);
            body_len += len;
            tags[i] = tag;
            if body_len < 4096 {
                let desc = ((body_len as u16) << 4) | u16::from(tag);
                let db = desc.to_le_bytes();
                hdr_buf[2 + 2 * i] = db[0];
                hdr_buf[2 + 2 * i + 1] = db[1];
            }
        }
        if body_len >= 4096 {
            encode_p3v1_wide_runtime_into(values, enc, out);
            return;
        }
        hdr_buf[0] = P3V1_RECORD_MAGIC;
        hdr_buf[1] = n as u8;
        let hdr_len = 2 + 2 * n;
        out.clear();
        out.reserve(hdr_len + body_len);
        out.extend_from_slice(&hdr_buf[..hdr_len]);
        for (rv, &tag) in values.iter().zip(tags[..n].iter()) {
            write_p3v1_value(out, &rv.value, tag, enc);
        }
        return;
    }
    encode_p3v1_wide_runtime_into(values, enc, out);
}

/// Serializes [`RuntimeValue`]s into an explicit Format-3 record using `enc`.
pub fn encode_runtime_record_format3_into(
    values: &[RuntimeValue],
    enc: TextEncoding,
    out: &mut Vec<u8>,
) {
    out.clear();
    if values.len() <= 16 {
        let mut serials = [0u64; 16];
        let mut serial_bytes_len = 0usize;
        let mut body_len = 0usize;
        for (i, rv) in values.iter().enumerate() {
            let (st, len) = serial_type_for_with_encoding(&rv.value, enc);
            serial_bytes_len += varint_len(st);
            body_len += len;
            serials[i] = st;
        }
        let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
        if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
            hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
        }
        out.reserve(hdr_len + body_len);
        push_varint(out, hdr_len as u64);
        for &st in &serials[..values.len()] {
            push_varint(out, st);
        }
        debug_assert_eq!(out.len(), hdr_len);
        for (rv, &st) in values.iter().zip(serials[..values.len()].iter()) {
            write_serial_value(out, &rv.value, st, enc);
        }
        return;
    }

    let mut serials = Vec::with_capacity(values.len());
    let mut serial_bytes_len = 0usize;
    let mut body_len = 0usize;
    for rv in values {
        let (st, len) = serial_type_for_with_encoding(&rv.value, enc);
        serial_bytes_len += varint_len(st);
        body_len += len;
        serials.push(st);
    }
    let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
    if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
        hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
    }
    out.reserve(hdr_len + body_len);
    push_varint(out, hdr_len as u64);
    for st in &serials {
        push_varint(out, *st);
    }
    debug_assert_eq!(out.len(), hdr_len);
    for (rv, &st) in values.iter().zip(serials.iter()) {
        write_serial_value(out, &rv.value, st, enc);
    }
}

/// Serializes [`RuntimeValue`]s into `out` as `P3v1` or Format-3.
#[inline]
pub fn encode_runtime_record_for_format_into(
    values: &[RuntimeValue],
    enc: TextEncoding,
    is_p3v1: bool,
    out: &mut Vec<u8>,
) {
    if is_p3v1 {
        encode_runtime_record_into(values, enc, out);
    } else {
        encode_runtime_record_format3_into(values, enc, out);
    }
}

/// Serializes a slice of [`RuntimeValue`] registers directly into a `P3v1` record
/// without cloning intermediate [`Value`] elements (`OP_MakeRecord`).
pub fn encode_runtime_record_with_encoding(values: &[RuntimeValue], enc: TextEncoding) -> Vec<u8> {
    let mut out = Vec::new();
    encode_runtime_record_into(values, enc, &mut out);
    out
}

fn write_serial_value(out: &mut Vec<u8>, value: &Value, serial_type: u64, enc: TextEncoding) {
    match (value, serial_type) {
        (Value::Null, _) | (_, 0 | 8 | 9) => {}
        (Value::Integer(i), 1) => out.push(*i as u8),
        (Value::Integer(i), 2) => out.extend_from_slice(&(*i as i16).to_be_bytes()),
        (Value::Integer(i), 3) => {
            let bytes = (*i as i32).to_be_bytes();
            out.extend_from_slice(&bytes[1..4]);
        }
        (Value::Integer(i), 4) => out.extend_from_slice(&(*i as i32).to_be_bytes()),
        (Value::Integer(i), 5) => {
            let bytes = i.to_be_bytes();
            out.extend_from_slice(&bytes[2..8]);
        }
        (Value::Integer(i), 6) => out.extend_from_slice(&i.to_be_bytes()),
        (Value::Real(r), 7) => out.extend_from_slice(&r.to_bits().to_be_bytes()),
        (Value::Blob(b), _) => out.extend_from_slice(b),
        (Value::Text(s), _) => enc.encode_into(s, out),
        _ => unreachable!("serial_type matched value in encode_record"),
    }
}

#[inline(always)]
fn p3v1_tag_for_raw_field(field: RawField<'_>) -> (u8, usize) {
    match field {
        RawField::Null => (P3V1_TAG_NULL, 0),
        RawField::Int(x) => {
            if x == 0 {
                (P3V1_TAG_ZERO, 0)
            } else if x == 1 {
                (P3V1_TAG_ONE, 0)
            } else if i64::from(x as i8) == x {
                (P3V1_TAG_I8, 1)
            } else if i64::from(x as i16) == x {
                (P3V1_TAG_I16_LE, 2)
            } else if i64::from(x as i32) == x {
                (P3V1_TAG_I32_LE, 4)
            } else {
                (P3V1_TAG_I64_LE, 8)
            }
        }
        RawField::Float(r) => {
            if r.is_nan() {
                (P3V1_TAG_NULL, 0)
            } else {
                (P3V1_TAG_F64_LE, 8)
            }
        }
        RawField::Blob(b) => {
            if b.is_empty() {
                (P3V1_TAG_EMPTY_BLOB, 0)
            } else {
                (P3V1_TAG_BLOB, b.len())
            }
        }
        RawField::Text(b) => {
            if b.is_empty() {
                (P3V1_TAG_EMPTY_TEXT, 0)
            } else {
                (P3V1_TAG_TEXT, b.len())
            }
        }
    }
}

#[inline(always)]
fn write_p3v1_raw_field(out: &mut Vec<u8>, field: RawField<'_>, tag: u8) {
    match (field, tag) {
        (RawField::Null, _)
        | (
            _,
            P3V1_TAG_NULL | P3V1_TAG_ZERO | P3V1_TAG_ONE | P3V1_TAG_EMPTY_BLOB
            | P3V1_TAG_EMPTY_TEXT,
        ) => {}
        (RawField::Int(i), P3V1_TAG_I8) => out.push(i as u8),
        (RawField::Int(i), P3V1_TAG_I16_LE) => out.extend_from_slice(&(i as i16).to_le_bytes()),
        (RawField::Int(i), P3V1_TAG_I32_LE) => out.extend_from_slice(&(i as i32).to_le_bytes()),
        (RawField::Int(i), P3V1_TAG_I64_LE) => out.extend_from_slice(&i.to_le_bytes()),
        (RawField::Float(r), P3V1_TAG_F64_LE) => out.extend_from_slice(&r.to_bits().to_le_bytes()),
        (RawField::Blob(b), P3V1_TAG_BLOB) | (RawField::Text(b), P3V1_TAG_TEXT) => {
            out.extend_from_slice(b);
        }
        _ => unreachable!("p3v1 tag matched RawField"),
    }
}

#[inline(always)]
fn serial_type_for_raw_field(field: RawField<'_>) -> (u64, usize) {
    match field {
        RawField::Null => (0, 0),
        RawField::Int(i) => {
            if i == 0 {
                return (8, 0);
            }
            if i == 1 {
                return (9, 0);
            }
            let u = if i < 0 { (!i) as u64 } else { i as u64 };
            if u <= 127 {
                (1, 1)
            } else if u <= 32_767 {
                (2, 2)
            } else if u <= 8_388_607 {
                (3, 3)
            } else if u <= 2_147_483_647 {
                (4, 4)
            } else if u <= MAX_6BYTE_INT {
                (5, 6)
            } else {
                (6, 8)
            }
        }
        RawField::Float(r) => {
            if r.is_nan() {
                (0, 0)
            } else {
                (7, 8)
            }
        }
        RawField::Blob(b) => ((b.len() as u64) * 2 + 12, b.len()),
        RawField::Text(b) => ((b.len() as u64) * 2 + 13, b.len()),
    }
}

#[inline(always)]
fn write_format3_raw_field(out: &mut Vec<u8>, field: RawField<'_>, st: u64) {
    match (field, st) {
        (RawField::Null, _) | (_, 0 | 8 | 9) => {}
        (RawField::Int(i), 1) => out.push(i as u8),
        (RawField::Int(i), 2) => out.extend_from_slice(&(i as i16).to_be_bytes()),
        (RawField::Int(i), 3) => {
            let b = (i as i32).to_be_bytes();
            out.extend_from_slice(&b[1..4]);
        }
        (RawField::Int(i), 4) => out.extend_from_slice(&(i as i32).to_be_bytes()),
        (RawField::Int(i), 5) => {
            let b = i.to_be_bytes();
            out.extend_from_slice(&b[2..8]);
        }
        (RawField::Int(i), 6) => out.extend_from_slice(&i.to_be_bytes()),
        (RawField::Float(r), 7) => out.extend_from_slice(&r.to_bits().to_be_bytes()),
        (RawField::Blob(b) | RawField::Text(b), _) => out.extend_from_slice(b),
        _ => unreachable!(),
    }
}

/// Serializes borrowed [`RawField`] slices directly into `out` as `P3v1` or Format-3 without
/// intermediate [`Value`] allocations (used by `SingleRowUpdatePlan`, `SingleRowDeletePlan`,
/// and bulk `CREATE INDEX`).
pub fn encode_raw_fields_for_format_into(
    fields: &[RawField<'_>],
    is_p3v1: bool,
    out: &mut Vec<u8>,
) {
    if is_p3v1 {
        let n = fields.len();
        if n <= 16 {
            let mut hdr_buf = [0u8; 34];
            let mut tags = [0u8; 16];
            let mut body_len = 0usize;
            for (i, &f) in fields.iter().enumerate() {
                let (tag, len) = p3v1_tag_for_raw_field(f);
                body_len += len;
                tags[i] = tag;
                if body_len < 4096 {
                    let desc = ((body_len as u16) << 4) | u16::from(tag);
                    let db = desc.to_le_bytes();
                    hdr_buf[2 + 2 * i] = db[0];
                    hdr_buf[2 + 2 * i + 1] = db[1];
                }
            }
            if body_len < 4096 {
                hdr_buf[0] = P3V1_RECORD_MAGIC;
                hdr_buf[1] = n as u8;
                let hdr_len = 2 + 2 * n;
                out.clear();
                out.reserve(hdr_len + body_len);
                out.extend_from_slice(&hdr_buf[..hdr_len]);
                for (&f, &tag) in fields.iter().zip(tags[..n].iter()) {
                    write_p3v1_raw_field(out, f, tag);
                }
                return;
            }
        }
        if n < 128 {
            let mut hdr_buf = [0u8; 258];
            let mut tags = [0u8; 128];
            let mut body_len = 0usize;
            for (i, &f) in fields.iter().enumerate() {
                let (tag, len) = p3v1_tag_for_raw_field(f);
                body_len += len;
                tags[i] = tag;
                if body_len < 4096 {
                    let desc = ((body_len as u16) << 4) | u16::from(tag);
                    let db = desc.to_le_bytes();
                    hdr_buf[2 + 2 * i] = db[0];
                    hdr_buf[2 + 2 * i + 1] = db[1];
                }
            }
            if body_len < 4096 {
                hdr_buf[0] = P3V1_RECORD_MAGIC;
                hdr_buf[1] = n as u8;
                let hdr_len = 2 + 2 * n;
                out.clear();
                out.reserve(hdr_len + body_len);
                out.extend_from_slice(&hdr_buf[..hdr_len]);
                for (&f, &tag) in fields.iter().zip(tags[..n].iter()) {
                    write_p3v1_raw_field(out, f, tag);
                }
                return;
            }
        }
        if n <= usize::from(u16::MAX) {
            let hdr_len = 4 + 5 * n;
            let mut tags = Vec::with_capacity(n);
            let mut ends = Vec::with_capacity(n);
            let mut body_len = 0usize;
            for &f in fields {
                let (tag, len) = p3v1_tag_for_raw_field(f);
                body_len += len;
                tags.push(tag);
                ends.push(body_len as u32);
            }
            out.clear();
            out.reserve(hdr_len + body_len);
            out.push(P3V1_RECORD_MAGIC);
            out.push(P3V1_WIDE_FLAG);
            out.extend_from_slice(&(n as u16).to_le_bytes());
            out.extend_from_slice(&tags);
            for &end in &ends {
                out.extend_from_slice(&end.to_le_bytes());
            }
            for (&f, &tag) in fields.iter().zip(tags.iter()) {
                write_p3v1_raw_field(out, f, tag);
            }
            return;
        }
    }

    out.clear();
    if fields.len() <= 16 {
        let mut serials = [0u64; 16];
        let mut serial_bytes_len = 0usize;
        let mut body_len = 0usize;
        for (i, &f) in fields.iter().enumerate() {
            let (st, len) = serial_type_for_raw_field(f);
            serial_bytes_len += varint_len(st);
            body_len += len;
            serials[i] = st;
        }
        let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
        if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
            hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
        }
        out.reserve(hdr_len + body_len);
        push_varint(out, hdr_len as u64);
        for &st in &serials[..fields.len()] {
            push_varint(out, st);
        }
        for (&f, &st) in fields.iter().zip(serials[..fields.len()].iter()) {
            write_format3_raw_field(out, f, st);
        }
        return;
    }
    let mut serials = Vec::with_capacity(fields.len());
    let mut serial_bytes_len = 0usize;
    let mut body_len = 0usize;
    for &f in fields {
        let (st, len) = serial_type_for_raw_field(f);
        serial_bytes_len += varint_len(st);
        body_len += len;
        serials.push(st);
    }
    let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
    if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
        hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
    }
    out.reserve(hdr_len + body_len);
    push_varint(out, hdr_len as u64);
    for &st in &serials {
        push_varint(out, st);
    }
    for (&f, &st) in fields.iter().zip(serials.iter()) {
        write_format3_raw_field(out, f, st);
    }
}

/// Losslessly transcodes a `P3v1` record (`record[0] == 0x00`) into an equivalent Format-3
/// record in `out`, preserving raw `TEXT` / `BLOB` encodings (`UTF-8`, `UTF-16le`, `UTF-16be`)
/// byte-for-byte. If `record` is already Format-3, copies `record` into `out`.
pub fn transcode_p3v1_to_format3(record: &[u8], out: &mut Vec<u8>) -> Result<()> {
    if !is_p3v1_record(record) {
        out.clear();
        out.extend_from_slice(record);
        return Ok(());
    }
    let (n, _hdr_len, _is_wide) = p3v1_header_info(record)?;
    let mut fields = Vec::with_capacity(n);
    for col_idx in 0..n {
        let (start, end, tag) =
            p3v1_column_offset_and_tag(record, col_idx)?.ok_or_else(err_corrupt_p3v1_record)?;
        if end > record.len() {
            return Err(err_corrupt_p3v1_record());
        }
        fields.push(RawField::from_p3v1_tag(tag, &record[start..end])?);
    }
    encode_raw_fields_for_format_into(&fields, false, out);
    Ok(())
}

/// Decodes a single value given its `serial_type` and exact byte slice under UTF-8 (`sqlite3VdbeSerialGet`).
pub fn decode_serial_value(serial_type: u64, buf: &[u8]) -> Result<Value> {
    decode_serial_value_with_encoding(serial_type, buf, TextEncoding::Utf8)
}

/// Decodes a single value given its `serial_type` and exact byte slice under `enc` (`sqlite3VdbeSerialGet`).
pub fn decode_serial_value_with_encoding(
    serial_type: u64,
    buf: &[u8],
    enc: TextEncoding,
) -> Result<Value> {
    match serial_type {
        0 => Ok(Value::Null),
        1 => Ok(Value::Integer(i64::from(buf[0] as i8))),
        2 => {
            let v = i16::from_be_bytes([buf[0], buf[1]]);
            Ok(Value::Integer(i64::from(v)))
        }
        3 => {
            let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
            let v = i32::from_be_bytes([sign, buf[0], buf[1], buf[2]]);
            Ok(Value::Integer(i64::from(v)))
        }
        4 => {
            let v = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
            Ok(Value::Integer(i64::from(v)))
        }
        5 => {
            let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
            let v =
                i64::from_be_bytes([sign, sign, buf[0], buf[1], buf[2], buf[3], buf[4], buf[5]]);
            Ok(Value::Integer(v))
        }
        6 => {
            let v = i64::from_be_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]);
            Ok(Value::Integer(v))
        }
        7 => {
            let bits = u64::from_be_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]);
            let r = f64::from_bits(bits);
            Ok(Value::real(r))
        }
        8 => Ok(Value::Integer(0)),
        9 => Ok(Value::Integer(1)),
        10 | 11 => Err(SqliteError::corrupt(format!(
            "reserved serial type {serial_type}"
        ))),
        n if (n & 1) == 0 => Ok(Value::Blob(buf.to_vec())),
        _ => Ok(Value::Text(enc.decode_bytes(buf))),
    }
}

/// Decodes a single value given its `serial_type` and exact byte slice under `enc`
/// (`sqlite3VdbeSerialGet`), reusing recycled `String` / `Vec<u8>` allocations from `texts` / `blobs`
/// when available.
pub fn decode_serial_value_pooled(
    serial_type: u64,
    buf: &[u8],
    enc: TextEncoding,
    texts: &mut Vec<String>,
    blobs: &mut Vec<Vec<u8>>,
) -> Result<Value> {
    match serial_type {
        0 => Ok(Value::Null),
        1 => Ok(Value::Integer(i64::from(buf[0] as i8))),
        2 => {
            let v = i16::from_be_bytes([buf[0], buf[1]]);
            Ok(Value::Integer(i64::from(v)))
        }
        3 => {
            let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
            let v = i32::from_be_bytes([sign, buf[0], buf[1], buf[2]]);
            Ok(Value::Integer(i64::from(v)))
        }
        4 => {
            let v = i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
            Ok(Value::Integer(i64::from(v)))
        }
        5 => {
            let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
            let v =
                i64::from_be_bytes([sign, sign, buf[0], buf[1], buf[2], buf[3], buf[4], buf[5]]);
            Ok(Value::Integer(v))
        }
        6 => {
            let v = i64::from_be_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]);
            Ok(Value::Integer(v))
        }
        7 => {
            let bits = u64::from_be_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]);
            let r = f64::from_bits(bits);
            Ok(Value::real(r))
        }
        8 => Ok(Value::Integer(0)),
        9 => Ok(Value::Integer(1)),
        10 | 11 => Err(SqliteError::corrupt(format!(
            "reserved serial type {serial_type}"
        ))),
        n if (n & 1) == 0 => {
            let mut b = blobs.pop().unwrap_or_default();
            b.clear();
            b.extend_from_slice(buf);
            Ok(Value::Blob(b))
        }
        _ => match enc {
            TextEncoding::Utf8 => match std::str::from_utf8(buf) {
                Ok(s) => {
                    let mut out_s = texts.pop().unwrap_or_default();
                    out_s.clear();
                    out_s.push_str(s);
                    Ok(Value::Text(out_s))
                }
                Err(_) => Ok(Value::Text(String::from_utf8_lossy(buf).into_owned())),
            },
            _ => Ok(Value::Text(enc.decode_bytes(buf))),
        },
    }
}

/// Decodes a single `P3v1` column value given its 4-bit `tag` and exact byte slice under `enc`.
#[inline]
pub fn decode_p3v1_tag_value_with_encoding(
    tag: u8,
    buf: &[u8],
    enc: TextEncoding,
) -> Result<Value> {
    match tag {
        P3V1_TAG_NULL => Ok(Value::Null),
        P3V1_TAG_ZERO => Ok(Value::Integer(0)),
        P3V1_TAG_ONE => Ok(Value::Integer(1)),
        P3V1_TAG_I8 => Ok(Value::Integer(i64::from(buf[0] as i8))),
        P3V1_TAG_I16_LE => Ok(Value::Integer(i64::from(i16::from_le_bytes([
            buf[0], buf[1],
        ])))),
        P3V1_TAG_I32_LE => Ok(Value::Integer(i64::from(i32::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3],
        ])))),
        P3V1_TAG_I64_LE => Ok(Value::Integer(i64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]))),
        P3V1_TAG_F64_LE => {
            let bits = u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]);
            Ok(Value::real(f64::from_bits(bits)))
        }
        P3V1_TAG_EMPTY_BLOB => Ok(Value::Blob(Vec::new())),
        P3V1_TAG_EMPTY_TEXT => Ok(Value::Text(String::new())),
        P3V1_TAG_BLOB => Ok(Value::Blob(buf.to_vec())),
        P3V1_TAG_TEXT => Ok(Value::Text(enc.decode_bytes(buf))),
        _ => Err(err_corrupt_p3v1_record()),
    }
}

/// Decodes a single `P3v1` column value reusing recycled `String` / `Vec<u8>` buffers.
#[inline]
pub fn decode_p3v1_tag_value_pooled(
    tag: u8,
    buf: &[u8],
    enc: TextEncoding,
    texts: &mut Vec<String>,
    blobs: &mut Vec<Vec<u8>>,
) -> Result<Value> {
    match tag {
        P3V1_TAG_NULL => Ok(Value::Null),
        P3V1_TAG_ZERO => Ok(Value::Integer(0)),
        P3V1_TAG_ONE => Ok(Value::Integer(1)),
        P3V1_TAG_I8 => Ok(Value::Integer(i64::from(buf[0] as i8))),
        P3V1_TAG_I16_LE => Ok(Value::Integer(i64::from(i16::from_le_bytes([
            buf[0], buf[1],
        ])))),
        P3V1_TAG_I32_LE => Ok(Value::Integer(i64::from(i32::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3],
        ])))),
        P3V1_TAG_I64_LE => Ok(Value::Integer(i64::from_le_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]))),
        P3V1_TAG_F64_LE => {
            let bits = u64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]);
            Ok(Value::real(f64::from_bits(bits)))
        }
        P3V1_TAG_EMPTY_BLOB => {
            let mut b = blobs.pop().unwrap_or_default();
            b.clear();
            Ok(Value::Blob(b))
        }
        P3V1_TAG_EMPTY_TEXT => {
            let mut s = texts.pop().unwrap_or_default();
            s.clear();
            Ok(Value::Text(s))
        }
        P3V1_TAG_BLOB => {
            let mut b = blobs.pop().unwrap_or_default();
            b.clear();
            b.extend_from_slice(buf);
            Ok(Value::Blob(b))
        }
        P3V1_TAG_TEXT => match enc {
            TextEncoding::Utf8 => match std::str::from_utf8(buf) {
                Ok(s) => {
                    let mut out_s = texts.pop().unwrap_or_default();
                    out_s.clear();
                    out_s.push_str(s);
                    Ok(Value::Text(out_s))
                }
                Err(_) => Ok(Value::Text(String::from_utf8_lossy(buf).into_owned())),
            },
            _ => Ok(Value::Text(enc.decode_bytes(buf))),
        },
        _ => Err(err_corrupt_p3v1_record()),
    }
}

/// Returns the number of columns in `record` (`O(1)` for `P3v1`, varint header walk for Format-3).
#[inline]
pub fn record_column_count(record: &[u8]) -> Result<usize> {
    if record.is_empty() {
        return Ok(0);
    }
    if is_p3v1_record(record) {
        let (n, _, _) = p3v1_header_info(record)?;
        return Ok(n);
    }
    let (hdr_u64, mut hdr_pos) = get_varint(record)?;
    let hdr_size = usize::try_from(hdr_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > record.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }
    let mut n = 0usize;
    while hdr_pos < hdr_size {
        let (_, used) = get_varint(&record[hdr_pos..hdr_size])?;
        hdr_pos += used;
        n += 1;
    }
    Ok(n)
}

/// Decodes all columns from a serialized UTF-8 SQLite record.
pub fn decode_record(record: &[u8]) -> Result<Vec<Value>> {
    decode_record_with_encoding(record, TextEncoding::Utf8)
}

/// Decodes all columns from a serialized SQLite record using `enc`.
pub fn decode_record_with_encoding(record: &[u8], enc: TextEncoding) -> Result<Vec<Value>> {
    if record.is_empty() {
        return Ok(Vec::new());
    }
    if is_p3v1_record(record) {
        let (n, hdr_len, is_wide) = p3v1_header_info(record)?;
        let mut values = Vec::with_capacity(n);
        if !is_wide {
            let mut prev_end = 0usize;
            for col_idx in 0..n {
                let d = u16::from_le_bytes([record[2 + 2 * col_idx], record[3 + 2 * col_idx]]);
                let tag = (d & 0x0f) as u8;
                let end_rel = usize::from(d >> 4);
                if tag > P3V1_TAG_TEXT || prev_end > end_rel {
                    return Err(err_corrupt_p3v1_record());
                }
                if tag < P3V1_TAG_BLOB && (end_rel - prev_end) != p3v1_fixed_tag_width(tag) {
                    return Err(err_corrupt_p3v1_record());
                }
                let start = hdr_len + prev_end;
                let end = hdr_len + end_rel;
                if end > record.len() {
                    return Err(SqliteError::corrupt("record payload truncated"));
                }
                values.push(decode_p3v1_tag_value_with_encoding(
                    tag,
                    &record[start..end],
                    enc,
                )?);
                prev_end = end_rel;
            }
        } else {
            for col_idx in 0..n {
                let (start, end, tag) = p3v1_column_offset_and_tag(record, col_idx)?
                    .ok_or_else(err_corrupt_p3v1_record)?;
                if end > record.len() {
                    return Err(SqliteError::corrupt("record payload truncated"));
                }
                values.push(decode_p3v1_tag_value_with_encoding(
                    tag,
                    &record[start..end],
                    enc,
                )?);
            }
        }
        return Ok(values);
    }
    let (hdr_size_u64, mut hdr_pos) = get_varint(record)?;
    let hdr_size = usize::try_from(hdr_size_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > record.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }

    let mut body_pos = hdr_size;
    let mut values = Vec::new();
    while hdr_pos < hdr_size {
        let (st, used) = get_varint(&record[hdr_pos..hdr_size])?;
        hdr_pos += used;
        let len = serial_type_len(st)?;
        let end = body_pos
            .checked_add(len)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end > record.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        values.push(decode_serial_value_with_encoding(
            st,
            &record[body_pos..end],
            enc,
        )?);
        body_pos = end;
    }
    Ok(values)
}

/// Fast single-column extraction (`OP_Column`) under UTF-8: decodes only column `col_idx` (0-based).
pub fn decode_record_column(record: &[u8], col_idx: usize) -> Result<Option<Value>> {
    decode_record_column_with_encoding(record, col_idx, TextEncoding::Utf8)
}

/// Fast single-column extraction (`OP_Column`) under `enc`: decodes only column `col_idx` (0-based).
/// On `P3v1` records, projects `col_idx` in true `O(1)` time without scanning columns `0..col_idx`.
pub fn decode_record_column_with_encoding(
    record: &[u8],
    col_idx: usize,
    enc: TextEncoding,
) -> Result<Option<Value>> {
    if record.is_empty() {
        return Ok(None);
    }
    if is_p3v1_record(record) {
        let Some((start, end, tag)) = p3v1_column_offset_and_tag(record, col_idx)? else {
            return Ok(None);
        };
        if end > record.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        return Ok(Some(decode_p3v1_tag_value_with_encoding(
            tag,
            &record[start..end],
            enc,
        )?));
    }
    let (hdr_size_u64, mut hdr_pos) = get_varint(record)?;
    let hdr_size = usize::try_from(hdr_size_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > record.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }

    let mut body_pos = hdr_size;
    let mut idx = 0usize;
    while hdr_pos < hdr_size {
        let (st, used) = get_varint(&record[hdr_pos..hdr_size])?;
        hdr_pos += used;
        let len = serial_type_len(st)?;
        let end = body_pos
            .checked_add(len)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end > record.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        if idx == col_idx {
            return Ok(Some(decode_serial_value_with_encoding(
                st,
                &record[body_pos..end],
                enc,
            )?));
        }
        body_pos = end;
        idx += 1;
    }
    Ok(None)
}

/// Sort-order specification for a single key column in an Index B-Tree (`KeyInfo` in `sqliteInt.h`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySortOrder {
    pub desc: bool,
    pub nulls_last: bool,
    pub collation: Collation,
    pub custom_collation: Option<CustomCollation>,
}

impl Default for KeySortOrder {
    fn default() -> Self {
        Self {
            desc: false,
            nulls_last: false,
            collation: Collation::Binary,
            custom_collation: None,
        }
    }
}

impl KeySortOrder {
    pub fn collation_name(&self) -> &str {
        if let Some(ref cc) = self.custom_collation {
            &cc.name
        } else {
            match self.collation {
                Collation::Binary => "BINARY",
                Collation::NoCase => "NOCASE",
                Collation::RTrim => "RTRIM",
            }
        }
    }
}

#[inline(never)]
fn compare_raw_text_to_str(
    buf: &[u8],
    rs: &str,
    spec: &KeySortOrder,
    enc: TextEncoding,
) -> Result<Ordering> {
    if enc == TextEncoding::Utf8 && spec.custom_collation.is_none() {
        match spec.collation {
            Collation::Binary => {
                #[cfg(target_arch = "x86_64")]
                let fast = sqliters_asm::cmp_utf8_ascii_binary(buf, rs.as_bytes());
                #[cfg(not(target_arch = "x86_64"))]
                let fast = swar_cmp_ascii_binary(buf, rs.as_bytes());
                if let Some(ord) = fast {
                    return Ok(ord);
                }
            }
            Collation::NoCase => {
                #[cfg(target_arch = "x86_64")]
                let fast = sqliters_asm::cmp_utf8_ascii_nocase(buf, rs.as_bytes());
                #[cfg(not(target_arch = "x86_64"))]
                let fast = swar_cmp_ascii_nocase(buf, rs.as_bytes());
                if let Some(ord) = fast {
                    return Ok(ord);
                }
            }
            Collation::RTrim => {}
        }
    }
    if enc == TextEncoding::Utf8
        && let Ok(lhs_s) = std::str::from_utf8(buf)
    {
        if let Some(ref cc) = spec.custom_collation {
            (cc.comparator)(lhs_s, rs)
        } else {
            Ok(spec.collation.compare_str_with_encoding(lhs_s, rs, enc))
        }
    } else {
        let lhs_s = enc.decode_bytes(buf);
        if let Some(ref cc) = spec.custom_collation {
            (cc.comparator)(&lhs_s, rs)
        } else {
            Ok(spec.collation.compare_str_with_encoding(&lhs_s, rs, enc))
        }
    }
}

#[inline(never)]
fn compare_raw_text_slices(
    a: &[u8],
    b: &[u8],
    spec: &KeySortOrder,
    enc: TextEncoding,
) -> Result<Ordering> {
    if enc == TextEncoding::Utf8 && spec.custom_collation.is_none() {
        match spec.collation {
            Collation::Binary => {
                #[cfg(target_arch = "x86_64")]
                let fast = sqliters_asm::cmp_utf8_ascii_binary(a, b);
                #[cfg(not(target_arch = "x86_64"))]
                let fast = swar_cmp_ascii_binary(a, b);
                if let Some(ord) = fast {
                    return Ok(ord);
                }
            }
            Collation::NoCase => {
                #[cfg(target_arch = "x86_64")]
                let fast = sqliters_asm::cmp_utf8_ascii_nocase(a, b);
                #[cfg(not(target_arch = "x86_64"))]
                let fast = swar_cmp_ascii_nocase(a, b);
                if let Some(ord) = fast {
                    return Ok(ord);
                }
            }
            Collation::RTrim => {}
        }
    }
    if enc == TextEncoding::Utf8
        && let (Ok(sa), Ok(sb)) = (std::str::from_utf8(a), std::str::from_utf8(b))
    {
        if let Some(ref cc) = spec.custom_collation {
            (cc.comparator)(sa, sb)
        } else {
            Ok(spec.collation.compare_str_with_encoding(sa, sb, enc))
        }
    } else {
        let sa = enc.decode_bytes(a);
        let sb = enc.decode_bytes(b);
        if let Some(ref cc) = spec.custom_collation {
            (cc.comparator)(&sa, &sb)
        } else {
            Ok(spec.collation.compare_str_with_encoding(&sa, &sb, enc))
        }
    }
}

/// Compares two serialized SQLite index records under UTF-8 (`sqlite3VdbeRecordCompare`).
pub fn compare_records(
    lhs_record: &[u8],
    rhs_values: &[Value],
    orders: &[KeySortOrder],
) -> Result<Ordering> {
    compare_records_with_encoding(lhs_record, rhs_values, orders, TextEncoding::Utf8)
}

/// Compares two serialized SQLite index records under `enc` (`sqlite3VdbeRecordCompare`).
/// If `rhs_fields` has fewer fields than `lhs` (e.g. prefix index search), compares up to `min(lhs, rhs)`.
pub fn compare_records_with_encoding(
    lhs_record: &[u8],
    rhs_values: &[Value],
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Ordering> {
    if lhs_record.is_empty() {
        return Ok(Ordering::Equal);
    }
    if is_p3v1_record(lhs_record) {
        let (n, hdr_len, is_wide) = p3v1_header_info(lhs_record)?;
        let default_spec = KeySortOrder::default();
        let stop = n.min(rhs_values.len());
        if !is_wide {
            if n > 0 {
                let last_d =
                    u16::from_le_bytes([lhs_record[2 + 2 * (n - 1)], lhs_record[3 + 2 * (n - 1)]]);
                let last_tag = (last_d & 0x0f) as u8;
                let last_end = hdr_len + usize::from(last_d >> 4);
                if last_tag > P3V1_TAG_TEXT || last_end > lhs_record.len() {
                    return Err(err_corrupt_p3v1_record());
                }
            }
            let mut prev_end = 0usize;
            for col_idx in 0..stop {
                let d =
                    u16::from_le_bytes([lhs_record[2 + 2 * col_idx], lhs_record[3 + 2 * col_idx]]);
                let tag = (d & 0x0f) as u8;
                let end_rel = usize::from(d >> 4);
                if tag > P3V1_TAG_TEXT || prev_end > end_rel {
                    return Err(err_corrupt_p3v1_record());
                }
                if tag < P3V1_TAG_BLOB && (end_rel - prev_end) != p3v1_fixed_tag_width(tag) {
                    return Err(err_corrupt_p3v1_record());
                }
                let start = hdr_len + prev_end;
                let end = hdr_len + end_rel;
                if end > lhs_record.len() {
                    return Err(SqliteError::corrupt("record payload truncated"));
                }
                let f = RawField::from_p3v1_tag(tag, &lhs_record[start..end])?;
                let spec = orders.get(col_idx).unwrap_or(&default_spec);
                let ord = f.compare_key_to_value(&rhs_values[col_idx], spec, enc)?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
                prev_end = end_rel;
            }
            return Ok(Ordering::Equal);
        }
        for (col_idx, rhs_val) in rhs_values.iter().enumerate().take(stop) {
            let (start, end, tag) = p3v1_column_offset_and_tag(lhs_record, col_idx)?
                .ok_or_else(err_corrupt_p3v1_record)?;
            if end > lhs_record.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            let f = RawField::from_p3v1_tag(tag, &lhs_record[start..end])?;
            let spec = orders.get(col_idx).unwrap_or(&default_spec);
            let ord = f.compare_key_to_value(rhs_val, spec, enc)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
        }
        return Ok(Ordering::Equal);
    }
    let (hdr_size_u64, mut hdr_pos) = get_varint(lhs_record)?;
    let hdr_size = usize::try_from(hdr_size_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > lhs_record.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }

    let mut body_pos = hdr_size;
    let mut col_idx = 0usize;
    let mut decided = Ordering::Equal;
    let default_spec = KeySortOrder::default();

    while hdr_pos < hdr_size {
        let (st, used) = get_varint(&lhs_record[hdr_pos..hdr_size])?;
        hdr_pos += used;
        if matches!(st, 10 | 11) {
            return Err(SqliteError::corrupt(format!("reserved serial type {st}")));
        }
        let len = serial_type_len(st)?;
        let end = body_pos
            .checked_add(len)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end > lhs_record.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }

        if decided == Ordering::Equal && col_idx < rhs_values.len() {
            let spec = orders.get(col_idx).unwrap_or(&default_spec);
            let b = &rhs_values[col_idx];
            let buf = &lhs_record[body_pos..end];
            decided = if st < 12 {
                let a = decode_serial_value_with_encoding(st, buf, enc)?;
                match (a.is_null(), b.is_null()) {
                    (true, true) => Ordering::Equal,
                    (true, false) => {
                        if spec.nulls_last {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                    }
                    (false, true) => {
                        if spec.nulls_last {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
                    (false, false) => {
                        let raw = a.cmp_sqlite_fallible(b, spec, enc)?;
                        if spec.desc { raw.reverse() } else { raw }
                    }
                }
            } else if b.is_null() {
                if spec.nulls_last {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            } else if (st & 1) == 0 {
                let raw = match b {
                    Value::Blob(rb) => sqliters_asm::cmp_bytes_binary(buf, rb.as_slice()),
                    _ => Ordering::Greater,
                };
                if spec.desc { raw.reverse() } else { raw }
            } else {
                let raw = match b {
                    Value::Integer(_) | Value::Real(_) => Ordering::Greater,
                    Value::Blob(_) => Ordering::Less,
                    Value::Text(rs) => compare_raw_text_to_str(buf, rs.as_str(), spec, enc)?,
                    Value::Null => unreachable!(),
                };
                if spec.desc { raw.reverse() } else { raw }
            };
        }

        body_pos = end;
        col_idx += 1;
    }

    Ok(decided)
}

/// Borrowed field view decoded directly from a raw `P3v1` or Format-3 record slice without
/// heap allocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RawField<'a> {
    Null,
    Int(i64),
    Float(f64),
    Text(&'a [u8]),
    Blob(&'a [u8]),
}

impl<'a> RawField<'a> {
    #[inline(always)]
    pub fn from_p3v1_tag(tag: u8, buf: &'a [u8]) -> Result<Self> {
        match tag {
            P3V1_TAG_NULL => Ok(Self::Null),
            P3V1_TAG_ZERO => Ok(Self::Int(0)),
            P3V1_TAG_ONE => Ok(Self::Int(1)),
            P3V1_TAG_I8 => Ok(Self::Int(i64::from(buf[0] as i8))),
            P3V1_TAG_I16_LE => Ok(Self::Int(i64::from(i16::from_le_bytes([buf[0], buf[1]])))),
            P3V1_TAG_I32_LE => Ok(Self::Int(i64::from(i32::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3],
            ])))),
            P3V1_TAG_I64_LE => Ok(Self::Int(i64::from_le_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]))),
            P3V1_TAG_F64_LE => {
                let r = f64::from_bits(u64::from_le_bytes([
                    buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                ]));
                if r.is_nan() {
                    Ok(Self::Null)
                } else {
                    Ok(Self::Float(r))
                }
            }
            P3V1_TAG_EMPTY_BLOB => Ok(Self::Blob(&[])),
            P3V1_TAG_EMPTY_TEXT => Ok(Self::Text(&[])),
            P3V1_TAG_BLOB => Ok(Self::Blob(buf)),
            P3V1_TAG_TEXT => Ok(Self::Text(buf)),
            _ => Err(err_corrupt_p3v1_record()),
        }
    }

    #[inline(always)]
    pub fn from_serial(serial_type: u64, buf: &'a [u8]) -> Result<Self> {
        match serial_type {
            0 => Ok(Self::Null),
            1 => Ok(Self::Int(i64::from(buf[0] as i8))),
            2 => Ok(Self::Int(i64::from(i16::from_be_bytes([buf[0], buf[1]])))),
            3 => {
                let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
                Ok(Self::Int(i64::from(i32::from_be_bytes([
                    sign, buf[0], buf[1], buf[2],
                ]))))
            }
            4 => Ok(Self::Int(i64::from(i32::from_be_bytes([
                buf[0], buf[1], buf[2], buf[3],
            ])))),
            5 => {
                let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
                Ok(Self::Int(i64::from_be_bytes([
                    sign, sign, buf[0], buf[1], buf[2], buf[3], buf[4], buf[5],
                ])))
            }
            6 => Ok(Self::Int(i64::from_be_bytes([
                buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
            ]))),
            7 => {
                let r = f64::from_bits(u64::from_be_bytes([
                    buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
                ]));
                if r.is_nan() {
                    Ok(Self::Null)
                } else {
                    Ok(Self::Float(r))
                }
            }
            8 => Ok(Self::Int(0)),
            9 => Ok(Self::Int(1)),
            10 | 11 => Err(SqliteError::corrupt(format!(
                "reserved serial type {serial_type}"
            ))),
            n if (n & 1) == 0 => Ok(Self::Blob(buf)),
            _ => Ok(Self::Text(buf)),
        }
    }

    #[inline(always)]
    pub const fn is_null(self) -> bool {
        matches!(self, Self::Null)
    }

    #[inline(always)]
    const fn class_rank(self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Int(_) | Self::Float(_) => 1,
            Self::Text(_) => 2,
            Self::Blob(_) => 3,
        }
    }

    /// Materializes an owned [`Value`] from this borrowed field when needed.
    #[inline]
    pub fn to_owned_value(self, enc: TextEncoding) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Int(i) => Value::Integer(i),
            Self::Float(r) => Value::Real(r),
            Self::Blob(b) => Value::Blob(b.to_vec()),
            Self::Text(b) => Value::Text(enc.decode_bytes(b)),
        }
    }

    /// Compares `self` (encoded under `enc`) against a non-NULL `rhs` in raw SQL value order
    /// (without applying `spec.desc` or `spec.nulls_last`), matching `Value::cmp_sqlite_fallible`.
    #[inline]
    pub fn compare_sql_to_nonnull_value(
        self,
        rhs: &Value,
        spec: &KeySortOrder,
        enc: TextEncoding,
    ) -> Result<Ordering> {
        match (self, rhs) {
            (Self::Null, Value::Null) => Ok(Ordering::Equal),
            (Self::Null, _) => Ok(Ordering::Less),
            (_, Value::Null) => Ok(Ordering::Greater),
            (Self::Int(a), Value::Integer(b)) => Ok(a.cmp(b)),
            (Self::Float(a), Value::Real(b)) => {
                if b.is_nan() {
                    return Ok(Ordering::Greater);
                }
                if a == 0.0 && *b == 0.0 {
                    Ok(Ordering::Equal)
                } else {
                    Ok(a.total_cmp(b))
                }
            }
            (Self::Int(i), Value::Real(r)) => {
                if r.is_nan() {
                    Ok(Ordering::Greater)
                } else {
                    Ok(cmp_int_float(i, *r))
                }
            }
            (Self::Float(r), Value::Integer(i)) => Ok(cmp_int_float(*i, r).reverse()),
            (Self::Int(_) | Self::Float(_), Value::Text(_) | Value::Blob(_)) => Ok(Ordering::Less),
            (Self::Text(_), Value::Integer(_) | Value::Real(_)) => Ok(Ordering::Greater),
            (Self::Text(_), Value::Blob(_)) => Ok(Ordering::Less),
            (Self::Text(buf), Value::Text(rs)) => {
                compare_raw_text_to_str(buf, rs.as_str(), spec, enc)
            }
            (Self::Blob(buf), Value::Blob(rb)) => {
                Ok(sqliters_asm::cmp_bytes_binary(buf, rb.as_slice()))
            }
            (Self::Blob(_), _) => Ok(Ordering::Greater),
        }
    }

    /// Compares `self` against `rhs` in B-tree index key order (honoring `spec.nulls_last` and `spec.desc`).
    #[inline]
    pub fn compare_key_to_value(
        self,
        rhs: &Value,
        spec: &KeySortOrder,
        enc: TextEncoding,
    ) -> Result<Ordering> {
        let rhs_is_null = rhs.is_null();
        match (self.is_null(), rhs_is_null) {
            (true, true) => Ok(Ordering::Equal),
            (true, false) => Ok(if spec.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            }),
            (false, true) => Ok(if spec.nulls_last {
                Ordering::Less
            } else {
                Ordering::Greater
            }),
            (false, false) => {
                let raw = self.compare_sql_to_nonnull_value(rhs, spec, enc)?;
                Ok(if spec.desc { raw.reverse() } else { raw })
            }
        }
    }
}

/// Decodes up to `out_fields.len()` leading columns from `raw` in place with zero heap allocation,
/// returning the number of columns decoded (`0..=out_fields.len()`).
#[inline]
pub fn parse_raw_record_prefix<'a>(
    raw: &'a [u8],
    out_fields: &mut [RawField<'a>],
) -> Result<usize> {
    if raw.is_empty() || out_fields.is_empty() {
        return Ok(0);
    }
    if is_p3v1_record(raw) {
        let (n, hdr_len, is_wide) = p3v1_header_info(raw)?;
        let stop = n.min(out_fields.len());
        if !is_wide {
            let mut prev_end = 0usize;
            for (col_idx, dst) in out_fields[..stop].iter_mut().enumerate() {
                let d = u16::from_le_bytes([raw[2 + 2 * col_idx], raw[3 + 2 * col_idx]]);
                let tag = (d & 0x0f) as u8;
                let end_rel = usize::from(d >> 4);
                if tag > P3V1_TAG_TEXT || prev_end > end_rel {
                    return Err(err_corrupt_p3v1_record());
                }
                if tag < P3V1_TAG_BLOB && (end_rel - prev_end) != p3v1_fixed_tag_width(tag) {
                    return Err(err_corrupt_p3v1_record());
                }
                let start = hdr_len + prev_end;
                let end = hdr_len + end_rel;
                if end > raw.len() {
                    return Err(SqliteError::corrupt("record payload truncated"));
                }
                *dst = RawField::from_p3v1_tag(tag, &raw[start..end])?;
                prev_end = end_rel;
            }
            return Ok(stop);
        }
        for (col_idx, dst) in out_fields[..stop].iter_mut().enumerate() {
            let (start, end, tag) =
                p3v1_column_offset_and_tag(raw, col_idx)?.ok_or_else(err_corrupt_p3v1_record)?;
            if end > raw.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            *dst = RawField::from_p3v1_tag(tag, &raw[start..end])?;
        }
        return Ok(stop);
    }
    let (hdr_u64, mut hdr_pos) = get_varint(raw)?;
    let hdr_size = usize::try_from(hdr_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > raw.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }
    let mut body_pos = hdr_size;
    let mut idx = 0usize;
    let max_cols = out_fields.len();
    while idx < max_cols && hdr_pos < hdr_size {
        let (st, used) = get_varint(&raw[hdr_pos..hdr_size])?;
        hdr_pos += used;
        let len = serial_type_len(st)?;
        let end = body_pos
            .checked_add(len)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end > raw.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        out_fields[idx] = RawField::from_serial(st, &raw[body_pos..end])?;
        body_pos = end;
        idx += 1;
    }
    Ok(idx)
}

/// Reads a single column `col_idx` directly as a borrowed [`RawField`] without heap allocation.
/// On `P3v1` records, executes in true `O(1)` time without scanning columns `0..col_idx`.
#[inline]
pub fn read_raw_record_field(raw: &[u8], col_idx: usize) -> Result<Option<RawField<'_>>> {
    if raw.is_empty() {
        return Ok(None);
    }
    if is_p3v1_record(raw) {
        let Some((start, end, tag)) = p3v1_column_offset_and_tag(raw, col_idx)? else {
            return Ok(None);
        };
        if end > raw.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        return Ok(Some(RawField::from_p3v1_tag(tag, &raw[start..end])?));
    }
    let (hdr_u64, mut hdr_pos) = get_varint(raw)?;
    let hdr_size = usize::try_from(hdr_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > raw.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }
    let mut body_pos = hdr_size;
    let mut cur = 0usize;
    while hdr_pos < hdr_size {
        let (st, used) = get_varint(&raw[hdr_pos..hdr_size])?;
        hdr_pos += used;
        let len = serial_type_len(st)?;
        let end = body_pos
            .checked_add(len)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end > raw.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        if cur == col_idx {
            return Ok(Some(RawField::from_serial(st, &raw[body_pos..end])?));
        }
        if matches!(st, 10 | 11) {
            return Err(SqliteError::corrupt(format!("reserved serial type {st}")));
        }
        body_pos = end;
        cur += 1;
    }
    Ok(None)
}

/// Reads an integer column `col_idx` in `O(1)` time on `P3v1` records (with Format-3 fallback).
/// Returns `Ok(None)` if `col_idx >= col_count` or the column is not an SQL `INTEGER`.
#[inline(always)]
pub fn read_raw_record_int_at(raw: &[u8], col_idx: usize) -> Result<Option<i64>> {
    if raw.is_empty() {
        return Ok(None);
    }
    if raw[0] == P3V1_RECORD_MAGIC {
        let Some((start, end, tag)) = p3v1_column_offset_and_tag(raw, col_idx)? else {
            return Ok(None);
        };
        if end > raw.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        return Ok(decode_p3v1_int_tag(tag, &raw[start..end]));
    }
    Ok(match read_raw_record_field(raw, col_idx)? {
        Some(RawField::Int(v)) => Some(v),
        _ => None,
    })
}

/// Reads two integer columns `(col0, col1)` in `O(1)` time on `P3v1` records (with Format-3 fallback).
#[inline(always)]
pub fn read_raw_record_int_pair(
    raw: &[u8],
    col0: usize,
    col1: usize,
) -> Result<Option<(i64, i64)>> {
    if raw.len() >= 6 && raw[0] == P3V1_RECORD_MAGIC && raw[1] < P3V1_WIDE_FLAG {
        let n = usize::from(raw[1]);
        let hdr_len = 2 + 2 * n;
        if hdr_len > raw.len() {
            return Err(err_corrupt_p3v1_record());
        }
        if col0 == 0 && col1 == 1 && n >= 2 {
            let d0 = u16::from_le_bytes([raw[2], raw[3]]);
            let d1 = u16::from_le_bytes([raw[4], raw[5]]);
            let t0 = (d0 & 0x0f) as u8;
            let t1 = (d1 & 0x0f) as u8;
            let e0 = hdr_len + usize::from(d0 >> 4);
            let e1 = hdr_len + usize::from(d1 >> 4);
            if t0 > P3V1_TAG_TEXT || t1 > P3V1_TAG_TEXT || e0 > e1 || e1 > raw.len() {
                return Err(err_corrupt_p3v1_record());
            }
            let Some(v0) = decode_p3v1_int_tag(t0, &raw[hdr_len..e0]) else {
                return Ok(None);
            };
            let Some(v1) = decode_p3v1_int_tag(t1, &raw[e0..e1]) else {
                return Ok(None);
            };
            return Ok(Some((v0, v1)));
        }
        if col0 >= n || col1 >= n {
            return Ok(None);
        }
        let Some((s0, e0, t0)) = p3v1_column_offset_and_tag(raw, col0)? else {
            return Ok(None);
        };
        let Some((s1, e1, t1)) = p3v1_column_offset_and_tag(raw, col1)? else {
            return Ok(None);
        };
        if e0 > raw.len() || e1 > raw.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        let Some(v0) = decode_p3v1_int_tag(t0, &raw[s0..e0]) else {
            return Ok(None);
        };
        let Some(v1) = decode_p3v1_int_tag(t1, &raw[s1..e1]) else {
            return Ok(None);
        };
        return Ok(Some((v0, v1)));
    }
    let Some(v0) = read_raw_record_int_at(raw, col0)? else {
        return Ok(None);
    };
    let Some(v1) = read_raw_record_int_at(raw, col1)? else {
        return Ok(None);
    };
    Ok(Some((v0, v1)))
}

/// Reads column `col_idx` directly into an [`InlineVal`] in `O(1)` time on `P3v1` records.
/// Returns `Ok(None)` when `col_idx >= col_count`, `Ok(Some(Some(iv)))` when inline-decodable,
/// or `Ok(Some(None))` when the column is a long text/blob requiring heap materialization.
#[inline(always)]
pub fn read_raw_record_inline_val(
    raw: &[u8],
    col_idx: usize,
    enc: TextEncoding,
) -> Result<Option<Option<InlineVal>>> {
    if raw.is_empty() {
        return Ok(None);
    }
    if raw[0] == P3V1_RECORD_MAGIC {
        let Some((start, end, tag)) = p3v1_column_offset_and_tag(raw, col_idx)? else {
            return Ok(None);
        };
        if end > raw.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        return Ok(Some(InlineVal::try_from_p3v1_bytes(
            tag,
            &raw[start..end],
            enc,
        )?));
    }
    Ok(read_raw_record_field(raw, col_idx)?.map(|rf| InlineVal::try_from_raw_field(rf, enc)))
}

/// Compares a raw serialized index record `rec` against `prefix_values` plus optional
/// `lower_bound` / `upper_bound` on column `k = prefix_values.len()` with zero heap allocations.
///
/// Returns:
/// - `Ordering::Less` if `rec` precedes the target index range in B-tree key order,
/// - `Ordering::Equal` if `rec` lies within the target index range,
/// - `Ordering::Greater` if `rec` follows the target index range in B-tree key order.
pub fn compare_raw_record_range(
    rec: &[u8],
    prefix_values: &[Value],
    lower_bound: Option<(&Value, bool)>,
    upper_bound: Option<(&Value, bool)>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Ordering> {
    let k = prefix_values.len();
    let has_bounds = lower_bound.is_some() || upper_bound.is_some();
    if rec.is_empty() {
        return Ok(if has_bounds {
            Ordering::Less
        } else {
            Ordering::Equal
        });
    }
    if rec[0] == P3V1_RECORD_MAGIC {
        let (ncols, hdr_len, is_wide) = p3v1_header_info(rec)?;
        if !is_wide && ncols > 0 {
            let last_d = u16::from_le_bytes([rec[2 + 2 * (ncols - 1)], rec[3 + 2 * (ncols - 1)]]);
            if (last_d & 0x0f) as u8 > P3V1_TAG_TEXT
                || hdr_len + usize::from(last_d >> 4) > rec.len()
            {
                return Err(err_corrupt_p3v1_record());
            }
        }
        let default_so = KeySortOrder::default();
        let prefix_stop = k.min(ncols);
        for (col_idx, pref_val) in prefix_values.iter().enumerate().take(prefix_stop) {
            let (start, end, tag) =
                p3v1_column_offset_and_tag(rec, col_idx)?.ok_or_else(err_corrupt_p3v1_record)?;
            if end > rec.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            let f = RawField::from_p3v1_tag(tag, &rec[start..end])?;
            let spec = orders.get(col_idx).unwrap_or(&default_so);
            let ord = f.compare_key_to_value(pref_val, spec, enc)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
        }
        if !has_bounds {
            return Ok(Ordering::Equal);
        }
        if k >= ncols {
            return Ok(Ordering::Less);
        }
        let (start, end, tag) =
            p3v1_column_offset_and_tag(rec, k)?.ok_or_else(err_corrupt_p3v1_record)?;
        if end > rec.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        let f = RawField::from_p3v1_tag(tag, &rec[start..end])?;
        let range_so = orders.get(k).unwrap_or(&default_so);
        if f.is_null() {
            return Ok(if range_so.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            });
        }
        if let Some((lo_val, lo_inc)) = lower_bound
            && !lo_val.is_null()
        {
            let cmp = f.compare_sql_to_nonnull_value(lo_val, range_so, enc)?;
            if cmp == Ordering::Less || (!lo_inc && cmp == Ordering::Equal) {
                return Ok(if range_so.desc {
                    Ordering::Greater
                } else {
                    Ordering::Less
                });
            }
        }
        if let Some((hi_val, hi_inc)) = upper_bound {
            if hi_val.is_null() {
                return Ok(Ordering::Greater);
            }
            let cmp = f.compare_sql_to_nonnull_value(hi_val, range_so, enc)?;
            if cmp == Ordering::Greater || (!hi_inc && cmp == Ordering::Equal) {
                return Ok(if range_so.desc {
                    Ordering::Less
                } else {
                    Ordering::Greater
                });
            }
        }
        return Ok(Ordering::Equal);
    }
    // 32-bit SWAR fast path for 2- and 3-column single-byte-header index records
    // (`[0x03, st0, st1, ...]` and `[0x04, st0, st1, st2, ...]`).
    if rec.len() >= 4 && k <= 2 {
        let w32 = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
        let hdr_size = (w32 & 0xff) as usize;
        if (hdr_size == 4 && (w32 & 0x8080_8080) == 0)
            || (hdr_size == 3 && (w32 & 0x0080_8080) == 0)
        {
            let st0 = ((w32 >> 8) & 0xff) as u8;
            let st1 = ((w32 >> 16) & 0xff) as u8;
            let l0 = swar_serial_len_1b(st0);
            let l1 = swar_serial_len_1b(st1);
            let l2 = if hdr_size == 4 {
                swar_serial_len_1b(((w32 >> 24) & 0xff) as u8)
            } else {
                0
            };
            if ((l0 | l1 | l2) & 0x80) == 0
                && hdr_size + usize::from(l0) + usize::from(l1) + usize::from(l2) <= rec.len()
            {
                let default_so = KeySortOrder::default();
                let ncols = hdr_size - 1;
                let sts = [st0, st1, ((w32 >> 24) & 0xff) as u8];
                let lens = [usize::from(l0), usize::from(l1), usize::from(l2)];
                let mut body_pos = hdr_size;
                let prefix_stop = k.min(ncols);
                for col_idx in 0..prefix_stop {
                    let end = body_pos + lens[col_idx];
                    let f = RawField::from_serial(u64::from(sts[col_idx]), &rec[body_pos..end])?;
                    let spec = orders.get(col_idx).unwrap_or(&default_so);
                    let ord = f.compare_key_to_value(&prefix_values[col_idx], spec, enc)?;
                    if ord != Ordering::Equal {
                        return Ok(ord);
                    }
                    body_pos = end;
                }
                if !has_bounds {
                    return Ok(Ordering::Equal);
                }
                if k >= ncols {
                    return Ok(Ordering::Less);
                }
                let end = body_pos + lens[k];
                let f = RawField::from_serial(u64::from(sts[k]), &rec[body_pos..end])?;
                let range_so = orders.get(k).unwrap_or(&default_so);
                if f.is_null() {
                    return Ok(if range_so.nulls_last {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    });
                }
                if let Some((lo_val, lo_inc)) = lower_bound
                    && !lo_val.is_null()
                {
                    let cmp = f.compare_sql_to_nonnull_value(lo_val, range_so, enc)?;
                    if cmp == Ordering::Less || (!lo_inc && cmp == Ordering::Equal) {
                        return Ok(if range_so.desc {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        });
                    }
                }
                if let Some((hi_val, hi_inc)) = upper_bound {
                    if hi_val.is_null() {
                        return Ok(Ordering::Greater);
                    }
                    let cmp = f.compare_sql_to_nonnull_value(hi_val, range_so, enc)?;
                    if cmp == Ordering::Greater || (!hi_inc && cmp == Ordering::Equal) {
                        return Ok(if range_so.desc {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        });
                    }
                }
                return Ok(Ordering::Equal);
            }
        }
    }
    let (hdr_u64, mut hdr_pos) = get_varint(rec)?;
    let hdr_size = usize::try_from(hdr_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > rec.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }

    let mut body_pos = hdr_size;
    let mut col_idx = 0usize;
    let mut decided = Ordering::Equal;
    let mut saw_range_col = false;
    let default_so = KeySortOrder::default();

    while hdr_pos < hdr_size {
        let (st, used) = get_varint(&rec[hdr_pos..hdr_size])?;
        hdr_pos += used;
        if matches!(st, 10 | 11) {
            return Err(SqliteError::corrupt(format!("reserved serial type {st}")));
        }
        let len = serial_type_len(st)?;
        let end = body_pos
            .checked_add(len)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end > rec.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }

        if decided == Ordering::Equal {
            if col_idx < k {
                let f = RawField::from_serial(st, &rec[body_pos..end])?;
                let spec = orders.get(col_idx).unwrap_or(&default_so);
                decided = f.compare_key_to_value(&prefix_values[col_idx], spec, enc)?;
            } else if col_idx == k && has_bounds {
                saw_range_col = true;
                let f = RawField::from_serial(st, &rec[body_pos..end])?;
                let range_so = orders.get(k).unwrap_or(&default_so);
                if f.is_null() {
                    decided = if range_so.nulls_last {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    };
                } else {
                    if let Some((lo_val, lo_inc)) = lower_bound
                        && !lo_val.is_null()
                    {
                        let cmp = f.compare_sql_to_nonnull_value(lo_val, range_so, enc)?;
                        if cmp == Ordering::Less || (!lo_inc && cmp == Ordering::Equal) {
                            decided = if range_so.desc {
                                Ordering::Greater
                            } else {
                                Ordering::Less
                            };
                        }
                    }
                    if decided == Ordering::Equal
                        && let Some((hi_val, hi_inc)) = upper_bound
                    {
                        if hi_val.is_null() {
                            decided = Ordering::Greater;
                        } else {
                            let cmp = f.compare_sql_to_nonnull_value(hi_val, range_so, enc)?;
                            if cmp == Ordering::Greater || (!hi_inc && cmp == Ordering::Equal) {
                                decided = if range_so.desc {
                                    Ordering::Less
                                } else {
                                    Ordering::Greater
                                };
                            }
                        }
                    }
                }
            }
        }

        body_pos = end;
        col_idx += 1;
    }

    if decided == Ordering::Equal && has_bounds && !saw_range_col {
        return Ok(Ordering::Less);
    }
    Ok(decided)
}

#[inline]
fn compare_raw_fields(
    lhs: RawField<'_>,
    rhs: RawField<'_>,
    spec: &KeySortOrder,
    enc: TextEncoding,
) -> Result<Ordering> {
    match (lhs, rhs) {
        (RawField::Null, RawField::Null) => return Ok(Ordering::Equal),
        (RawField::Null, _) => {
            return Ok(if spec.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            });
        }
        (_, RawField::Null) => {
            return Ok(if spec.nulls_last {
                Ordering::Less
            } else {
                Ordering::Greater
            });
        }
        _ => {}
    }

    let ra = lhs.class_rank();
    let rb = rhs.class_rank();
    let raw = if ra != rb {
        ra.cmp(&rb)
    } else {
        match (lhs, rhs) {
            (RawField::Int(a), RawField::Int(b)) => a.cmp(&b),
            (RawField::Float(a), RawField::Float(b)) => {
                if a == 0.0 && b == 0.0 {
                    Ordering::Equal
                } else {
                    a.total_cmp(&b)
                }
            }
            (RawField::Int(i), RawField::Float(r)) => cmp_int_float(i, r),
            (RawField::Float(r), RawField::Int(i)) => cmp_int_float(i, r).reverse(),
            (RawField::Blob(a), RawField::Blob(b)) => sqliters_asm::cmp_bytes_binary(a, b),
            (RawField::Text(a), RawField::Text(b)) => compare_raw_text_slices(a, b, spec, enc)?,
            _ => unreachable!(),
        }
    };
    Ok(if spec.desc { raw.reverse() } else { raw })
}

#[inline(always)]
fn decode_int_serial_1b(st: u8, bytes: &[u8]) -> Option<i64> {
    match st {
        8 => Some(0),
        9 => Some(1),
        1 => Some(i64::from(bytes[0] as i8)),
        2 => Some(i64::from(i16::from_be_bytes([bytes[0], bytes[1]]))),
        3 => {
            let sign_ext = (bytes[0] as i8) as i32;
            Some(i64::from(
                (sign_ext << 16) | ((bytes[1] as i32) << 8) | (bytes[2] as i32),
            ))
        }
        4 => Some(i64::from(i32::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3],
        ]))),
        5 => {
            let hi = i16::from_be_bytes([bytes[0], bytes[1]]) as i64;
            let lo = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as i64;
            Some((hi << 32) | lo)
        }
        6 => Some(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        _ => None,
    }
}

/// Compares two serialized SQLite records directly on raw byte slices without decoding into
/// heap [`Value`] vectors (`sqlite3VdbeRecordCompare`).
///
/// Compares up to `min(lhs_cols, rhs_cols, max_fields.unwrap_or(usize::MAX))` columns.
pub fn compare_raw_records_with_encoding(
    lhs_record: &[u8],
    rhs_record: &[u8],
    max_fields: Option<usize>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Ordering> {
    if lhs_record.is_empty() || rhs_record.is_empty() {
        return Ok(Ordering::Equal);
    }

    let lh0 = lhs_record[0];
    let rh0 = rhs_record[0];

    // Native `P3v1` compact-vs-compact fast path (used by bulk `CREATE INDEX` sorting and index
    // leaf splits/comparisons).
    if lh0 == P3V1_RECORD_MAGIC
        && rh0 == P3V1_RECORD_MAGIC
        && lhs_record.len() >= 2
        && rhs_record.len() >= 2
        && lhs_record[1] < P3V1_WIDE_FLAG
        && rhs_record[1] < P3V1_WIDE_FLAG
    {
        let ln = usize::from(lhs_record[1]);
        let rn = usize::from(rhs_record[1]);
        let l_hdr = 2 + 2 * ln;
        let r_hdr = 2 + 2 * rn;
        if l_hdr > lhs_record.len() || r_hdr > rhs_record.len() {
            return Err(err_corrupt_p3v1_record());
        }
        let stop = ln.min(rn).min(max_fields.unwrap_or(usize::MAX));
        let mut l_prev = 0usize;
        let mut r_prev = 0usize;
        let default_spec = KeySortOrder::default();
        for col_idx in 0..stop {
            let da = u16::from_le_bytes([lhs_record[2 + 2 * col_idx], lhs_record[3 + 2 * col_idx]]);
            let db = u16::from_le_bytes([rhs_record[2 + 2 * col_idx], rhs_record[3 + 2 * col_idx]]);
            let ta = (da & 0x0f) as u8;
            let tb = (db & 0x0f) as u8;
            let ea_rel = usize::from(da >> 4);
            let eb_rel = usize::from(db >> 4);
            if ta > P3V1_TAG_TEXT || tb > P3V1_TAG_TEXT || l_prev > ea_rel || r_prev > eb_rel {
                return Err(err_corrupt_p3v1_record());
            }
            if (ta < P3V1_TAG_BLOB && (ea_rel - l_prev) != p3v1_fixed_tag_width(ta))
                || (tb < P3V1_TAG_BLOB && (eb_rel - r_prev) != p3v1_fixed_tag_width(tb))
            {
                return Err(err_corrupt_p3v1_record());
            }
            let sa = l_hdr + l_prev;
            let ea = l_hdr + ea_rel;
            let sb = r_hdr + r_prev;
            let eb = r_hdr + eb_rel;
            if ea > lhs_record.len() || eb > rhs_record.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            let spec = orders.get(col_idx).unwrap_or(&default_spec);
            let cmp = if let (Some(ia), Some(ib)) = (
                decode_p3v1_int_tag(ta, &lhs_record[sa..ea]),
                decode_p3v1_int_tag(tb, &rhs_record[sb..eb]),
            ) {
                let raw = ia.cmp(&ib);
                if spec.desc { raw.reverse() } else { raw }
            } else if enc == TextEncoding::Utf8
                && matches!(ta, P3V1_TAG_TEXT | P3V1_TAG_EMPTY_TEXT)
                && matches!(tb, P3V1_TAG_TEXT | P3V1_TAG_EMPTY_TEXT)
                && spec.collation == Collation::Binary
                && spec.custom_collation.is_none()
            {
                let ba = &lhs_record[sa..ea];
                let bb = &rhs_record[sb..eb];
                if let Some(raw) = sqliters_asm::cmp_utf8_ascii_binary(ba, bb) {
                    if spec.desc { raw.reverse() } else { raw }
                } else {
                    compare_raw_fields(RawField::Text(ba), RawField::Text(bb), spec, enc)?
                }
            } else {
                let fa = RawField::from_p3v1_tag(ta, &lhs_record[sa..ea])?;
                let fb = RawField::from_p3v1_tag(tb, &rhs_record[sb..eb])?;
                compare_raw_fields(fa, fb, spec, enc)?
            };
            if cmp != Ordering::Equal {
                return Ok(cmp);
            }
            l_prev = ea_rel;
            r_prev = eb_rel;
        }
        return Ok(Ordering::Equal);
    }

    if lh0 == P3V1_RECORD_MAGIC || rh0 == P3V1_RECORD_MAGIC {
        let ln = record_column_count(lhs_record)?;
        let rn = record_column_count(rhs_record)?;
        let stop = ln.min(rn).min(max_fields.unwrap_or(usize::MAX));
        let default_spec = KeySortOrder::default();
        for col_idx in 0..stop {
            let fa = read_raw_record_field(lhs_record, col_idx)?
                .ok_or_else(|| SqliteError::corrupt("record payload truncated"))?;
            let fb = read_raw_record_field(rhs_record, col_idx)?
                .ok_or_else(|| SqliteError::corrupt("record payload truncated"))?;
            let spec = orders.get(col_idx).unwrap_or(&default_spec);
            let cmp = compare_raw_fields(fa, fb, spec, enc)?;
            if cmp != Ordering::Equal {
                return Ok(cmp);
            }
        }
        return Ok(Ordering::Equal);
    }

    // Fast path for UTF-8 Format-3 records with 1-byte header sizes (`2..=8`) and 1-byte serial types.
    if enc == TextEncoding::Utf8
        && (2..=8).contains(&lh0)
        && (2..=8).contains(&rh0)
        && usize::from(lh0) <= lhs_record.len()
        && usize::from(rh0) <= rhs_record.len()
    {
        let l_hdr = usize::from(lh0);
        let r_hdr = usize::from(rh0);
        let stop = (l_hdr - 1)
            .min(r_hdr - 1)
            .min(max_fields.unwrap_or(usize::MAX));
        let mut l_body = l_hdr;
        let mut r_body = r_hdr;
        let mut fast_ok = true;
        let default_spec = KeySortOrder::default();
        for col_idx in 0..stop {
            let st_a = lhs_record[1 + col_idx];
            let st_b = rhs_record[1 + col_idx];
            let la = swar_serial_len_1b(st_a);
            let lb = swar_serial_len_1b(st_b);
            if ((st_a | st_b | la | lb) & 0x80) != 0 {
                fast_ok = false;
                break;
            }
            let end_a = l_body + usize::from(la);
            let end_b = r_body + usize::from(lb);
            if end_a > lhs_record.len() || end_b > rhs_record.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            let spec = orders.get(col_idx).unwrap_or(&default_spec);
            let cmp = if let (Some(ia), Some(ib)) = (
                decode_int_serial_1b(st_a, &lhs_record[l_body..end_a]),
                decode_int_serial_1b(st_b, &rhs_record[r_body..end_b]),
            ) {
                let raw = ia.cmp(&ib);
                if spec.desc { raw.reverse() } else { raw }
            } else if st_a >= 13
                && st_b >= 13
                && ((st_a & st_b) & 1) == 1
                && spec.collation == Collation::Binary
                && spec.custom_collation.is_none()
            {
                let sa = &lhs_record[l_body..end_a];
                let sb = &rhs_record[r_body..end_b];
                if let Some(raw) = sqliters_asm::cmp_utf8_ascii_binary(sa, sb) {
                    if spec.desc { raw.reverse() } else { raw }
                } else {
                    let fa = RawField::Text(sa);
                    let fb = RawField::Text(sb);
                    compare_raw_fields(fa, fb, spec, enc)?
                }
            } else {
                let fa = RawField::from_serial(u64::from(st_a), &lhs_record[l_body..end_a])?;
                let fb = RawField::from_serial(u64::from(st_b), &rhs_record[r_body..end_b])?;
                compare_raw_fields(fa, fb, spec, enc)?
            };
            if cmp != Ordering::Equal {
                return Ok(cmp);
            }
            l_body = end_a;
            r_body = end_b;
        }
        if fast_ok {
            return Ok(Ordering::Equal);
        }
    }

    let (lhs_hdr_u64, mut lhs_hdr_pos) = get_varint(lhs_record)?;
    let lhs_hdr_size = usize::try_from(lhs_hdr_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if lhs_hdr_size > lhs_record.len() || lhs_hdr_pos > lhs_hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }

    let (rhs_hdr_u64, mut rhs_hdr_pos) = get_varint(rhs_record)?;
    let rhs_hdr_size = usize::try_from(rhs_hdr_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if rhs_hdr_size > rhs_record.len() || rhs_hdr_pos > rhs_hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }

    let mut lhs_body_pos = lhs_hdr_size;
    let mut rhs_body_pos = rhs_hdr_size;
    let mut col_idx = 0usize;
    let limit = max_fields.unwrap_or(usize::MAX);
    let default_spec = KeySortOrder::default();

    while col_idx < limit && lhs_hdr_pos < lhs_hdr_size && rhs_hdr_pos < rhs_hdr_size {
        let (st_a, used_a) = get_varint(&lhs_record[lhs_hdr_pos..lhs_hdr_size])?;
        lhs_hdr_pos += used_a;
        let len_a = serial_type_len(st_a)?;
        let end_a = lhs_body_pos
            .checked_add(len_a)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end_a > lhs_record.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }

        let (st_b, used_b) = get_varint(&rhs_record[rhs_hdr_pos..rhs_hdr_size])?;
        rhs_hdr_pos += used_b;
        let len_b = serial_type_len(st_b)?;
        let end_b = rhs_body_pos
            .checked_add(len_b)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end_b > rhs_record.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }

        let fa = RawField::from_serial(st_a, &lhs_record[lhs_body_pos..end_a])?;
        let fb = RawField::from_serial(st_b, &rhs_record[rhs_body_pos..end_b])?;
        let spec = orders.get(col_idx).unwrap_or(&default_spec);
        let cmp = compare_raw_fields(fa, fb, spec, enc)?;
        if cmp != Ordering::Equal {
            return Ok(cmp);
        }

        lhs_body_pos = end_a;
        rhs_body_pos = end_b;
        col_idx += 1;
    }

    Ok(Ordering::Equal)
}

/// Returns `true` if any of the first `prefix_cols` fields of `record` is SQL `NULL`
/// (or if `record` has fewer than `prefix_cols` fields).
pub fn raw_record_prefix_has_null(record: &[u8], prefix_cols: usize) -> Result<bool> {
    if prefix_cols == 0 {
        return Ok(false);
    }
    if record.is_empty() {
        return Ok(true);
    }
    if record[0] == P3V1_RECORD_MAGIC {
        let (n, hdr_len, is_wide) = p3v1_header_info(record)?;
        if n < prefix_cols {
            return Ok(true);
        }
        if !is_wide {
            let mut prev_end = 0usize;
            for col_idx in 0..prefix_cols {
                let d = u16::from_le_bytes([record[2 + 2 * col_idx], record[3 + 2 * col_idx]]);
                let tag = (d & 0x0f) as u8;
                let end_rel = usize::from(d >> 4);
                if tag > P3V1_TAG_TEXT || prev_end > end_rel {
                    return Err(err_corrupt_p3v1_record());
                }
                if tag < P3V1_TAG_BLOB && (end_rel - prev_end) != p3v1_fixed_tag_width(tag) {
                    return Err(err_corrupt_p3v1_record());
                }
                let start = hdr_len + prev_end;
                let end = hdr_len + end_rel;
                if end > record.len() {
                    return Err(SqliteError::corrupt("record payload truncated"));
                }
                if tag == P3V1_TAG_NULL {
                    return Ok(true);
                }
                if tag == P3V1_TAG_F64_LE {
                    let bits = u64::from_le_bytes([
                        record[start],
                        record[start + 1],
                        record[start + 2],
                        record[start + 3],
                        record[start + 4],
                        record[start + 5],
                        record[start + 6],
                        record[start + 7],
                    ]);
                    if f64::from_bits(bits).is_nan() {
                        return Ok(true);
                    }
                }
                prev_end = end_rel;
            }
            return Ok(false);
        }
        for col_idx in 0..prefix_cols {
            let (start, end, tag) =
                p3v1_column_offset_and_tag(record, col_idx)?.ok_or_else(err_corrupt_p3v1_record)?;
            if end > record.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            if tag == P3V1_TAG_NULL {
                return Ok(true);
            }
            if tag == P3V1_TAG_F64_LE {
                let bits = u64::from_le_bytes([
                    record[start],
                    record[start + 1],
                    record[start + 2],
                    record[start + 3],
                    record[start + 4],
                    record[start + 5],
                    record[start + 6],
                    record[start + 7],
                ]);
                if f64::from_bits(bits).is_nan() {
                    return Ok(true);
                }
            }
        }
        return Ok(false);
    }
    let (hdr_u64, mut hdr_pos) = get_varint(record)?;
    let hdr_size = usize::try_from(hdr_u64)
        .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
    if hdr_size > record.len() || hdr_pos > hdr_size {
        return Err(SqliteError::corrupt("record header exceeds record length"));
    }
    let mut body_pos = hdr_size;
    for _ in 0..prefix_cols {
        if hdr_pos >= hdr_size {
            return Ok(true);
        }
        let (st, used) = get_varint(&record[hdr_pos..hdr_size])?;
        hdr_pos += used;
        let len = serial_type_len(st)?;
        let end = body_pos
            .checked_add(len)
            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
        if end > record.len() {
            return Err(SqliteError::corrupt("record payload truncated"));
        }
        if st == 0 {
            return Ok(true);
        }
        if st == 7 {
            let bits = u64::from_be_bytes([
                record[body_pos],
                record[body_pos + 1],
                record[body_pos + 2],
                record[body_pos + 3],
                record[body_pos + 4],
                record[body_pos + 5],
                record[body_pos + 6],
                record[body_pos + 7],
            ]);
            if f64::from_bits(bits).is_nan() {
                return Ok(true);
            }
        }
        body_pos = end;
    }
    Ok(false)
}

/// Stable fallible merge sort for slices whose comparator may return `Err(SqliteError)`
/// (e.g. custom collation errors or panics).
pub fn fallible_sort_by<T: Clone, F>(slice: &mut [T], mut compare: F) -> Result<()>
where
    F: FnMut(&T, &T) -> Result<Ordering>,
{
    fn merge_sort_rec<T: Clone, F>(
        slice: &mut [T],
        scratch: &mut [T],
        compare: &mut F,
    ) -> Result<()>
    where
        F: FnMut(&T, &T) -> Result<Ordering>,
    {
        let len = slice.len();
        if len <= 1 {
            return Ok(());
        }
        let mid = len / 2;
        merge_sort_rec(&mut slice[..mid], &mut scratch[..mid], compare)?;
        merge_sort_rec(&mut slice[mid..], &mut scratch[mid..], compare)?;

        let mut i = 0;
        let mut j = mid;
        let mut k = 0;
        while i < mid && j < len {
            if compare(&slice[i], &slice[j])? != Ordering::Greater {
                scratch[k] = slice[i].clone();
                i += 1;
            } else {
                scratch[k] = slice[j].clone();
                j += 1;
            }
            k += 1;
        }
        while i < mid {
            scratch[k] = slice[i].clone();
            i += 1;
            k += 1;
        }
        while j < len {
            scratch[k] = slice[j].clone();
            j += 1;
            k += 1;
        }
        slice.clone_from_slice(&scratch[..len]);
        Ok(())
    }

    if slice.len() <= 1 {
        return Ok(());
    }
    let mut scratch = slice.to_vec();
    merge_sort_rec(slice, &mut scratch, &mut compare)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RuntimeValue;

    #[test]
    fn record_codec_never_persists_runtime_subtype() {
        let tagged = RuntimeValue::with_subtype(Value::Text("{\"k\":1}".to_owned()), 74);
        assert_eq!(tagged.subtype, 74);
        let encoded = encode_record(std::slice::from_ref(&tagged.value));
        let plain_encoded = encode_record(&[Value::Text("{\"k\":1}".to_owned())]);
        assert_eq!(encoded, plain_encoded);

        let decoded = decode_record(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        let rt = RuntimeValue::from(decoded.into_iter().next().unwrap());
        assert_eq!(rt.subtype, 0);
    }

    #[test]
    fn p3v1_record_compact_wide_o1_projection_and_format3_fallback() {
        let cols = vec![
            Value::Null,
            Value::Integer(0),
            Value::Integer(1),
            Value::Integer(-128),
            Value::Integer(127),
            Value::Integer(-32_768),
            Value::Integer(32_767),
            Value::Integer(i32::MIN as i64),
            Value::Integer(i32::MAX as i64),
            Value::Integer(i64::MIN),
            Value::Integer(i64::MAX),
            Value::Real(-0.0),
            Value::Real(f64::NAN),
            Value::Real(std::f64::consts::PI),
            Value::Text(String::new()),
            Value::Text("merhaba-utf8-çğıöşü".to_owned()),
            Value::Blob(Vec::new()),
            Value::Blob(vec![0xde, 0xad, 0xbe, 0xef, 0x42]),
        ];

        let mut p3_bytes = Vec::new();
        encode_record_for_format_into(&cols, TextEncoding::Utf8, true, &mut p3_bytes);
        assert!(is_p3v1_record(&p3_bytes));
        assert!(p3_bytes[1] < P3V1_WIDE_FLAG);
        assert_eq!(record_column_count(&p3_bytes).unwrap(), cols.len());

        let decoded = decode_record_with_encoding(&p3_bytes, TextEncoding::Utf8).unwrap();
        assert_eq!(decoded.len(), cols.len());
        assert_eq!(decoded[0], Value::Null);
        assert_eq!(decoded[12], Value::Null); // NaN normalizes to SQL NULL
        assert_eq!(decoded[11], Value::Real(0.0));
        for idx in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 14, 15, 16, 17] {
            assert_eq!(decoded[idx], cols[idx], "mismatch at col {idx}");
            assert_eq!(
                decode_record_column_with_encoding(&p3_bytes, idx, TextEncoding::Utf8).unwrap(),
                Some(cols[idx].clone())
            );
            let rf = read_raw_record_field(&p3_bytes, idx).unwrap().unwrap();
            assert_eq!(rf.to_owned_value(TextEncoding::Utf8), cols[idx]);
        }
        assert_eq!(
            decode_record_column_with_encoding(&p3_bytes, 99, TextEncoding::Utf8).unwrap(),
            None
        );
        assert_eq!(
            read_raw_record_int_at(&p3_bytes, 9).unwrap(),
            Some(i64::MIN)
        );
        assert_eq!(
            read_raw_record_int_pair(&p3_bytes, 3, 10).unwrap(),
            Some((-128, i64::MAX))
        );

        // Transcode to Format-3 and verify cross-format raw comparison parity.
        let mut f3_bytes = Vec::new();
        transcode_p3v1_to_format3(&p3_bytes, &mut f3_bytes).unwrap();
        assert!(!is_p3v1_record(&f3_bytes));
        assert_eq!(
            decode_record_with_encoding(&f3_bytes, TextEncoding::Utf8).unwrap(),
            decoded
        );
        let orders = vec![KeySortOrder::default(); cols.len()];
        assert_eq!(
            compare_raw_records_with_encoding(
                &p3_bytes,
                &f3_bytes,
                None,
                &orders,
                TextEncoding::Utf8
            )
            .unwrap(),
            Ordering::Equal
        );

        // UTF-16le and UTF-16be round-trip on P3v1.
        for enc in [TextEncoding::Utf16Le, TextEncoding::Utf16Be] {
            let mut u16_rec = Vec::new();
            encode_record_for_format_into(&cols, enc, true, &mut u16_rec);
            assert_eq!(decode_record_with_encoding(&u16_rec, enc).unwrap(), decoded);
            assert_eq!(
                decode_record_column_with_encoding(&u16_rec, 15, enc).unwrap(),
                Some(cols[15].clone())
            );
        }

        // 2,000-column wide row (`SQLITE_MAX_COLUMN = 2000`) + >4 KiB body.
        let wide_cols: Vec<Value> = (0..2000_i64)
            .map(|i| match i % 5 {
                0 => Value::Integer(i * 100_003),
                1 => Value::Text(format!("col-{i:04}-payload")),
                2 => Value::Blob(vec![(i & 0xff) as u8; 6]),
                3 => Value::Real(i as f64 * 0.25),
                _ => Value::Null,
            })
            .collect();
        let mut wide_p3 = Vec::new();
        encode_record_for_format_into(&wide_cols, TextEncoding::Utf8, true, &mut wide_p3);
        assert!(is_p3v1_record(&wide_p3));
        assert_eq!(wide_p3[1], P3V1_WIDE_FLAG);
        assert_eq!(record_column_count(&wide_p3).unwrap(), 2000);
        for probe in [0usize, 1, 127, 128, 999, 1501, 1998, 1999] {
            assert_eq!(
                decode_record_column_with_encoding(&wide_p3, probe, TextEncoding::Utf8).unwrap(),
                Some(wide_cols[probe].clone())
            );
        }
        assert_eq!(
            decode_record_with_encoding(&wide_p3, TextEncoding::Utf8).unwrap(),
            wide_cols
        );

        // Corrupt non-monotonic descriptor offset rejection.
        let mut bad = vec![P3V1_RECORD_MAGIC, 2, 0x24, 0x00, 0x14, 0x00, 1, 2];
        assert!(decode_record(&bad).is_err());
        bad.truncate(3);
        assert!(decode_record(&bad).is_err());
    }
}
