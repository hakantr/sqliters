//! `sqliters` — Zero-dependency Rust port of SQLite 3.54.1's rollback-journal and WAL storage and SQL engine,
//! interoperable with SQLite Format 3 database files (`UTF-8`, `UTF-16le`, `UTF-16be`).
#![forbid(unsafe_code)]
//!
//! # Quick Start
//!
//! ```
//! use sqliters::{Connection, Value};
//!
//! let conn = Connection::open_in_memory()?;
//! conn.execute("
//!     PRAGMA foreign_keys = ON;
//!     CREATE TABLE departments (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
//!     CREATE TABLE employees (
//!         id INTEGER PRIMARY KEY,
//!         dept_id INTEGER NOT NULL REFERENCES departments(id) ON DELETE CASCADE,
//!         name TEXT NOT NULL
//!     );
//!     INSERT INTO departments VALUES (1, 'Ar-Ge');
//!     INSERT INTO employees VALUES (10, 1, 'Ada');
//! ")?;
//!
//! let res = conn.query(
//!     "SELECT e.name, d.name AS dept FROM employees e JOIN departments d ON d.id = e.dept_id WHERE d.id = ?1",
//!     &[Value::Integer(1)],
//! )?;
//! assert_eq!(res.column_names, vec!["name", "dept"]);
//! assert_eq!(res.rows.len(), 1);
//! assert_eq!(res.rows[0].get_str(0), Some("Ada"));
//! assert_eq!(res.rows[0].get_str(1), Some("Ar-Ge"));
//! # Ok::<(), sqliters::SqliteError>(())
//! ```
//!
//! # Architecture (`sqlite/src/*` -> `crates/sqliters-*`)
//! - [`types`] (`sqliters-types`): Error codes, limits, 9-byte big-endian varints, `Value` / `TypeAffinity` / `Collation` / `TextEncoding`, Format 4 records.
//! - [`vfs`] (`sqliters-vfs`): OS & in-memory Virtual File System with POSIX `fcntl` advisory byte-range locking (`os.h`, `os_unix.c`, `memvfs.c`).
//! - [`pager`] (`sqliters-pager`): 100-byte database header, page cache, rollback journal (`-journal`), lock-coordinated hot-journal recovery, and savepoints (`pager.c`, `pcache1.c`).
//! - [`btree`] (`sqliters-btree`): Table B+Trees (`0x05`/`0x0d`), Index B-Trees (`0x02`/`0x0a`), overflow chains, freelist, and `PRAGMA integrity_check` (`btree.c`).
//! - [`syntax`] (`sqliters-syntax`): SQLite 3.54.1 tokenizer, `is_complete` DFA, AST, and recursive-descent/Pratt SQL parser (`tokenize.c`, `complete.c`, `parse.y`).
//! - [`vdbe`] (`sqliters-vdbe`): Bytecode VM, cursors, sorter, scalar & aggregate SQL functions (`vdbe.c`, `func.c`, `date.c`).
//! - [`core`] (`sqliters-core`): `sqlite_schema` catalog, SQL-to-VDBE compiler, relational query engine, foreign keys, DDL/DML/PRAGMAs, and [`Connection`] / [`Statement`] API.

pub use sqliters_btree as btree;
pub use sqliters_core as core;
pub use sqliters_pager as pager;
pub use sqliters_syntax as syntax;
pub use sqliters_types as types;
pub use sqliters_vdbe as vdbe;
pub use sqliters_vfs as vfs;

#[doc(hidden)]
pub use sqliters_core::TestBlobHandle;
pub use sqliters_core::{
    CloseError, ColumnSchema, Connection, ConnectionOpenOptions, ForeignKeySchema, IndexSchema,
    InterruptHandle, PrepareMode, QueryOutput, QueryResult, Row, SQLITE_DEFAULT_WAL_AUTOCHECKPOINT,
    Schema, Statement, StepResult, TableSchema,
};
pub use sqliters_pager::{
    AutoVacuumMode, DatabaseHeader, JournalMode, LockingMode, WalCheckpointMode,
    WalCheckpointResult,
};
pub use sqliters_syntax::{FkAction, is_complete, parse_one, parse_statements};
pub use sqliters_types::{
    Collation, ConflictResolutionEffect, DatabaseKind, ErrorCode, LockGeometry, Pgno, Result,
    SQLITE_DEFAULT_PAGE_SIZE, SQLITE_FILE_HEADER, SQLITE_SOURCE_ID, SQLITE_VERSION,
    SQLITE_VERSION_NUMBER, SqliteError, StorageClass, TextEncoding, TypeAffinity, Value,
};
pub use sqliters_vfs::{LockLevel, MemoryVfs, OpenFlags, OsVfs, SyncFlags, Vfs, VfsFile};

/// Returns the SQLite library version string (`"3.54.1"`), matching `sqlite3_libversion()`.
///
/// ```
/// assert_eq!(sqliters::libversion(), "3.54.1");
/// assert_eq!(sqliters::libversion_number(), 3_054_001);
/// ```
#[inline]
pub const fn libversion() -> &'static str {
    SQLITE_VERSION
}

/// Returns the SQLite library version integer (`3_054_001`), matching `sqlite3_libversion_number()`.
#[inline]
pub const fn libversion_number() -> u32 {
    SQLITE_VERSION_NUMBER
}
