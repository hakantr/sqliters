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
use crate::value::{Collation, CustomCollation, TextEncoding, Value};
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
#[inline]
pub fn serial_type_len(serial_type: u64) -> Result<usize> {
    match serial_type {
        0 | 8 | 9 => Ok(0),
        1 => Ok(1),
        2 => Ok(2),
        3 => Ok(3),
        4 => Ok(4),
        5 => Ok(6),
        6 | 7 => Ok(8),
        10 | 11 => Err(SqliteError::corrupt(format!(
            "reserved serial type {serial_type} encountered in record"
        ))),
        n => Ok(((n - 12) / 2) as usize),
    }
}

/// Serializes a slice of [`Value`]s into an SQLite Format-3 record using UTF-8 (`OP_MakeRecord`).
pub fn encode_record(values: &[Value]) -> Vec<u8> {
    encode_record_with_encoding(values, TextEncoding::Utf8)
}

/// Serializes a slice of [`Value`]s into an SQLite Format-3 record using `enc` (`OP_MakeRecord`).
pub fn encode_record_with_encoding(values: &[Value], enc: TextEncoding) -> Vec<u8> {
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

    let mut out = Vec::with_capacity(hdr_len + body_len);
    push_varint(&mut out, hdr_len as u64);
    for st in &serials {
        push_varint(&mut out, *st);
    }
    debug_assert_eq!(out.len(), hdr_len);

    for (v, &st) in values.iter().zip(serials.iter()) {
        write_serial_value(&mut out, v, st, enc);
    }

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
    let lhs_values = decode_record_with_encoding(lhs_record, enc)?;
    let n = lhs_values.len().min(rhs_values.len());
    let default_spec = KeySortOrder::default();
    for i in 0..n {
        let spec = orders.get(i).unwrap_or(&default_spec);
        let a = &lhs_values[i];
        let b = &rhs_values[i];
        let ord = match (a.is_null(), b.is_null()) {
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
        };
        if ord != Ordering::Equal {
            return Ok(ord);
        }
    }
    Ok(Ordering::Equal)
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
