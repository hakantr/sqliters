//! `sqliters` — Pure safe Rust (`unsafe_code = "forbid"`), zero-dependency port of SQLite 3.54.1,
//! 100% binary-compatible with SQLite Format 3 database files.
//!
//! # Architecture (`sqlite/src/*` -> `crates/sqliters-*`)
//! - [`types`] (`sqliters-types`): Error codes, limits, 9-byte big-endian varints, `Value` / `TypeAffinity` / `Collation`, Format 4 records.
//! - [`vfs`] (`sqliters-vfs`): OS & in-memory Virtual File System (`os.h`, `os_unix.c`, `memvfs.c`).
//! - [`pager`] (`sqliters-pager`): 100-byte database header, page cache, atomic rollback journal (`-journal`), crash recovery, and savepoints (`pager.c`, `pcache1.c`).
//! - [`btree`] (`sqliters-btree`): Table B+Trees (`0x05`/`0x0d`), Index B-Trees (`0x02`/`0x0a`), overflow chains, freelist, and `PRAGMA integrity_check` (`btree.c`).
//! - [`syntax`] (`sqliters-syntax`): Complete SQLite 3.54.1 tokenizer, `is_complete` DFA, AST, and recursive-descent/Pratt SQL parser (`tokenize.c`, `complete.c`, `parse.y`).
//! - [`vdbe`] (`sqliters-vdbe`): Bytecode VM, cursors, sorter, scalar & aggregate SQL functions (`vdbe.c`, `func.c`).
//! - [`core`] (`sqliters-core`): `sqlite_schema` catalog, SQL-to-VDBE compiler, relational query engine, DDL/DML/PRAGMAs, and [`Connection`] / [`Statement`] API.

pub use sqliters_btree as btree;
pub use sqliters_core as core;
pub use sqliters_pager as pager;
pub use sqliters_syntax as syntax;
pub use sqliters_types as types;
pub use sqliters_vdbe as vdbe;
pub use sqliters_vfs as vfs;

pub use sqliters_core::{
    ColumnSchema, Connection, IndexSchema, QueryOutput, QueryResult, Row, Schema, Statement,
    StepResult, TableSchema,
};
pub use sqliters_syntax::{is_complete, parse_one, parse_statements};
pub use sqliters_types::{
    Collation, ErrorCode, Pgno, Result, SQLITE_DEFAULT_PAGE_SIZE, SQLITE_FILE_HEADER,
    SQLITE_SOURCE_ID, SQLITE_VERSION, SQLITE_VERSION_NUMBER, SqliteError, StorageClass,
    TypeAffinity, Value,
};
pub use sqliters_vfs::{LockLevel, MemoryVfs, OpenFlags, OsVfs, SyncFlags, Vfs, VfsFile};

/// Returns the SQLite library version string (`"3.54.1"`), matching `sqlite3_libversion()`.
#[inline]
pub const fn libversion() -> &'static str {
    SQLITE_VERSION
}

/// Returns the SQLite library version integer (`3_054_001`), matching `sqlite3_libversion_number()`.
#[inline]
pub const fn libversion_number() -> u32 {
    SQLITE_VERSION_NUMBER
}
