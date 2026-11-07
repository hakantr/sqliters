//! Result codes and error types ported from `sqlite/src/sqlite.h.in`.

use std::fmt;

/// Primary SQLite result codes (`SQLITE_OK`, `SQLITE_ERROR`, ..., `SQLITE_DONE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ErrorCode {
    Ok = 0,
    Error = 1,
    Internal = 2,
    Perm = 3,
    Abort = 4,
    Busy = 5,
    Locked = 6,
    NoMem = 7,
    ReadOnly = 8,
    Interrupt = 9,
    IoErr = 10,
    Corrupt = 11,
    NotFound = 12,
    Full = 13,
    CantOpen = 14,
    Protocol = 15,
    Empty = 16,
    Schema = 17,
    TooBig = 18,
    Constraint = 19,
    Mismatch = 20,
    Misuse = 21,
    NoLfs = 22,
    Auth = 23,
    Format = 24,
    Range = 25,
    NotADb = 26,
    Notice = 27,
    Warning = 28,
    Row = 100,
    Done = 101,
}

impl ErrorCode {
    /// Returns the standard SQLite error string (`sqlite3ErrStr` in `main.c`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "not an error",
            Self::Error => "SQL logic error",
            Self::Internal => "internal logic error",
            Self::Perm => "access permission denied",
            Self::Abort => "query aborted",
            Self::Busy => "database is locked",
            Self::Locked => "database table is locked",
            Self::NoMem => "out of memory",
            Self::ReadOnly => "attempt to write a readonly database",
            Self::Interrupt => "interrupted",
            Self::IoErr => "disk I/O error",
            Self::Corrupt => "database disk image is malformed",
            Self::NotFound => "unknown operation",
            Self::Full => "database or disk is full",
            Self::CantOpen => "unable to open database file",
            Self::Protocol => "locking protocol",
            Self::Empty => "empty",
            Self::Schema => "database schema has changed",
            Self::TooBig => "string or blob too big",
            Self::Constraint => "constraint failed",
            Self::Mismatch => "datatype mismatch",
            Self::Misuse => "bad parameter or other API misuse",
            Self::NoLfs => "large file support is disabled",
            Self::Auth => "authorization denied",
            Self::Format => "not used",
            Self::Range => "column index out of range",
            Self::NotADb => "file is not a database",
            Self::Notice => "notification message",
            Self::Warning => "warning message",
            Self::Row => "another row available",
            Self::Done => "no more rows available",
        }
    }

    /// Converts a raw extended or primary code to its primary `ErrorCode`.
    pub const fn from_raw_primary(code: i32) -> Self {
        match code & 0xff {
            0 => Self::Ok,
            1 => Self::Error,
            2 => Self::Internal,
            3 => Self::Perm,
            4 => Self::Abort,
            5 => Self::Busy,
            6 => Self::Locked,
            7 => Self::NoMem,
            8 => Self::ReadOnly,
            9 => Self::Interrupt,
            10 => Self::IoErr,
            11 => Self::Corrupt,
            12 => Self::NotFound,
            13 => Self::Full,
            14 => Self::CantOpen,
            15 => Self::Protocol,
            16 => Self::Empty,
            17 => Self::Schema,
            18 => Self::TooBig,
            19 => Self::Constraint,
            20 => Self::Mismatch,
            21 => Self::Misuse,
            22 => Self::NoLfs,
            23 => Self::Auth,
            24 => Self::Format,
            25 => Self::Range,
            26 => Self::NotADb,
            27 => Self::Notice,
            28 => Self::Warning,
            100 => Self::Row,
            101 => Self::Done,
            _ => Self::Error,
        }
    }
}

/// Extended SQLite error codes (`sqlite.h.in` lines 500-581).
pub mod extended {
    use super::ErrorCode;

    pub const SQLITE_IOERR_READ: i32 = (ErrorCode::IoErr as i32) | (1 << 8);
    pub const SQLITE_IOERR_SHORT_READ: i32 = (ErrorCode::IoErr as i32) | (2 << 8);
    pub const SQLITE_IOERR_WRITE: i32 = (ErrorCode::IoErr as i32) | (3 << 8);
    pub const SQLITE_IOERR_FSYNC: i32 = (ErrorCode::IoErr as i32) | (4 << 8);
    pub const SQLITE_IOERR_TRUNCATE: i32 = (ErrorCode::IoErr as i32) | (6 << 8);
    pub const SQLITE_IOERR_FSTAT: i32 = (ErrorCode::IoErr as i32) | (7 << 8);
    pub const SQLITE_IOERR_UNLOCK: i32 = (ErrorCode::IoErr as i32) | (8 << 8);
    pub const SQLITE_IOERR_RDLOCK: i32 = (ErrorCode::IoErr as i32) | (9 << 8);
    pub const SQLITE_IOERR_DELETE: i32 = (ErrorCode::IoErr as i32) | (10 << 8);
    pub const SQLITE_IOERR_CHECKRESERVEDLOCK: i32 = (ErrorCode::IoErr as i32) | (14 << 8);
    pub const SQLITE_IOERR_LOCK: i32 = (ErrorCode::IoErr as i32) | (15 << 8);
    pub const SQLITE_READONLY_ROLLBACK: i32 = (ErrorCode::ReadOnly as i32) | (3 << 8);
    pub const SQLITE_CORRUPT_INDEX: i32 = (ErrorCode::Corrupt as i32) | (3 << 8);
    pub const SQLITE_CONSTRAINT_CHECK: i32 = (ErrorCode::Constraint as i32) | (1 << 8);
    pub const SQLITE_CONSTRAINT_FOREIGNKEY: i32 = (ErrorCode::Constraint as i32) | (3 << 8);
    pub const SQLITE_CONSTRAINT_NOTNULL: i32 = (ErrorCode::Constraint as i32) | (5 << 8);
    pub const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = (ErrorCode::Constraint as i32) | (6 << 8);
    pub const SQLITE_CONSTRAINT_UNIQUE: i32 = (ErrorCode::Constraint as i32) | (8 << 8);
    pub const SQLITE_CONSTRAINT_ROWID: i32 = (ErrorCode::Constraint as i32) | (10 << 8);
    pub const SQLITE_CONSTRAINT_DATATYPE: i32 = (ErrorCode::Constraint as i32) | (12 << 8);
}

/// Transaction/statement rollback behavior attached to a constraint error (`OE_Abort`, `OE_Fail`, `OE_Rollback`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ConflictResolutionEffect {
    /// Roll back only the current SQL statement's savepoint (`OR ABORT`, SQLite's default).
    #[default]
    Abort,
    /// Stop the current statement immediately **without** rolling back prior row changes made by it (`OR FAIL`).
    Fail,
    /// Roll back the entire enclosing transaction and return the connection to autocommit mode (`OR ROLLBACK`).
    Rollback,
}

/// Rich error returned across all `sqliters` subsystems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteError {
    pub code: ErrorCode,
    pub extended_code: i32,
    pub message: String,
    pub offset: Option<usize>,
    pub conflict_effect: ConflictResolutionEffect,
}

impl SqliteError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            extended_code: code as i32,
            message: message.into(),
            offset: None,
            conflict_effect: ConflictResolutionEffect::Abort,
        }
    }

    pub fn with_extended(extended_code: i32, message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::from_raw_primary(extended_code),
            extended_code,
            message: message.into(),
            offset: None,
            conflict_effect: ConflictResolutionEffect::Abort,
        }
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn with_conflict_effect(mut self, effect: ConflictResolutionEffect) -> Self {
        self.conflict_effect = effect;
        self
    }

    pub fn syntax(message: impl Into<String>, offset: usize) -> Self {
        Self::new(ErrorCode::Error, message).with_offset(offset)
    }

    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Corrupt, message)
    }

    pub fn constraint(extended_code: i32, message: impl Into<String>) -> Self {
        Self::with_extended(extended_code, message)
    }

    pub fn misuse(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Misuse, message)
    }

    pub fn readonly(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ReadOnly, message)
    }
}

impl fmt::Display for SqliteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.code.as_str())
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for SqliteError {}

impl From<std::io::Error> for SqliteError {
    fn from(err: std::io::Error) -> Self {
        Self::new(ErrorCode::IoErr, err.to_string())
    }
}

/// Alias for `std::result::Result<T, SqliteError>`.
pub type Result<T> = std::result::Result<T, SqliteError>;
