//! Compile-time version numbers and limits ported from `sqlite/VERSION`,
//! `sqlite/src/sqlite.h.in`, and `sqlite/src/sqliteLimit.h`.

/// Human-readable SQLite version string (`SQLITE_VERSION`).
pub const SQLITE_VERSION: &str = "3.54.1";

/// Numeric SQLite version (`X*1_000_000 + Y*1_000 + Z` = `3_054_001`).
pub const SQLITE_VERSION_NUMBER: u32 = 3_054_001;

/// Canonical Fossil check-in identifier from `sqlite/manifest.uuid`.
pub const SQLITE_SOURCE_ID: &str = "2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76 (sqliters)";

/// 16-byte magic header at offset 0 of every SQLite 3 database file.
pub const SQLITE_FILE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Maximum length of a TEXT or BLOB in bytes (`SQLITE_MAX_LENGTH`).
pub const SQLITE_MAX_LENGTH: usize = 1_000_000_000;

/// Maximum number of columns in a table, index, view, or SELECT (`SQLITE_MAX_COLUMN`).
pub const SQLITE_MAX_COLUMN: usize = 2_000;

/// Maximum length of an SQL statement in bytes (`SQLITE_MAX_SQL_LENGTH`).
pub const SQLITE_MAX_SQL_LENGTH: usize = 1_000_000_000;

/// Maximum depth of an expression tree (`SQLITE_MAX_EXPR_DEPTH`).
pub const SQLITE_MAX_EXPR_DEPTH: usize = 1_000;

/// Maximum parser stack depth (`SQLITE_MAX_PARSER_DEPTH`).
pub const SQLITE_MAX_PARSER_DEPTH: usize = 2_500;

/// Maximum terms in a compound SELECT (`SQLITE_MAX_COMPOUND_SELECT`).
pub const SQLITE_MAX_COMPOUND_SELECT: usize = 500;

/// Maximum arguments to an SQL function (`SQLITE_MAX_FUNCTION_ARG`).
pub const SQLITE_MAX_FUNCTION_ARG: usize = 1_000;

/// Default suggested page cache size in KiB (`SQLITE_DEFAULT_CACHE_SIZE`).
pub const SQLITE_DEFAULT_CACHE_SIZE: i32 = -2_000;

/// Maximum attached databases (`SQLITE_MAX_ATTACHED`).
pub const SQLITE_MAX_ATTACHED: usize = 10;

/// Maximum host parameter number `?NNN` (`SQLITE_MAX_VARIABLE_NUMBER`).
pub const SQLITE_MAX_VARIABLE_NUMBER: usize = 32_766;

/// Minimum SQLite page size in bytes.
pub const SQLITE_MIN_PAGE_SIZE: u32 = 512;

/// Default SQLite page size in bytes (`SQLITE_DEFAULT_PAGE_SIZE`).
pub const SQLITE_DEFAULT_PAGE_SIZE: u32 = 4_096;

/// Maximum SQLite page size in bytes (`SQLITE_MAX_PAGE_SIZE`).
pub const SQLITE_MAX_PAGE_SIZE: u32 = 65_536;

/// Maximum page count (`SQLITE_MAX_PAGE_COUNT`).
pub const SQLITE_MAX_PAGE_COUNT: u32 = 0xffff_fffe;

/// Maximum LIKE or GLOB pattern length (`SQLITE_MAX_LIKE_PATTERN_LENGTH`).
pub const SQLITE_MAX_LIKE_PATTERN_LENGTH: usize = 50_000;

/// Byte offset of the lock-byte region (`PENDING_BYTE` = 1 GiB).
pub const PENDING_BYTE_OFFSET: u64 = 0x4000_0000;

/// Test-profile byte offset of the lock-byte region (`0x0001_0000` = 64 KiB, `tester.tcl`).
pub const TEST_PENDING_BYTE_OFFSET: u64 = 0x0001_0000;

/// Length in bytes of the `SHARED` byte-range lock region (`SHARED_SIZE` in `os_unix.c` / `os_win.c`).
pub const SHARED_LOCK_SIZE: u64 = 510;

/// 1-based page number (`Pgno` in `sqliteInt.h`; 0 means "no such page").
pub type Pgno = u32;

/// Byte-range lock geometry derived from `PENDING_BYTE` (`os_unix.c`, `os_win.c`, `pager.h`, `btreeInt.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LockGeometry {
    pending_byte_offset: u64,
}

impl Default for LockGeometry {
    #[inline]
    fn default() -> Self {
        Self::PRODUCTION
    }
}

impl LockGeometry {
    /// Standard SQLite production lock geometry (`PENDING_BYTE = 0x4000_0000` = 1 GiB).
    pub const PRODUCTION: Self = Self {
        pending_byte_offset: PENDING_BYTE_OFFSET,
    };

    /// SQLite test suite lock geometry (`PENDING_BYTE = 0x0001_0000` = 64 KiB).
    pub const TEST_64K: Self = Self {
        pending_byte_offset: TEST_PENDING_BYTE_OFFSET,
    };

    /// Returns the standard 1 GiB production lock geometry.
    #[inline]
    pub const fn production() -> Self {
        Self::PRODUCTION
    }

    /// Returns the 64 KiB test-profile lock geometry.
    #[inline]
    pub const fn test_64k() -> Self {
        Self::TEST_64K
    }

    /// Constructs a [`LockGeometry`] from a validated `pending_byte_offset`
    /// (`SQLITE_TESTCTRL_PENDING_BYTE` in `sqlite/src/main.c`).
    pub fn try_from_pending_byte(pending_byte_offset: u64) -> crate::Result<Self> {
        // Must be > 0 and fit within a 32-bit unsigned offset (`unsigned int` in C
        // `SQLITE_TESTCTRL_PENDING_BYTE`) without overflowing the 512-byte lock range.
        if pending_byte_offset == 0 || pending_byte_offset > u64::from(u32::MAX) - 512 {
            return Err(crate::SqliteError::misuse(format!(
                "invalid pending_byte_offset: {pending_byte_offset:#x}"
            )));
        }
        Ok(Self {
            pending_byte_offset,
        })
    }

    /// Byte offset of `PENDING_BYTE`.
    #[inline]
    pub const fn pending_byte_offset(&self) -> u64 {
        self.pending_byte_offset
    }

    /// Byte offset of `RESERVED_BYTE` (`PENDING_BYTE + 1`).
    #[inline]
    pub const fn reserved_byte_offset(&self) -> u64 {
        self.pending_byte_offset + 1
    }

    /// First byte offset of the `SHARED` lock region (`PENDING_BYTE + 2`).
    #[inline]
    pub const fn shared_first_offset(&self) -> u64 {
        self.pending_byte_offset + 2
    }

    /// Byte length of the `SHARED` lock region (`510`).
    #[inline]
    pub const fn shared_size(&self) -> u64 {
        SHARED_LOCK_SIZE
    }

    /// 1-based page number containing `PENDING_BYTE` (`PENDING_BYTE_PAGE(pBt)` in `sqlite/src/btreeInt.h:612`).
    #[inline(always)]
    pub const fn lock_byte_page(&self, page_size: u32) -> Pgno {
        if page_size.is_power_of_two() {
            ((self.pending_byte_offset >> page_size.trailing_zeros()) as u32) + 1
        } else {
            assert!(page_size > 0, "page_size must be non-zero");
            ((self.pending_byte_offset / (page_size as u64)) as u32) + 1
        }
    }

    /// Super-journal sentinel page number (`PAGER_SJ_PGNO_COMPUTED(x)` in `sqlite/src/pager.h:53`
    /// and `pPager->lckPgno` in `sqlite/src/pager.c:3866`).
    ///
    /// In SQLite 3.54.1, `PAGER_SJ_PGNO_COMPUTED(x)` is `((Pgno)((PENDING_BYTE/((x)->pageSize))+1))`,
    /// identical to `PENDING_BYTE_PAGE`: because the lock-byte page is never allocated in a database,
    /// rollback journals reuse that page number to mark the super-journal trailer record.
    #[inline]
    pub const fn sj_pgno(&self, page_size: u32) -> Pgno {
        self.lock_byte_page(page_size)
    }
}

/// Logical database kind distinguishing named on-disk files, private temporary databases (`""`),
/// and pure in-memory databases (`":memory:"` / `OpenFlags::MEMORY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DatabaseKind {
    /// Named on-disk database file.
    NamedDisk,
    /// Private temporary database (`""` in `Connection::open("")` or `ATTACH ''`).
    PrivateTemporary,
    /// Pure in-memory database (`":memory:"` or `OpenFlags::MEMORY`).
    Memory,
}

/// Returns `true` iff `size` is a valid SQLite page size (power of two in `512..=65536`).
#[inline]
pub const fn is_valid_page_size(size: u32) -> bool {
    size >= SQLITE_MIN_PAGE_SIZE && size <= SQLITE_MAX_PAGE_SIZE && size.is_power_of_two()
}

/// Encodes a validated SQLite page size into the 2-byte big-endian `.db` header format (`offset 16..18`),
/// where `65536` is stored as `1` (`[0x00, 0x01]`).
#[inline]
pub fn encode_db_header_page_size(size: u32) -> crate::Result<[u8; 2]> {
    if !is_valid_page_size(size) {
        return Err(crate::SqliteError::misuse(format!(
            "invalid SQLite page size: {size}"
        )));
    }
    let raw: u16 = if size == 65_536 { 1 } else { size as u16 };
    Ok(raw.to_be_bytes())
}

/// Decodes the 2-byte big-endian `.db` header page size (`offset 16..18`), mapping `1 -> 65536`.
#[inline]
pub fn decode_db_header_page_size(bytes: [u8; 2]) -> crate::Result<u32> {
    let raw = u16::from_be_bytes(bytes);
    let size = if raw == 1 { 65_536 } else { u32::from(raw) };
    if !is_valid_page_size(size) {
        return Err(crate::SqliteError::corrupt(format!(
            "invalid database header page size encoding: {raw}"
        )));
    }
    Ok(size)
}

/// Encodes a validated SQLite page size into the 4-byte big-endian `.wal` header format (`offset 8..12`),
/// where `65536` is stored directly as `65536` (`[0x00, 0x01, 0x00, 0x00]`).
#[inline]
pub fn encode_wal_header_page_size(size: u32) -> crate::Result<[u8; 4]> {
    if !is_valid_page_size(size) {
        return Err(crate::SqliteError::misuse(format!(
            "invalid SQLite WAL page size: {size}"
        )));
    }
    Ok(size.to_be_bytes())
}

/// Decodes the 4-byte big-endian `.wal` header page size (`offset 8..12`), rejecting `1` or non-powers-of-two.
#[inline]
pub fn decode_wal_header_page_size(bytes: [u8; 4]) -> crate::Result<u32> {
    let size = u32::from_be_bytes(bytes);
    if !is_valid_page_size(size) {
        return Err(crate::SqliteError::corrupt(format!(
            "invalid WAL header page size: {size}"
        )));
    }
    Ok(size)
}

/// Encodes a validated SQLite page size into `WalIndexHdr.szPage` native-endian `u16`
/// (`((size & 0xff00) | (size >> 16)) as u16` in `sqlite/src/wal.c:1551`, so `65536 -> 1`).
#[inline]
pub fn encode_shm_page_size(size: u32) -> crate::Result<u16> {
    if !is_valid_page_size(size) {
        return Err(crate::SqliteError::misuse(format!(
            "invalid SQLite SHM page size: {size}"
        )));
    }
    Ok(((size & 0xff00) | (size >> 16)) as u16)
}

/// Decodes `WalIndexHdr.szPage` native-endian `u16` into bytes
/// (`(encoded & 0xfe00) + ((encoded & 1) << 16)` in `sqlite/src/wal.c:2214`, so `1 -> 65536`).
#[inline]
pub fn decode_shm_page_size(encoded: u16) -> crate::Result<u32> {
    let raw = u32::from(encoded);
    let size = (raw & 0xfe00) + ((raw & 1) << 16);
    if !is_valid_page_size(size) || (((size & 0xff00) | (size >> 16)) as u16) != encoded {
        return Err(crate::SqliteError::corrupt(format!(
            "invalid WalIndexHdr.szPage encoding: {encoded:#06x}"
        )));
    }
    Ok(size)
}
