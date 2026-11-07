//! `sqliters-types`: foundational types, constants, error codes, varints,
//! runtime values, collation rules, on-disk record serialization, and bitmaps
//! ported from SQLite 3.54.1.

pub mod bitvec;
pub mod error;
pub mod limits;
pub mod record;
pub mod value;
pub mod varint;

pub use bitvec::Bitvec;
pub use error::{ErrorCode, Result, SqliteError, extended};
pub use limits::*;
pub use record::{
    KeySortOrder, compare_records, decode_record, decode_record_column, decode_serial_value,
    encode_record, serial_type_for, serial_type_len,
};
pub use value::{
    Collation, StorageClass, TypeAffinity, Value, cmp_int_float, format_sqlite_real,
    parse_numeric_text,
};
pub use varint::{
    get_varint, get_varint_i64, push_varint, put_varint, put_varint_i64, varint_len,
};

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;

    #[test]
    fn test_varint_known_sqlite_vectors() {
        // Known test vectors directly from sqlite/src/btreeInt.h lines 178-184:
        let vectors: &[(&[u8], u64)] = &[
            (&[0x00], 0x0000_0000),
            (&[0x7f], 0x0000_007f),
            (&[0x81, 0x00], 0x0000_0080),
            (&[0x82, 0x00], 0x0000_0100),
            (&[0x81, 0x91, 0xd1, 0xac, 0x78], 0x1234_5678),
            (&[0x81, 0x81, 0x81, 0x81, 0x01], 0x1020_4081),
        ];

        for &(expected_bytes, val) in vectors {
            let mut buf = [0u8; 9];
            let n = put_varint(&mut buf, val);
            assert_eq!(&buf[..n], expected_bytes, "encoding {val:#x}");
            assert_eq!(n, varint_len(val));

            let (decoded, read_n) = get_varint(expected_bytes).unwrap();
            assert_eq!(decoded, val);
            assert_eq!(read_n, expected_bytes.len());
        }
    }

    #[test]
    fn test_varint_boundaries_and_negative_i64() {
        let interesting = [
            0u64,
            1,
            127,
            128,
            16_383,
            16_384,
            (1 << 21) - 1,
            1 << 21,
            (1 << 28) - 1,
            1 << 28,
            (1 << 35) - 1,
            1 << 35,
            (1 << 42) - 1,
            1 << 42,
            (1 << 49) - 1,
            1 << 49,
            (1 << 56) - 1,
            1 << 56,
            u64::MAX - 1,
            u64::MAX,
            (-1i64) as u64,
            i64::MIN as u64,
            i64::MAX as u64,
        ];
        for v in interesting {
            let mut buf = [0u8; 9];
            let n = put_varint(&mut buf, v);
            assert_eq!(n, varint_len(v));
            let (dec, used) = get_varint(&buf[..n]).unwrap();
            assert_eq!(dec, v);
            assert_eq!(used, n);
        }
    }

    #[test]
    fn test_record_roundtrip_all_serial_types() {
        let row = vec![
            Value::Null,
            Value::Integer(0),                     // serial type 8
            Value::Integer(1),                     // serial type 9
            Value::Integer(-1),                    // serial type 1 (1-byte)
            Value::Integer(127),                   // serial type 1 (1-byte)
            Value::Integer(-128),                  // serial type 1 (1-byte)
            Value::Integer(128),                   // serial type 2 (2-byte)
            Value::Integer(-32_768),               // serial type 2 (2-byte)
            Value::Integer(100_000),               // serial type 3 (3-byte 24-bit!)
            Value::Integer(-100_000),              // serial type 3 (3-byte negative!)
            Value::Integer(1_000_000_000),         // serial type 4 (4-byte)
            Value::Integer(-1_000_000_000),        // serial type 4 (4-byte)
            Value::Integer(10_000_000_000),        // serial type 5 (6-byte 48-bit!)
            Value::Integer(-10_000_000_000),       // serial type 5 (6-byte negative!)
            Value::Integer(i64::MAX),              // serial type 6 (8-byte)
            Value::Integer(i64::MIN),              // serial type 6 (8-byte)
            Value::real(3.141592653589793),        // serial type 7 (IEEE f64)
            Value::Text(String::new()),            // serial type 13
            Value::Text("Merhaba SQLite!".into()), // odd >= 13
            Value::Blob(vec![]),                   // serial type 12
            Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]),
        ];

        let encoded = encode_record(&row);
        let decoded = decode_record(&encoded).unwrap();
        assert_eq!(decoded, row);

        for (idx, expected) in row.iter().enumerate() {
            let single = decode_record_column(&encoded, idx).unwrap().unwrap();
            assert_eq!(&single, expected);
        }
        assert_eq!(decode_record_column(&encoded, row.len()).unwrap(), None);
    }

    #[test]
    fn test_sqlite_cross_type_comparison_and_affinities() {
        // Storage class hierarchy: NULL < Numeric < Text < Blob
        assert_eq!(
            Value::Null.cmp_sqlite(&Value::Integer(i64::MIN), Collation::Binary),
            Ordering::Less
        );
        assert_eq!(
            Value::Integer(10).cmp_sqlite(&Value::Real(10.0), Collation::Binary),
            Ordering::Equal
        );
        assert_eq!(
            Value::Integer(10).cmp_sqlite(&Value::Real(10.5), Collation::Binary),
            Ordering::Less
        );
        // Large 64-bit integer precision check (2^60 vs 2^60 + 1, which collapse to the same f64!)
        let big = 1_i64 << 60;
        assert_eq!(cmp_int_float(big, big as f64), Ordering::Equal);
        assert_eq!(cmp_int_float(big + 1, big as f64), Ordering::Greater);
        assert_eq!(cmp_int_float(big - 1, big as f64), Ordering::Less);

        assert_eq!(
            Value::Real(1e100).cmp_sqlite(&Value::Text("0".into()), Collation::Binary),
            Ordering::Less
        );
        assert_eq!(
            Value::Text("z".into()).cmp_sqlite(&Value::Blob(vec![]), Collation::Binary),
            Ordering::Less
        );

        // Affinity rules from SQLite §3.1
        assert_eq!(
            TypeAffinity::from_declared_type("BIGINT"),
            TypeAffinity::Integer
        );
        assert_eq!(
            TypeAffinity::from_declared_type("VARCHAR(255)"),
            TypeAffinity::Text
        );
        assert_eq!(TypeAffinity::from_declared_type(""), TypeAffinity::Blob);
        assert_eq!(
            TypeAffinity::from_declared_type("DOUBLE PRECISION"),
            TypeAffinity::Real
        );
        assert_eq!(
            TypeAffinity::from_declared_type("DECIMAL(10,5)"),
            TypeAffinity::Numeric
        );
        // Tricky SQLite rule: "FLOATING POINT" contains "INT" so Rule 1 wins -> INTEGER!
        assert_eq!(
            TypeAffinity::from_declared_type("FLOATING POINT"),
            TypeAffinity::Integer
        );
    }

    #[test]
    fn test_bitvec() {
        let mut bv = Bitvec::new(200);
        assert!(!bv.test(1));
        assert!(bv.set(1).unwrap());
        assert!(!bv.set(1).unwrap());
        assert!(bv.set(64).unwrap());
        assert!(bv.set(65).unwrap());
        assert!(bv.set(200).unwrap());
        assert_eq!(bv.len(), 4);
        assert!(bv.test(64));
        assert!(bv.test(65));
        bv.clear(64);
        assert!(!bv.test(64));
        assert_eq!(bv.len(), 3);
    }
}
