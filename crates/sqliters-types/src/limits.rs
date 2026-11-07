//! Compile-time version numbers and limits ported from `sqlite/VERSION`,
//! `sqlite/src/sqlite.h.in`, and `sqlite/src/sqliteLimit.h`.

/// Human-readable SQLite version string (`SQLITE_VERSION`).
pub const SQLITE_VERSION: &str = "3.54.1";

/// Numeric SQLite version (`X*1_000_000 + Y*1_000 + Z` = `3_054_001`).
pub const SQLITE_VERSION_NUMBER: u32 = 3_054_001;

/// Canonical Fossil check-in identifier from `sqlite/manifest.uuid`.
pub const SQLITE_SOURCE_ID: &str =
    "2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76 (sqliters)";

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

/// 1-based page number (`Pgno` in `sqliteInt.h`; 0 means "no such page").
pub type Pgno = u32;
