//! Diagnostic profile of the pinned SQLite 3.54.1 C reference build.
//! COMPILER describes that compatibility target; the Rust toolchain is pinned separately.

pub const SQLITE_REFERENCE_COMPILE_OPTIONS: &[&str] = &[
    "ATOMIC_INTRINSICS=1",
    "COMPILER=gcc-15.2.0",
    "DEFAULT_AUTOVACUUM",
    "DEFAULT_CACHE_SIZE=-2000",
    "DEFAULT_FILE_FORMAT=4",
    "DEFAULT_JOURNAL_SIZE_LIMIT=-1",
    "DEFAULT_MMAP_SIZE=0",
    "DEFAULT_PAGE_SIZE=4096",
    "DEFAULT_PCACHE_INITSZ=20",
    "DEFAULT_RECURSIVE_TRIGGERS",
    "DEFAULT_SECTOR_SIZE=4096",
    "DEFAULT_SYNCHRONOUS=2",
    "DEFAULT_WAL_AUTOCHECKPOINT=1000",
    "DEFAULT_WAL_SYNCHRONOUS=2",
    "DEFAULT_WORKER_THREADS=0",
    "DIRECT_OVERFLOW_READ",
    "ENABLE_DBSTAT_VTAB",
    "ENABLE_FTS5",
    "ENABLE_MATH_FUNCTIONS",
    "ENABLE_RTREE",
    "MALLOC_SOFT_LIMIT=1024",
    "MAX_ATTACHED=10",
    "MAX_COLUMN=2000",
    "MAX_COMPOUND_SELECT=500",
    "MAX_DEFAULT_PAGE_SIZE=8192",
    "MAX_EXPR_DEPTH=1000",
    "MAX_FUNCTION_ARG=1000",
    "MAX_LENGTH=1000000000",
    "MAX_LIKE_PATTERN_LENGTH=50000",
    "MAX_MMAP_SIZE=0x7fff0000",
    "MAX_PAGE_COUNT=0xfffffffe",
    "MAX_PAGE_SIZE=65536",
    "MAX_SCHEMA=10000000",
    "MAX_SQL_LENGTH=1000000000",
    "MAX_TRIGGER_DEPTH=1000",
    "MAX_VARIABLE_NUMBER=32766",
    "MAX_VDBE_OP=250000000",
    "MAX_WORKER_THREADS=8",
    "MUTEX_PTHREADS",
    "SYSTEM_MALLOC",
    "TEMP_STORE=1",
    "THREADSAFE=1",
];

/// main.c:5264: NULL outside the compiled option array.
pub fn sqlite_compileoption_get(index: i32) -> Option<&'static str> {
    usize::try_from(index)
        .ok()
        .and_then(|i| SQLITE_REFERENCE_COMPILE_OPTIONS.get(i).copied())
}

/// main.c:5231: ASCII case-insensitive prefix, optional SQLITE_, identifier boundary.
pub fn sqlite_compileoption_used(name: &[u8]) -> bool {
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let mut name = &name[..end];
    if name.len() >= 7 && name[..7].eq_ignore_ascii_case(b"SQLITE_") {
        name = &name[7..];
    }
    // sqlite3Strlen30 masks the length after removing the optional prefix.
    name = &name[..name.len() & 0x3fff_ffff];
    SQLITE_REFERENCE_COMPILE_OPTIONS.iter().any(|s| {
        let b = s.as_bytes();
        b.len() >= name.len()
            && b[..name.len()].eq_ignore_ascii_case(name)
            && b.get(name.len()).is_none_or(|&c| {
                !(c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80)
            })
    })
}
