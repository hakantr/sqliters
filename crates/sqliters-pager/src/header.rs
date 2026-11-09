//! 100-byte SQLite Format 3 database file header (`btreeInt.h:57-83`, `btree.c:3336-3576`).

use sqliters_types::{
    ErrorCode, Pgno, Result, SQLITE_DEFAULT_PAGE_SIZE, SQLITE_FILE_HEADER, SQLITE_MAX_PAGE_SIZE,
    SQLITE_MIN_PAGE_SIZE, SQLITE_VERSION_NUMBER, SqliteError, TextEncoding,
};

/// Size in bytes of the database file header on Page 1.
pub const DB_HEADER_SIZE: usize = 100;

/// Parsed representation of the 100-byte header at offset 0 of Page 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseHeader {
    pub page_size: u32,
    pub write_version: u8,
    pub read_version: u8,
    pub reserved_bytes: u8,
    pub change_counter: u32,
    pub database_size_pages: Pgno,
    pub first_freelist_trunk_page: Pgno,
    pub total_freelist_pages: u32,
    pub schema_cookie: u32,
    pub schema_format: u32,
    pub default_cache_size: i32,
    pub largest_root_btree_page: Pgno,
    pub text_encoding: u32,
    pub user_version: i32,
    pub incremental_vacuum_mode: u32,
    pub application_id: i32,
    pub version_valid_for: u32,
    pub sqlite_version_number: u32,
}

impl Default for DatabaseHeader {
    fn default() -> Self {
        Self::new_with_page_size(SQLITE_DEFAULT_PAGE_SIZE)
    }
}

impl DatabaseHeader {
    pub const fn new_with_page_size(page_size: u32) -> Self {
        Self {
            page_size,
            write_version: 1,
            read_version: 1,
            reserved_bytes: 0,
            change_counter: 1,
            database_size_pages: 1,
            first_freelist_trunk_page: 0,
            total_freelist_pages: 0,
            schema_cookie: 0,
            schema_format: 4,
            default_cache_size: 0,
            largest_root_btree_page: 0,
            text_encoding: 1, // 1 = UTF-8
            user_version: 0,
            incremental_vacuum_mode: 0,
            application_id: 0,
            version_valid_for: 1,
            sqlite_version_number: SQLITE_VERSION_NUMBER,
        }
    }

    /// Usable bytes per page (`page_size - reserved_bytes`).
    #[inline]
    pub const fn usable_size(&self) -> u32 {
        self.page_size - (self.reserved_bytes as u32)
    }

    /// Validated [`TextEncoding`] of this database (`UTF-8`, `UTF-16le`, or `UTF-16be`).
    #[inline]
    pub const fn encoding(&self) -> TextEncoding {
        match TextEncoding::from_header_u32(self.text_encoding) {
            Some(enc) => enc,
            None => TextEncoding::Utf8,
        }
    }

    /// Validates whether `page_size` is a legal SQLite page size (power of 2 in `512..=65536`).
    #[inline]
    pub const fn is_valid_page_size(page_size: u32) -> bool {
        page_size >= SQLITE_MIN_PAGE_SIZE
            && page_size <= SQLITE_MAX_PAGE_SIZE
            && page_size.is_power_of_two()
    }

    /// Returns `true` when `write_version > 2`, which forces the connection into read-only mode
    /// while permitting reads when `read_version` is in `1..=2` (`BTS_READ_ONLY`, `btree.c:3356`).
    #[inline]
    pub const fn forces_readonly(&self) -> bool {
        self.write_version > 2
    }

    /// Returns `true` when `read_version == 2`, indicating WAL mode (`btree.c:3371`).
    #[inline]
    pub const fn is_wal(&self) -> bool {
        self.read_version == 2
    }

    /// Parses the 100-byte header from `buf[0..100]`, validating SQLite invariants (`lockBtree`).
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < DB_HEADER_SIZE {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                "file is shorter than 100-byte SQLite header",
            ));
        }
        if &buf[0..16] != SQLITE_FILE_HEADER {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                "file is not a database",
            ));
        }

        let raw_ps = u16::from_be_bytes([buf[16], buf[17]]);
        let page_size = if raw_ps == 1 {
            65_536
        } else {
            u32::from(raw_ps)
        };
        if !Self::is_valid_page_size(page_size) {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                format!("invalid SQLite page size: {page_size}"),
            ));
        }

        let write_version = buf[18];
        let read_version = buf[19];
        if write_version == 0 || read_version == 0 || read_version > 2 {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                format!(
                    "unsupported SQLite format versions: write={write_version}, read={read_version}"
                ),
            ));
        }

        let reserved_bytes = buf[20];
        let usable = page_size - u32::from(reserved_bytes);
        if usable < 480 {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                "usable page size is less than 480 bytes",
            ));
        }

        // Payload fractions must be fixed at 64, 32, 32 since SQLite 3.6.0 (R-15465-20813).
        if buf[21] != 64 || buf[22] != 32 || buf[23] != 32 {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                "invalid embedded payload fractions in SQLite header",
            ));
        }

        let get_u32 = |off: usize| -> u32 {
            u32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
        };
        let get_i32 = |off: usize| -> i32 {
            i32::from_be_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
        };

        let text_encoding = get_u32(56);
        let Some(enc) = TextEncoding::from_header_u32(text_encoding) else {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                format!("invalid SQLite text encoding in header: {text_encoding}"),
            ));
        };
        let normalized_encoding = enc as u32;

        Ok(Self {
            page_size,
            write_version,
            read_version,
            reserved_bytes,
            change_counter: get_u32(24),
            database_size_pages: get_u32(28),
            first_freelist_trunk_page: get_u32(32),
            total_freelist_pages: get_u32(36),
            schema_cookie: get_u32(40),
            schema_format: {
                let sf = get_u32(44);
                if sf == 0 { 4 } else { sf }
            },
            default_cache_size: get_i32(48),
            largest_root_btree_page: get_u32(52),
            text_encoding: normalized_encoding,
            user_version: get_i32(60),
            incremental_vacuum_mode: get_u32(64),
            application_id: get_i32(68),
            version_valid_for: get_u32(92),
            sqlite_version_number: get_u32(96),
        })
    }

    /// Writes this 100-byte header into `out[0..100]`.
    pub fn write_into(&self, out: &mut [u8]) {
        assert!(out.len() >= DB_HEADER_SIZE);
        out[0..16].copy_from_slice(SQLITE_FILE_HEADER);
        let raw_ps: u16 = if self.page_size == 65_536 {
            1
        } else {
            self.page_size as u16
        };
        out[16..18].copy_from_slice(&raw_ps.to_be_bytes());
        out[18] = self.write_version;
        out[19] = self.read_version;
        out[20] = self.reserved_bytes;
        out[21] = 64;
        out[22] = 32;
        out[23] = 32;
        out[24..28].copy_from_slice(&self.change_counter.to_be_bytes());
        out[28..32].copy_from_slice(&self.database_size_pages.to_be_bytes());
        out[32..36].copy_from_slice(&self.first_freelist_trunk_page.to_be_bytes());
        out[36..40].copy_from_slice(&self.total_freelist_pages.to_be_bytes());
        out[40..44].copy_from_slice(&self.schema_cookie.to_be_bytes());
        out[44..48].copy_from_slice(&self.schema_format.to_be_bytes());
        out[48..52].copy_from_slice(&self.default_cache_size.to_be_bytes());
        out[52..56].copy_from_slice(&self.largest_root_btree_page.to_be_bytes());
        out[56..60].copy_from_slice(&self.text_encoding.to_be_bytes());
        out[60..64].copy_from_slice(&self.user_version.to_be_bytes());
        out[64..68].copy_from_slice(&self.incremental_vacuum_mode.to_be_bytes());
        out[68..72].copy_from_slice(&self.application_id.to_be_bytes());
        out[72..92].fill(0);
        out[92..96].copy_from_slice(&self.version_valid_for.to_be_bytes());
        out[96..100].copy_from_slice(&self.sqlite_version_number.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format3_header_version_validation_rules() {
        let mut raw = [0u8; DB_HEADER_SIZE];
        DatabaseHeader::default().write_into(&mut raw);

        // (1, 1), (2, 2), (1, 2), (2, 1) are all valid and writable (`!forces_readonly()`).
        for (wv, rv) in [(1u8, 1u8), (2, 2), (1, 2), (2, 1)] {
            raw[18] = wv;
            raw[19] = rv;
            let parsed = DatabaseHeader::parse(&raw).unwrap();
            assert_eq!(parsed.write_version, wv);
            assert_eq!(parsed.read_version, rv);
            assert!(!parsed.forces_readonly());
            assert_eq!(parsed.is_wal(), rv == 2);
        }

        // `write_version > 2` with `read_version` in `1..=2` parses validly and marks readonly (`btree.c:3356`).
        for (wv, rv) in [(3u8, 1u8), (3, 2), (255, 1), (255, 2)] {
            raw[18] = wv;
            raw[19] = rv;
            let parsed = DatabaseHeader::parse(&raw).unwrap();
            assert_eq!(parsed.write_version, wv);
            assert_eq!(parsed.read_version, rv);
            assert!(parsed.forces_readonly());
            assert_eq!(parsed.is_wal(), rv == 2);
        }

        // `0` in either version or `read_version > 2` fails with `ErrorCode::NotADb`.
        for (wv, rv) in [
            (0u8, 0u8),
            (0, 1),
            (0, 2),
            (1, 0),
            (2, 0),
            (1, 3),
            (2, 3),
            (3, 3),
        ] {
            raw[18] = wv;
            raw[19] = rv;
            let err = DatabaseHeader::parse(&raw).unwrap_err();
            assert_eq!(err.code, ErrorCode::NotADb, "wv={wv}, rv={rv}");
        }

        // 64 KiB page size (`u16 = 1` at `@16..18`) roundtrips with WAL `(2, 2)`.
        let mut hdr64k = DatabaseHeader::new_with_page_size(65_536);
        hdr64k.write_version = 2;
        hdr64k.read_version = 2;
        hdr64k.write_into(&mut raw);
        assert_eq!(&raw[16..18], &1u16.to_be_bytes());
        let parsed64k = DatabaseHeader::parse(&raw).unwrap();
        assert_eq!(parsed64k.page_size, 65_536);
        assert!(parsed64k.is_wal());
        assert!(!parsed64k.forces_readonly());
    }
}
