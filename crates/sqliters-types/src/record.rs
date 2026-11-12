//! SQLite on-disk Record format (serial types + payloads),
//! ported from `OP_MakeRecord`, `sqlite3VdbeSerialTypeLen`, and `sqlite3VdbeSerialGet`
//! in `sqlite/src/vdbe.c` and `sqlite/src/vdbeaux.c`.
//!
//! Record binary layout:
//! ```text
//! [ header_size : varint ] [ serial_type_0 : varint ] ... [ serial_type_{N-1} : varint ]
//! [ body_0 ] ... [ body_{N-1} ]
//! ```
//! where `header_size` is the total number of bytes in the header INCLUDING `header_size` itself.

use std::cmp::Ordering;

use crate::error::{Result, SqliteError};
use crate::runtime_api::RuntimeValue;
use crate::value::{Collation, CustomCollation, TextEncoding, Value, cmp_int_float};
use crate::varint::{get_varint, push_varint, varint_len};

const MAX_6BYTE_INT: u64 = (0x0000_8000_u64 << 32) - 1; // 140_737_488_355_327

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
#[inline(always)]
pub fn serial_type_len(serial_type: u64) -> Result<usize> {
    if serial_type >= 12 {
        return Ok(((serial_type - 12) >> 1) as usize);
    }
    match serial_type {
        0 | 8 | 9 => Ok(0),
        1 => Ok(1),
        2 => Ok(2),
        3 => Ok(3),
        4 => Ok(4),
        5 => Ok(6),
        6 | 7 => Ok(8),
        _ => Err(SqliteError::corrupt(format!(
            "reserved serial type {serial_type} encountered in record"
        ))),
    }
}

/// Serializes a slice of [`Value`]s into an SQLite Format-3 record using UTF-8 (`OP_MakeRecord`).
pub fn encode_record(values: &[Value]) -> Vec<u8> {
    encode_record_with_encoding(values, TextEncoding::Utf8)
}

/// Serializes a slice of [`Value`]s into an SQLite Format-3 record using `enc` (`OP_MakeRecord`),
/// reusing `out`'s existing allocation.
pub fn encode_record_into(values: &[Value], enc: TextEncoding, out: &mut Vec<u8>) {
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

    // header_size includes its own varint length!
    // Check if adding varint_len(header_size) crosses a varint boundary (e.g. 127 -> 128).
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

/// Serializes a slice of [`Value`] references into an SQLite Format-3 record using `enc`
/// (`OP_MakeRecord`), reusing `out`'s existing allocation without cloning [`Value`]s.
pub fn encode_value_refs_record_into(values: &[&Value], enc: TextEncoding, out: &mut Vec<u8>) {
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

/// Serializes a slice of [`Value`]s into an SQLite Format-3 record using `enc` (`OP_MakeRecord`).
pub fn encode_record_with_encoding(values: &[Value], enc: TextEncoding) -> Vec<u8> {
    let mut out = Vec::new();
    encode_record_into(values, enc, &mut out);
    out
}

/// Serializes a slice of [`RuntimeValue`] registers directly into `out`, reusing `out`'s capacity
/// without cloning intermediate [`Value`] elements (`OP_MakeRecord`).
pub fn encode_runtime_record_into(values: &[RuntimeValue], enc: TextEncoding, out: &mut Vec<u8>) {
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

/// Byte range of one field body inside a stored record, with its serial type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawFieldSpan {
    pub serial_type: u64,
    pub start: usize,
    pub end: usize,
}

/// `true` when decoding the stored field `(serial_type, body)` to a [`Value`] and encoding that
/// value again yields the same serial type and bytes: NULL, minimally encoded integers,
/// non-NaN reals, blobs, and text that is valid UTF-8 on a UTF-8 database.
#[inline]
pub fn raw_field_is_roundtrip_stable(serial_type: u64, body: &[u8], enc: TextEncoding) -> bool {
    match serial_type {
        0 | 8 | 9 => true,
        1..=6 => match int_serial_len(serial_type as u8) {
            Some(len) if len == body.len() => {
                let v = read_int_serial(serial_type as u8, body);
                serial_type_for_with_encoding(&Value::Integer(v), enc).0 == serial_type
            }
            _ => false,
        },
        7 => {
            <[u8; 8]>::try_from(body).is_ok_and(|b| !f64::from_bits(u64::from_be_bytes(b)).is_nan())
        }
        10 | 11 => false,
        n if (n & 1) == 0 => true,
        _ => enc == TextEncoding::Utf8 && std::str::from_utf8(body).is_ok(),
    }
}

/// Locates the first `spans.len()` fields of the stored `record` with the same header checks
/// as a column read. Returns `false` when the record does not hold that many well-formed
/// fields, or when a field selected by `stable_mask` (bit `i` = field `i`) is not
/// [`raw_field_is_roundtrip_stable`].
pub fn locate_roundtrip_stable_fields(
    record: &[u8],
    enc: TextEncoding,
    stable_mask: u32,
    spans: &mut [RawFieldSpan],
) -> bool {
    if record.is_empty() {
        return false;
    }
    let Ok((hdr_u64, mut hdr_pos)) = get_varint(record) else {
        return false;
    };
    let Ok(hdr_size) = usize::try_from(hdr_u64) else {
        return false;
    };
    if hdr_size > record.len() || hdr_pos > hdr_size {
        return false;
    }
    let mut body_pos = hdr_size;
    for (i, span) in spans.iter_mut().enumerate() {
        if hdr_pos >= hdr_size {
            return false;
        }
        let Ok((st, used)) = get_varint(&record[hdr_pos..hdr_size]) else {
            return false;
        };
        let Ok(len) = serial_type_len(st) else {
            return false;
        };
        let Some(end) = body_pos.checked_add(len) else {
            return false;
        };
        if end > record.len() {
            return false;
        }
        if i < 32
            && (stable_mask >> i) & 1 == 1
            && !raw_field_is_roundtrip_stable(st, &record[body_pos..end], enc)
        {
            return false;
        }
        *span = RawFieldSpan {
            serial_type: st,
            start: body_pos,
            end,
        };
        hdr_pos += used;
        body_pos = end;
    }
    true
}

/// One field of a record assembled by [`encode_record_parts_into`].
#[derive(Debug, Clone, Copy)]
pub enum RecordPart<'a> {
    /// A field copied verbatim from a stored record (see [`raw_field_is_roundtrip_stable`]).
    Raw { serial_type: u64, body: &'a [u8] },
    /// A value encoded exactly as [`encode_record_into`] encodes it.
    Value(&'a Value),
}

/// Serializes `parts` into an SQLite Format-3 record. With every `Raw` part round-trip stable,
/// the result equals [`encode_record_into`] over the decoded values.
pub fn encode_record_parts_into(parts: &[RecordPart<'_>], enc: TextEncoding, out: &mut Vec<u8>) {
    out.clear();
    let mut serials_inline = [0u64; 32];
    let mut serials_heap: Vec<u64> = Vec::new();
    let serials: &mut [u64] = if parts.len() <= serials_inline.len() {
        &mut serials_inline[..parts.len()]
    } else {
        serials_heap.resize(parts.len(), 0);
        &mut serials_heap
    };
    let mut serial_bytes_len = 0usize;
    let mut body_len = 0usize;
    for (slot, part) in serials.iter_mut().zip(parts) {
        let (st, len) = match part {
            RecordPart::Raw { serial_type, body } => (*serial_type, body.len()),
            RecordPart::Value(v) => serial_type_for_with_encoding(v, enc),
        };
        serial_bytes_len += varint_len(st);
        body_len += len;
        *slot = st;
    }
    let mut hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 1) as u64);
    if varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
        hdr_len = serial_bytes_len + varint_len((serial_bytes_len + 2) as u64);
    }
    out.reserve(hdr_len + body_len);
    push_varint(out, hdr_len as u64);
    for &st in serials.iter() {
        push_varint(out, st);
    }
    debug_assert_eq!(out.len(), hdr_len);
    for (part, &st) in parts.iter().zip(serials.iter()) {
        match part {
            RecordPart::Raw { body, .. } => out.extend_from_slice(body),
            RecordPart::Value(v) => write_serial_value(out, v, st, enc),
        }
    }
}

/// Serializes a slice of [`RuntimeValue`] registers directly into an SQLite Format-3 record
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

/// Decodes all columns from a serialized UTF-8 SQLite record.
pub fn decode_record(record: &[u8]) -> Result<Vec<Value>> {
    decode_record_with_encoding(record, TextEncoding::Utf8)
}

/// Decodes all columns from a serialized SQLite record using `enc`.
pub fn decode_record_with_encoding(record: &[u8], enc: TextEncoding) -> Result<Vec<Value>> {
    if record.is_empty() {
        return Ok(Vec::new());
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
/// If the record has fewer columns than `col_idx + 1` (e.g. after `ALTER TABLE ADD COLUMN`),
/// returns `Ok(None)` so the caller can substitute the column's `DEFAULT` value (`NULL` if none).
pub fn decode_record_column_with_encoding(
    record: &[u8],
    col_idx: usize,
    enc: TextEncoding,
) -> Result<Option<Value>> {
    if record.is_empty() {
        return Ok(None);
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
                    Value::Blob(rb) => buf.cmp(rb.as_slice()),
                    _ => Ordering::Greater,
                };
                if spec.desc { raw.reverse() } else { raw }
            } else {
                let raw = match b {
                    Value::Integer(_) | Value::Real(_) => Ordering::Greater,
                    Value::Blob(_) => Ordering::Less,
                    Value::Text(rs) => {
                        if enc == TextEncoding::Utf8
                            && let Ok(lhs_s) = std::str::from_utf8(buf)
                        {
                            if let Some(ref cc) = spec.custom_collation {
                                (cc.comparator)(lhs_s, rs.as_str())?
                            } else {
                                spec.collation
                                    .compare_str_with_encoding(lhs_s, rs.as_str(), enc)
                            }
                        } else {
                            let lhs_s = enc.decode_bytes(buf);
                            if let Some(ref cc) = spec.custom_collation {
                                (cc.comparator)(&lhs_s, rs.as_str())?
                            } else {
                                spec.collation
                                    .compare_str_with_encoding(&lhs_s, rs.as_str(), enc)
                            }
                        }
                    }
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

/// Borrowed field view decoded directly from a raw SQLite Format-3 record slice without
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
                if enc == TextEncoding::Utf8
                    && let Ok(lhs_s) = std::str::from_utf8(buf)
                {
                    if let Some(ref cc) = spec.custom_collation {
                        (cc.comparator)(lhs_s, rs.as_str())
                    } else {
                        Ok(spec
                            .collation
                            .compare_str_with_encoding(lhs_s, rs.as_str(), enc))
                    }
                } else {
                    let lhs_s = enc.decode_bytes(buf);
                    if let Some(ref cc) = spec.custom_collation {
                        (cc.comparator)(&lhs_s, rs.as_str())
                    } else {
                        Ok(spec
                            .collation
                            .compare_str_with_encoding(&lhs_s, rs.as_str(), enc))
                    }
                }
            }
            (Self::Blob(buf), Value::Blob(rb)) => Ok(buf.cmp(rb.as_slice())),
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
#[inline]
pub fn read_raw_record_field(raw: &[u8], col_idx: usize) -> Result<Option<RawField<'_>>> {
    if raw.is_empty() {
        return Ok(None);
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

/// `true` when TEXT under `spec` on an `enc` database orders by plain byte comparison of
/// valid UTF-8 (built-in `BINARY`, no custom collation, UTF-8 storage).
///
/// Under that ordering, text that is not valid UTF-8 is compared after lossy decoding. When one
/// operand is pure ASCII the plain byte comparison still gives the same answer whatever the
/// other operand holds: up to the first differing byte both are the same ASCII text; at that
/// byte the ASCII side is `< 0x80`, while the other side is either a byte of a valid sequence
/// (compared as is) or the start of an invalid one, which lossy decoding replaces by U+FFFD
/// (lead byte `0xEF`) — `>= 0x80` both ways; and a proper prefix stays a proper prefix.
#[inline(always)]
fn raw_text_is_plain_binary(spec: &KeySortOrder, enc: TextEncoding) -> bool {
    enc == TextEncoding::Utf8
        && spec.custom_collation.is_none()
        && spec.collation == Collation::Binary
}

/// Lexicographic byte comparison; short keys are compared inline instead of through `memcmp`.
#[inline(always)]
fn cmp_bytes(a: &[u8], b: &[u8]) -> Ordering {
    let n = a.len().min(b.len());
    if n > 24 {
        return a.cmp(b);
    }
    for i in 0..n {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    a.len().cmp(&b.len())
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
            (RawField::Blob(a), RawField::Blob(b)) => a.cmp(b),
            (RawField::Text(a), RawField::Text(b)) => {
                if raw_text_is_plain_binary(spec, enc) && (b.is_ascii() || a.is_ascii()) {
                    cmp_bytes(a, b)
                } else if enc == TextEncoding::Utf8
                    && let (Ok(sa), Ok(sb)) = (std::str::from_utf8(a), std::str::from_utf8(b))
                {
                    if let Some(ref cc) = spec.custom_collation {
                        (cc.comparator)(sa, sb)?
                    } else {
                        spec.collation.compare_str_with_encoding(sa, sb, enc)
                    }
                } else {
                    let sa = enc.decode_bytes(a);
                    let sb = enc.decode_bytes(b);
                    if let Some(ref cc) = spec.custom_collation {
                        (cc.comparator)(&sa, &sb)?
                    } else {
                        spec.collation.compare_str_with_encoding(&sa, &sb, enc)
                    }
                }
            }
            _ => unreachable!(),
        }
    };
    Ok(if spec.desc { raw.reverse() } else { raw })
}

/// Body length of an integer serial type given as its (single) header byte, `None` for every
/// other serial type (NULL, REAL, reserved, TEXT/BLOB, or the first byte of a multi-byte varint).
#[inline(always)]
const fn int_serial_len(st: u8) -> Option<usize> {
    match st {
        1 => Some(1),
        2 => Some(2),
        3 => Some(3),
        4 => Some(4),
        5 => Some(6),
        6 => Some(8),
        8 | 9 => Some(0),
        _ => None,
    }
}

/// Decodes the body of an integer serial type accepted by [`int_serial_len`]; `buf` has exactly
/// that length. Mirrors the integer arms of [`RawField::from_serial`].
#[inline(always)]
fn read_int_serial(st: u8, buf: &[u8]) -> i64 {
    match st {
        1 => i64::from(buf[0] as i8),
        2 => i64::from(i16::from_be_bytes([buf[0], buf[1]])),
        3 => {
            let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
            i64::from(i32::from_be_bytes([sign, buf[0], buf[1], buf[2]]))
        }
        4 => i64::from(i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]])),
        5 => {
            let sign = if (buf[0] & 0x80) != 0 { 0xff } else { 0x00 };
            i64::from_be_bytes([sign, sign, buf[0], buf[1], buf[2], buf[3], buf[4], buf[5]])
        }
        6 => i64::from_be_bytes([
            buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
        ]),
        9 => 1,
        _ => 0,
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
        // Fast path for the dominant key shape: both fields are integers (serial types 1..=6,
        // 8, 9 — always a one-byte header varint). Same bounds checks and ordering as the general
        // path below, minus the `RawField` round trip.
        let st_a0 = lhs_record[lhs_hdr_pos];
        let st_b0 = rhs_record[rhs_hdr_pos];
        if let (Some(len_a), Some(len_b)) = (int_serial_len(st_a0), int_serial_len(st_b0)) {
            let end_a = lhs_body_pos + len_a;
            let end_b = rhs_body_pos + len_b;
            if end_a > lhs_record.len() || end_b > rhs_record.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            let ia = read_int_serial(st_a0, &lhs_record[lhs_body_pos..end_a]);
            let ib = read_int_serial(st_b0, &rhs_record[rhs_body_pos..end_b]);
            if ia != ib {
                let ord = ia.cmp(&ib);
                let desc = orders.get(col_idx).is_some_and(|spec| spec.desc);
                return Ok(if desc { ord.reverse() } else { ord });
            }
            lhs_hdr_pos += 1;
            rhs_hdr_pos += 1;
            lhs_body_pos = end_a;
            rhs_body_pos = end_b;
            col_idx += 1;
            continue;
        }

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

/// Number of leading key fields a [`RawRecordKey`] decodes ahead of time.
const RAW_KEY_MAX_FIELDS: usize = 8;

/// One pre-decoded field of a [`RawRecordKey`].
#[derive(Clone, Copy)]
enum RawKeyField<'a> {
    /// Any integer serial type (1..=6, 8, 9).
    Int(i64),
    /// TEXT consisting only of ASCII bytes (hence valid UTF-8).
    AsciiText(&'a [u8]),
    /// Everything else, compared through [`compare_raw_fields`].
    Other(RawField<'a>),
}

/// A serialized search key whose leading fields are decoded once, so that probing it against
/// many stored records (a B-tree descent) only walks the header of each stored record.
///
/// [`RawRecordKey::compare_stored`] returns exactly what
/// [`compare_raw_records_with_encoding`]`(stored, key, max_fields, orders, enc)` returns, errors
/// included. A key that is not a cleanly decodable record (or has more compared fields than
/// are decoded inline) simply delegates to that function.
pub struct RawRecordKey<'a> {
    record: &'a [u8],
    max_fields: Option<usize>,
    decoded: bool,
    n_fields: usize,
    fields: [RawKeyField<'a>; RAW_KEY_MAX_FIELDS],
}

impl<'a> RawRecordKey<'a> {
    /// Prepares `record` for comparisons limited to its first `max_fields` fields
    /// (`None` = all fields).
    pub fn new(record: &'a [u8], max_fields: Option<usize>) -> Self {
        let mut key = Self {
            record,
            max_fields,
            decoded: false,
            n_fields: 0,
            fields: [RawKeyField::Int(0); RAW_KEY_MAX_FIELDS],
        };
        key.decoded = key.decode().is_some();
        key
    }

    /// Prepares an index key record the way the B-tree index probes limit comparisons:
    /// to `orders.len()` fields, or to every field when `orders` is empty.
    #[inline]
    pub fn for_index_orders(record: &'a [u8], orders: &[KeySortOrder]) -> Self {
        Self::new(
            record,
            if orders.is_empty() {
                None
            } else {
                Some(orders.len())
            },
        )
    }

    /// The serialized key record this was built from.
    #[inline(always)]
    pub const fn record(&self) -> &'a [u8] {
        self.record
    }

    fn decode(&mut self) -> Option<()> {
        let record = self.record;
        if record.is_empty() {
            return None;
        }
        let (hdr_u64, mut hdr_pos) = get_varint(record).ok()?;
        let hdr_size = usize::try_from(hdr_u64).ok()?;
        if hdr_size > record.len() || hdr_pos > hdr_size {
            return None;
        }
        let limit = self.max_fields.unwrap_or(usize::MAX);
        let mut body_pos = hdr_size;
        let mut n = 0usize;
        while n < limit && hdr_pos < hdr_size {
            if n == RAW_KEY_MAX_FIELDS {
                return None;
            }
            if let Some(len) = int_serial_len(record[hdr_pos]) {
                let end = body_pos + len;
                if end > record.len() {
                    return None;
                }
                self.fields[n] =
                    RawKeyField::Int(read_int_serial(record[hdr_pos], &record[body_pos..end]));
                hdr_pos += 1;
                body_pos = end;
                n += 1;
                continue;
            }
            let (st, used) = get_varint(&record[hdr_pos..hdr_size]).ok()?;
            let len = serial_type_len(st).ok()?;
            let end = body_pos.checked_add(len)?;
            if end > record.len() {
                return None;
            }
            self.fields[n] = match RawField::from_serial(st, &record[body_pos..end]).ok()? {
                RawField::Int(i) => RawKeyField::Int(i),
                RawField::Text(t) if t.is_ascii() => RawKeyField::AsciiText(t),
                other => RawKeyField::Other(other),
            };
            hdr_pos += used;
            body_pos = end;
            n += 1;
        }
        self.n_fields = n;
        Some(())
    }

    /// Compares the stored record `stored` (left-hand side) against this key; see the type
    /// documentation for the exact contract.
    #[inline]
    pub fn compare_stored(
        &self,
        stored: &[u8],
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<Ordering> {
        if !self.decoded {
            return compare_raw_records_with_encoding(
                stored,
                self.record,
                self.max_fields,
                orders,
                enc,
            );
        }
        if stored.is_empty() {
            return Ok(Ordering::Equal);
        }
        let (hdr_u64, mut hdr_pos) = get_varint(stored)?;
        let hdr_size = usize::try_from(hdr_u64)
            .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
        if hdr_size > stored.len() || hdr_pos > hdr_size {
            return Err(SqliteError::corrupt("record header exceeds record length"));
        }
        let mut body_pos = hdr_size;
        for (col_idx, key_field) in self.fields[..self.n_fields].iter().enumerate() {
            if hdr_pos >= hdr_size {
                break;
            }
            if let RawKeyField::Int(key_int) = *key_field
                && let Some(len) = int_serial_len(stored[hdr_pos])
            {
                let end = body_pos + len;
                if end > stored.len() {
                    return Err(SqliteError::corrupt("record payload truncated"));
                }
                let stored_int = read_int_serial(stored[hdr_pos], &stored[body_pos..end]);
                if stored_int != key_int {
                    let ord = stored_int.cmp(&key_int);
                    let desc = orders.get(col_idx).is_some_and(|spec| spec.desc);
                    return Ok(if desc { ord.reverse() } else { ord });
                }
                hdr_pos += 1;
                body_pos = end;
                continue;
            }

            let (st, used) = get_varint(&stored[hdr_pos..hdr_size])?;
            hdr_pos += used;
            let len = serial_type_len(st)?;
            let end = body_pos
                .checked_add(len)
                .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
            if end > stored.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            let stored_field = RawField::from_serial(st, &stored[body_pos..end])?;
            let default_spec;
            let spec = match orders.get(col_idx) {
                Some(spec) => spec,
                None => {
                    default_spec = KeySortOrder::default();
                    &default_spec
                }
            };
            let cmp = match (stored_field, *key_field) {
                (RawField::Text(a), RawKeyField::AsciiText(b))
                    if raw_text_is_plain_binary(spec, enc) =>
                {
                    let raw = cmp_bytes(a, b);
                    if spec.desc { raw.reverse() } else { raw }
                }
                (_, RawKeyField::Int(i)) => {
                    compare_raw_fields(stored_field, RawField::Int(i), spec, enc)?
                }
                (_, RawKeyField::AsciiText(b)) => {
                    compare_raw_fields(stored_field, RawField::Text(b), spec, enc)?
                }
                (_, RawKeyField::Other(f)) => compare_raw_fields(stored_field, f, spec, enc)?,
            };
            if cmp != Ordering::Equal {
                return Ok(cmp);
            }
            body_pos = end;
        }
        Ok(Ordering::Equal)
    }
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

    /// The integer fast path of the raw comparator must agree with the decoded-value comparator
    /// for every integer width, mixed classes, DESC columns, field limits and truncated bodies.
    #[test]
    fn raw_record_compare_int_fast_path_matches_value_compare() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn pick(state: &mut u64) -> Value {
            let r = next(state);
            let magnitudes: [i64; 12] = [
                0,
                1,
                2,
                127,
                128,
                32_767,
                32_768,
                8_388_607,
                8_388_608,
                2_147_483_648,
                140_737_488_355_328,
                i64::MAX,
            ];
            let m = magnitudes[(r % 12) as usize];
            match (r >> 8) % 10 {
                0 => Value::Null,
                1 => Value::Real((m as f64) + 0.5),
                2 => Value::Text(format!("t{}", m % 7)),
                3 => Value::Blob(vec![(m % 5) as u8; (m % 3) as usize]),
                4 | 5 => Value::Integer(-m - ((r >> 20) % 2) as i64),
                _ => Value::Integer(m - ((r >> 20) % 3) as i64),
            }
        }

        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let asc = KeySortOrder::default();
        let desc = KeySortOrder {
            desc: true,
            ..KeySortOrder::default()
        };
        for round in 0..20_000 {
            let n_a = (next(&mut seed) % 4) as usize + 1;
            let n_b = (next(&mut seed) % 4) as usize + 1;
            let a: Vec<Value> = (0..n_a).map(|_| pick(&mut seed)).collect();
            let mut b: Vec<Value> = (0..n_b).map(|_| pick(&mut seed)).collect();
            // Make long equal prefixes common so later fields decide.
            for i in 0..n_a.min(n_b) {
                if !next(&mut seed).is_multiple_of(3) {
                    b[i] = a[i].clone();
                }
            }
            let orders: Vec<KeySortOrder> = (0..4)
                .map(|i| {
                    if (round >> i) & 1 == 1 {
                        desc.clone()
                    } else {
                        asc.clone()
                    }
                })
                .collect();
            let orders = &orders[..(round % 5).min(4)];
            let ra = encode_record(&a);
            let rb = encode_record(&b);
            for max_fields in [None, Some(1), Some(2), Some(4)] {
                let lim = max_fields.unwrap_or(usize::MAX).min(n_b);
                let expect =
                    compare_records_with_encoding(&ra, &b[..lim], orders, TextEncoding::Utf8)
                        .unwrap();
                let got = compare_raw_records_with_encoding(
                    &ra,
                    &rb,
                    max_fields,
                    orders,
                    TextEncoding::Utf8,
                )
                .unwrap();
                assert_eq!(got, expect, "{a:?} vs {b:?} max_fields={max_fields:?}");
            }
        }

        // A truncated integer body is reported as corruption on either side.
        let full = encode_record(&[Value::Integer(5), Value::Integer(70_000)]);
        let cut = &full[..full.len() - 1];
        for (l, r) in [(cut, full.as_slice()), (full.as_slice(), cut)] {
            let err =
                compare_raw_records_with_encoding(l, r, None, &[], TextEncoding::Utf8).unwrap_err();
            assert_eq!(err.code, crate::ErrorCode::Corrupt);
        }
    }

    /// `RawRecordKey::compare_stored` must agree with `compare_raw_records_with_encoding` on
    /// every input (result and error code), and both must agree with the decoded-`Value`
    /// comparison whenever the key decodes — including non-ASCII and invalid UTF-8 text, every
    /// built-in collation, DESC / NULLS LAST, keys wider than the inline decode window, and
    /// truncated or damaged records on either side.
    #[test]
    fn raw_record_key_matches_raw_record_compare() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn raw_record(fields: &[(u64, Vec<u8>)]) -> Vec<u8> {
            let mut hdr = Vec::new();
            for (st, _) in fields {
                push_varint(&mut hdr, *st);
            }
            let mut out = Vec::new();
            push_varint(&mut out, (hdr.len() + 1) as u64);
            out.extend_from_slice(&hdr);
            for (_, body) in fields {
                out.extend_from_slice(body);
            }
            out
        }
        fn pick(state: &mut u64) -> (u64, Vec<u8>) {
            let r = next(state);
            let texts: [&[u8]; 12] = [
                b"",
                b"a",
                b"A",
                b"a ",
                b"ab",
                b"C-0042",
                b"C-0043",
                "\u{e9}t\u{e9}".as_bytes(),
                "\u{ffff}".as_bytes(),
                b"\xff",
                b"a\xc3",
                b"\xef\xbf\xbd",
            ];
            match (r >> 8) % 12 {
                0 => (0, Vec::new()),
                1 => (8, Vec::new()),
                2 => (9, Vec::new()),
                3 => (1, vec![(r >> 16) as u8 % 4]),
                4 => (2, vec![(r >> 16) as u8 % 3, (r >> 24) as u8]),
                5 => (6, ((r >> 16) as i64 - 30_000).to_be_bytes().to_vec()),
                6 => (7, (((r >> 16) % 5) as f64 - 1.5).to_be_bytes().to_vec()),
                7 => {
                    let b = vec![(r >> 16) as u8 % 3; ((r >> 20) % 3) as usize];
                    (12 + 2 * b.len() as u64, b)
                }
                _ => {
                    let t = texts[((r >> 16) % 12) as usize];
                    (13 + 2 * t.len() as u64, t.to_vec())
                }
            }
        }
        fn same(a: &Result<Ordering>, b: &Result<Ordering>) -> bool {
            match (a, b) {
                (Ok(x), Ok(y)) => x == y,
                (Err(x), Err(y)) => x.code == y.code,
                _ => false,
            }
        }

        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let collations = [Collation::Binary, Collation::NoCase, Collation::RTrim];
        for round in 0..40_000u64 {
            let n_a = (next(&mut seed) % 5) as usize + 1;
            let n_b = if round % 97 == 0 {
                10
            } else {
                (next(&mut seed) % 5) as usize + 1
            };
            let a: Vec<(u64, Vec<u8>)> = (0..n_a).map(|_| pick(&mut seed)).collect();
            let mut b: Vec<(u64, Vec<u8>)> = (0..n_b).map(|_| pick(&mut seed)).collect();
            for i in 0..n_a.min(n_b) {
                if !next(&mut seed).is_multiple_of(3) {
                    b[i] = a[i].clone();
                }
            }
            let orders: Vec<KeySortOrder> = (0..5)
                .map(|_| {
                    let r = next(&mut seed);
                    KeySortOrder {
                        desc: r & 1 == 1,
                        nulls_last: r & 2 == 2,
                        collation: collations[((r >> 2) % 3) as usize],
                        custom_collation: None,
                    }
                })
                .collect();
            let orders = &orders[..(round % 6) as usize];
            let mut ra = raw_record(&a);
            let mut rb = raw_record(&b);
            // Occasionally damage one side: truncation or a clobbered byte.
            match next(&mut seed) % 40 {
                0 => ra.truncate((next(&mut seed) as usize) % (ra.len() + 1)),
                1 => rb.truncate((next(&mut seed) as usize) % (rb.len() + 1)),
                2 => {
                    let i = (next(&mut seed) as usize) % ra.len();
                    ra[i] = (next(&mut seed) % 14) as u8;
                }
                3 => {
                    let i = (next(&mut seed) as usize) % rb.len();
                    rb[i] = (next(&mut seed) % 14) as u8;
                }
                _ => {}
            }
            for enc in [TextEncoding::Utf8, TextEncoding::Utf16Le] {
                for max_fields in [None, Some(1), Some(2), Some(3), Some(9)] {
                    let reference =
                        compare_raw_records_with_encoding(&ra, &rb, max_fields, orders, enc);
                    let key = RawRecordKey::new(&rb, max_fields);
                    let got = key.compare_stored(&ra, orders, enc);
                    assert!(
                        same(&got, &reference),
                        "key compare {got:?} != raw compare {reference:?} for {ra:?} vs {rb:?} \
                         max_fields={max_fields:?} orders={orders:?} enc={enc:?}"
                    );
                    // Independent oracle: the decoded-value comparison.
                    if let (Ok(ord), Ok(vals_b)) =
                        (&reference, decode_record_with_encoding(&rb, enc))
                        && !ra.is_empty()
                    {
                        let lim = max_fields.unwrap_or(usize::MAX).min(vals_b.len());
                        if let Ok(expect) =
                            compare_records_with_encoding(&ra, &vals_b[..lim], orders, enc)
                        {
                            assert_eq!(
                                *ord, expect,
                                "raw compare != value compare for {ra:?} vs {rb:?} \
                                 max_fields={max_fields:?} orders={orders:?} enc={enc:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// `raw_field_is_roundtrip_stable` must hold exactly when `decode -> encode` reproduces the
    /// stored field, and `encode_record_parts_into` must equal `encode_record_into` over the
    /// decoded values whenever its raw parts are stable.
    #[test]
    fn record_parts_and_roundtrip_stability_match_value_codec() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn field(state: &mut u64) -> (u64, Vec<u8>) {
            let r = next(state);
            let small = ((r >> 20) % 5) as i64 - 2;
            match (r >> 8) % 14 {
                0 => (0, Vec::new()),
                1 => (8, Vec::new()),
                2 => (9, Vec::new()),
                // Integers in every width, minimal or not.
                3 => (1, vec![small as u8]),
                4 => (2, ((small * 100) as i16).to_be_bytes().to_vec()),
                5 => (3, ((small * 40_000) as i32).to_be_bytes()[1..].to_vec()),
                6 => (4, ((small * 9_000_000) as i32).to_be_bytes().to_vec()),
                7 => (5, (small * 3_000_000_000).to_be_bytes()[2..].to_vec()),
                8 => (6, (small * 150_000_000_000_000).to_be_bytes().to_vec()),
                9 => {
                    let f = [1.5f64, -0.0, f64::NAN, f64::INFINITY, 3.0][((r >> 24) % 5) as usize];
                    (7, f.to_bits().to_be_bytes().to_vec())
                }
                10 => {
                    let b = vec![(r >> 16) as u8; ((r >> 24) % 4) as usize];
                    (12 + 2 * b.len() as u64, b)
                }
                _ => {
                    let texts: [&[u8]; 6] = [
                        b"",
                        b"C-0042",
                        "h\u{e9}".as_bytes(),
                        b"\xff",
                        b"a\xc3",
                        b"ok",
                    ];
                    let t = texts[((r >> 16) % 6) as usize];
                    (13 + 2 * t.len() as u64, t.to_vec())
                }
            }
        }

        let mut seed = 0xA076_1D64_78BD_642Fu64;
        for _ in 0..30_000 {
            let n = (next(&mut seed) % 6) as usize + 1;
            let fields: Vec<(u64, Vec<u8>)> = (0..n).map(|_| field(&mut seed)).collect();
            let mut hdr = Vec::new();
            for (st, _) in &fields {
                push_varint(&mut hdr, *st);
            }
            let mut stored = Vec::new();
            push_varint(&mut stored, (hdr.len() + 1) as u64);
            stored.extend_from_slice(&hdr);
            for (_, body) in &fields {
                stored.extend_from_slice(body);
            }
            for enc in [TextEncoding::Utf8, TextEncoding::Utf16Le] {
                let decoded = decode_record_with_encoding(&stored, enc).unwrap();
                let mut reencoded = Vec::new();
                encode_record_into(&decoded, enc, &mut reencoded);

                // Per field: stable <=> the single-field record survives decode -> encode.
                let mut all_stable = true;
                for ((st, body), value) in fields.iter().zip(&decoded) {
                    let mut one = Vec::new();
                    encode_record_into(std::slice::from_ref(value), enc, &mut one);
                    let mut raw_one = Vec::new();
                    push_varint(&mut raw_one, 1 + varint_len(*st) as u64);
                    push_varint(&mut raw_one, *st);
                    raw_one.extend_from_slice(body);
                    let stable = raw_field_is_roundtrip_stable(*st, body, enc);
                    if enc == TextEncoding::Utf8 || *st < 13 || (*st & 1) == 0 {
                        assert_eq!(stable, one == raw_one, "field {st} {body:?} {enc:?}");
                    } else {
                        // UTF-16 text is never claimed stable (a conservative answer).
                        assert!(!stable);
                    }
                    all_stable &= stable;
                }

                let mut spans = vec![RawFieldSpan::default(); n];
                let located = locate_roundtrip_stable_fields(&stored, enc, u32::MAX, &mut spans);
                assert_eq!(located, all_stable);
                assert!(locate_roundtrip_stable_fields(&stored, enc, 0, &mut spans));
                // One field too many, or a truncated record, is refused.
                let mut too_many = vec![RawFieldSpan::default(); n + 1];
                assert!(!locate_roundtrip_stable_fields(
                    &stored,
                    enc,
                    0,
                    &mut too_many
                ));
                if stored.len() > hdr.len() + 1 {
                    let cut = &stored[..stored.len() - 1];
                    assert!(!locate_roundtrip_stable_fields(
                        cut,
                        enc,
                        0,
                        &mut spans.clone()
                    ));
                }

                if all_stable {
                    // Mix raw parts with values: must equal the plain value encoding.
                    let pick = next(&mut seed);
                    let parts: Vec<RecordPart<'_>> = spans
                        .iter()
                        .zip(&decoded)
                        .enumerate()
                        .map(|(i, (span, value))| {
                            if (pick >> i) & 1 == 0 {
                                RecordPart::Raw {
                                    serial_type: span.serial_type,
                                    body: &stored[span.start..span.end],
                                }
                            } else {
                                RecordPart::Value(value)
                            }
                        })
                        .collect();
                    let mut assembled = vec![0xAA; 3];
                    encode_record_parts_into(&parts, enc, &mut assembled);
                    assert_eq!(assembled, reencoded);
                    assert_eq!(assembled, stored);
                }
            }
        }
    }

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
}
