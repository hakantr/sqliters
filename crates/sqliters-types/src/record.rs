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
                if let Some(ord) = sqliters_asm::cmp_utf8_ascii_binary(buf, rs.as_bytes()) {
                    return Ok(ord);
                }
            }
            Collation::NoCase => {
                if let Some(ord) = sqliters_asm::cmp_utf8_ascii_nocase(buf, rs.as_bytes()) {
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
                if let Some(ord) = sqliters_asm::cmp_utf8_ascii_binary(a, b) {
                    return Ok(ord);
                }
            }
            Collation::NoCase => {
                if let Some(ord) = sqliters_asm::cmp_utf8_ascii_nocase(a, b) {
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
}
