//! 100-byte SQLite Format 3 database file header (`btreeInt.h:57-83`, `btree.c:3336-3576`).

use sqliters_types::{
    ErrorCode, Pgno, Result, SQLITE_DEFAULT_PAGE_SIZE, SQLITE_FILE_HEADER, SQLITE_MAX_PAGE_SIZE,
    SQLITE_MIN_PAGE_SIZE, SQLITE_VERSION_NUMBER, SqliteError,
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

    /// Validates whether `page_size` is a legal SQLite page size (power of 2 in `512..=65536`).
    #[inline]
    pub const fn is_valid_page_size(page_size: u32) -> bool {
        page_size >= SQLITE_MIN_PAGE_SIZE
            && page_size <= SQLITE_MAX_PAGE_SIZE
            && page_size.is_power_of_two()
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
        if read_version > 2 {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                format!("unsupported SQLite read format version: {read_version}"),
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
        // Empty freshly initialized DBs may have 0 before first schema write; treat 0 as 1 (UTF-8).
        let normalized_encoding = if text_encoding == 0 { 1 } else { text_encoding };

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
