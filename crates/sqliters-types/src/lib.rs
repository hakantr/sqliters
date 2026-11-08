//! `sqliters-types`: foundational types, constants, error codes, varints,
//! runtime values, collation rules, on-disk record serialization, and bitmaps
//! ported from SQLite 3.54.1.
#![forbid(unsafe_code)]

pub mod bitvec;
pub mod error;
pub mod limits;
pub mod record;
pub mod value;
pub mod varint;

pub use bitvec::Bitvec;
pub use error::{ConflictResolutionEffect, ErrorCode, Result, SqliteError, extended};
pub use limits::*;
pub use record::{
    KeySortOrder, compare_records, compare_records_with_encoding, decode_record,
    decode_record_column, decode_record_column_with_encoding, decode_record_with_encoding,
    decode_serial_value, decode_serial_value_with_encoding, encode_record,
    encode_record_with_encoding, serial_type_for, serial_type_for_with_encoding, serial_type_len,
};
pub use value::{
    Collation, StorageClass, TextEncoding, TypeAffinity, Value, cmp_int_float, format_sqlite_real,
    parse_numeric_text,
};
pub use varint::{get_varint, get_varint_i64, push_varint, put_varint, put_varint_i64, varint_len};

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
            Value::real(std::f64::consts::PI),     // serial type 7 (IEEE f64)
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

    #[test]
    fn test_extended_error_codes_match_sqlite_h() {
        assert_eq!(extended::SQLITE_ABORT_ROLLBACK, 4 | (2 << 8));
        assert_eq!(extended::SQLITE_BUSY_RECOVERY, 5 | (1 << 8));
        assert_eq!(extended::SQLITE_BUSY_SNAPSHOT, 5 | (2 << 8));
        assert_eq!(extended::SQLITE_BUSY_TIMEOUT, 5 | (3 << 8));
        assert_eq!(extended::SQLITE_READONLY_RECOVERY, 8 | (1 << 8));
        assert_eq!(extended::SQLITE_READONLY_CANTLOCK, 8 | (2 << 8));
        assert_eq!(extended::SQLITE_READONLY_ROLLBACK, 8 | (3 << 8));
        assert_eq!(extended::SQLITE_READONLY_DBMOVED, 8 | (4 << 8));
        assert_eq!(extended::SQLITE_READONLY_CANTINIT, 8 | (5 << 8));
        assert_eq!(extended::SQLITE_READONLY_DIRECTORY, 8 | (6 << 8));

        assert_eq!(extended::SQLITE_IOERR_READ, 10 | (1 << 8));
        assert_eq!(extended::SQLITE_IOERR_SHORT_READ, 10 | (2 << 8));
        assert_eq!(extended::SQLITE_IOERR_WRITE, 10 | (3 << 8));
        assert_eq!(extended::SQLITE_IOERR_FSYNC, 10 | (4 << 8));
        assert_eq!(extended::SQLITE_IOERR_DIR_FSYNC, 10 | (5 << 8));
        assert_eq!(extended::SQLITE_IOERR_TRUNCATE, 10 | (6 << 8));
        assert_eq!(extended::SQLITE_IOERR_FSTAT, 10 | (7 << 8));
        assert_eq!(extended::SQLITE_IOERR_UNLOCK, 10 | (8 << 8));
        assert_eq!(extended::SQLITE_IOERR_RDLOCK, 10 | (9 << 8));
        assert_eq!(extended::SQLITE_IOERR_DELETE, 10 | (10 << 8));
        assert_eq!(extended::SQLITE_IOERR_CHECKRESERVEDLOCK, 10 | (14 << 8));
        assert_eq!(extended::SQLITE_IOERR_LOCK, 10 | (15 << 8));
        assert_eq!(extended::SQLITE_IOERR_CLOSE, 10 | (16 << 8));
        assert_eq!(extended::SQLITE_IOERR_SHMOPEN, 10 | (18 << 8));
        assert_eq!(extended::SQLITE_IOERR_SHMSIZE, 10 | (19 << 8));
        assert_eq!(extended::SQLITE_IOERR_SHMLOCK, 10 | (20 << 8));
        assert_eq!(extended::SQLITE_IOERR_SHMMAP, 10 | (21 << 8));
        assert_eq!(extended::SQLITE_IOERR_IN_PAGE, 10 | (34 << 8));

        let err = SqliteError::with_extended(extended::SQLITE_READONLY_CANTINIT, "cantinit");
        assert_eq!(err.code, ErrorCode::ReadOnly);
        assert_eq!(err.extended_code, extended::SQLITE_READONLY_CANTINIT);
    }

    #[test]
    fn test_lock_geometry_production_and_test_64k() {
        let prod = LockGeometry::default();
        assert_eq!(prod, LockGeometry::PRODUCTION);
        assert_eq!(prod.pending_byte_offset(), 0x4000_0000);
        assert_eq!(prod.reserved_byte_offset(), 0x4000_0001);
        assert_eq!(prod.shared_first_offset(), 0x4000_0002);
        assert_eq!(prod.shared_size(), 510);
        assert_eq!(prod.lock_byte_page(1024), 1_048_577);
        assert_eq!(prod.lock_byte_page(4096), 262_145);
        assert_eq!(prod.lock_byte_page(65_536), 16_385);
        assert_eq!(prod.sj_pgno(4096), 262_145);

        let test_geom = LockGeometry::test_64k();
        assert_eq!(test_geom.pending_byte_offset(), 0x0001_0000);
        assert_eq!(test_geom.reserved_byte_offset(), 0x0001_0001);
        assert_eq!(test_geom.shared_first_offset(), 0x0001_0002);
        assert_eq!(test_geom.shared_size(), 510);
        assert_eq!(test_geom.lock_byte_page(1024), 65);
        assert_eq!(test_geom.lock_byte_page(4096), 17);
        assert_eq!(test_geom.lock_byte_page(65_536), 2);
        assert_eq!(test_geom.sj_pgno(1024), 65);

        assert!(LockGeometry::try_from_pending_byte(0).is_err());
        assert!(LockGeometry::try_from_pending_byte(u64::from(u32::MAX)).is_err());
        assert_eq!(
            LockGeometry::try_from_pending_byte(0x0001_0000).unwrap(),
            test_geom
        );

        assert_ne!(DatabaseKind::NamedDisk, DatabaseKind::PrivateTemporary);
        assert_ne!(DatabaseKind::PrivateTemporary, DatabaseKind::Memory);
    }

    #[test]
    fn test_page_size_codecs_including_64k() {
        let valid_sizes = [512u32, 1024, 2048, 4096, 8192, 16_384, 32_768, 65_536];
        for sz in valid_sizes {
            assert!(is_valid_page_size(sz));

            let db_bytes = encode_db_header_page_size(sz).unwrap();
            if sz == 65_536 {
                assert_eq!(db_bytes, [0x00, 0x01]);
            } else {
                assert_eq!(u16::from_be_bytes(db_bytes), sz as u16);
            }
            assert_eq!(decode_db_header_page_size(db_bytes).unwrap(), sz);

            let wal_bytes = encode_wal_header_page_size(sz).unwrap();
            if sz == 65_536 {
                assert_eq!(wal_bytes, [0x00, 0x01, 0x00, 0x00]);
            }
            assert_eq!(u32::from_be_bytes(wal_bytes), sz);
            assert_eq!(decode_wal_header_page_size(wal_bytes).unwrap(), sz);

            let shm_u16 = encode_shm_page_size(sz).unwrap();
            if sz == 65_536 {
                assert_eq!(shm_u16, 1);
            } else {
                assert_eq!(u32::from(shm_u16), sz);
            }
            assert_eq!(decode_shm_page_size(shm_u16).unwrap(), sz);
        }

        // Invalid encodings must fail closed
        for bad in [0u32, 1, 256, 513, 3000, 65_537, 131_072] {
            assert!(!is_valid_page_size(bad));
            assert!(encode_db_header_page_size(bad).is_err());
            assert!(encode_wal_header_page_size(bad).is_err());
            assert!(encode_shm_page_size(bad).is_err());
        }
        assert!(decode_db_header_page_size([0x00, 0x00]).is_err());
        assert!(decode_db_header_page_size([0x00, 0x02]).is_err());
        // WAL header must NOT accept 1 as 65536!
        assert!(decode_wal_header_page_size([0x00, 0x00, 0x00, 0x01]).is_err());
        assert!(decode_shm_page_size(0).is_err());
        assert!(decode_shm_page_size(2).is_err());
        assert!(decode_shm_page_size(0x0201).is_err());
    }
}
