//! Virtual File System (`sqlite3_vfs` & `sqlite3_file`) ported from
//! `sqlite/src/os.h`, `sqlite/src/os.c`, `sqlite/src/os_unix.c`, and `sqlite/src/memdb.c`.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqliters_types::{ErrorCode, Result, SqliteError, extended};

/// File lock states matching `NO_LOCK..EXCLUSIVE_LOCK` in `sqlite/src/os.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum LockLevel {
    #[default]
    NoLock = 0,
    Shared = 1,
    Reserved = 2,
    Pending = 3,
    Exclusive = 4,
}

/// Flags passed to [`Vfs::open`] (`SQLITE_OPEN_*` in `sqlite.h.in`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenFlags(pub u32);

impl OpenFlags {
    pub const READONLY: Self = Self(0x0000_0001);
    pub const READWRITE: Self = Self(0x0000_0002);
    pub const CREATE: Self = Self(0x0000_0004);
    pub const DELETEONCLOSE: Self = Self(0x0000_0008);
    pub const EXCLUSIVE: Self = Self(0x0000_0010);
    pub const MEMORY: Self = Self(0x0000_0080);
    pub const MAIN_DB: Self = Self(0x0000_0100);
    pub const TEMP_DB: Self = Self(0x0000_0200);
    pub const MAIN_JOURNAL: Self = Self(0x0000_0800);
    pub const SUBJOURNAL: Self = Self(0x0000_2000);
    pub const WAL: Self = Self(0x0008_0000);

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub const fn is_readonly(self) -> bool {
        self.contains(Self::READONLY) && !self.contains(Self::READWRITE)
    }
}

impl std::ops::BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

/// Flags for [`VfsFile::sync`] (`SQLITE_SYNC_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFlags {
    Normal,
    Full,
    DataOnly,
}

/// Open file handle corresponding to `sqlite3_file` / `sqlite3_io_methods`.
pub trait VfsFile: Send + Sync {
    /// Reads up to `buf.len()` bytes starting at `offset`.
    ///
    /// Following SQLite's `unixRead` (`os_unix.c`), if fewer than `buf.len()` bytes
    /// exist before EOF, the unread tail `buf[n..]` MUST be zero-filled and `Ok(n)` returned.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize>;

    /// Writes all bytes of `buf` at `offset` (`xWrite`).
    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()>;

    /// Truncates (or extends) the file to `size` bytes (`xTruncate`).
    fn truncate(&self, size: u64) -> Result<()>;

    /// Flushes OS buffers to persistent storage (`xSync`).
    fn sync(&self, flags: SyncFlags) -> Result<()>;

    /// Returns the current file size in bytes (`xFileSize`).
    fn file_size(&self) -> Result<u64>;

    /// Upgrades the file lock to `level` (`xLock`).
    fn lock(&self, level: LockLevel) -> Result<()>;

    /// Downgrades the file lock to `level` (`xUnlock`, where `level <= Shared`).
    fn unlock(&self, level: LockLevel) -> Result<()>;

    /// Returns the current lock level held by this file handle.
    fn lock_level(&self) -> LockLevel;

    /// Returns device sector size in bytes (`xSectorSize`, default 4096).
    fn sector_size(&self) -> u32 {
        4096
    }

    /// Whether this handle was opened read-only.
    fn is_readonly(&self) -> bool;
}

/// Operating system VFS interface corresponding to `sqlite3_vfs`.
pub trait Vfs: Send + Sync {
    /// Name of this VFS (e.g. `"unix"` or `"memdb"`).
    fn name(&self) -> &str;

    /// Opens a file at `path` (or creates a temporary/anonymous file if `path` is `None`).
    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>>;

    /// Deletes a file at `path` (`xDelete`). If the file does not exist, returns `Ok(())`.
    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()>;

    /// Checks whether `path` exists (`xAccess`).
    fn exists(&self, path: &Path) -> Result<bool>;

    /// Resolves `path` into a canonical or absolute pathname (`xFullPathname`).
    fn full_pathname(&self, path: &Path) -> Result<PathBuf>;

    /// Fills `buf` with high-quality pseudo-random bytes (`xRandomness`).
    fn randomness(&self, buf: &mut [u8]) {
        fill_random_bytes(buf);
    }

    /// Sleeps for at least `microseconds` (`xSleep`).
    fn sleep(&self, microseconds: u64) {
        std::thread::sleep(Duration::from_micros(microseconds));
    }

    /// Current time as Julian Day milliseconds (`xCurrentTimeInt64` in `os_unix.c`).
    /// SQLite epoch offset: Unix epoch `1970-01-01 00:00:00 UTC` = `210_866_760_000_000` JD ms.
    fn current_time_int64(&self) -> i64 {
        const UNIX_EPOCH_JD_MS: i64 = 210_866_760_000_000;
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        UNIX_EPOCH_JD_MS + ms
    }
}

// ============================================================================
// In-Memory VFS (`memdb.c` port)
// ============================================================================

struct MemEntry {
    data: RwLock<Vec<u8>>,
    lock: Mutex<LockLevel>,
}

impl MemEntry {
    fn new() -> Self {
        Self {
            data: RwLock::new(Vec::new()),
            lock: Mutex::new(LockLevel::NoLock),
        }
    }
}

struct MemFile {
    entry: Arc<MemEntry>,
    readonly: bool,
    held_lock: Mutex<LockLevel>,
}

impl VfsFile for MemFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let guard = self
            .entry
            .data
            .read()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memdb lock"))?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= guard.len() {
            buf.fill(0);
            return Ok(0);
        }
        let avail = (guard.len() - start).min(buf.len());
        buf[..avail].copy_from_slice(&guard[start..start + avail]);
        if avail < buf.len() {
            buf[avail..].fill(0);
        }
        Ok(avail)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        if self.readonly {
            return Err(SqliteError::readonly("attempt to write a readonly memdb"));
        }
        let mut guard = self
            .entry
            .data
            .write()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memdb lock"))?;
        let start = usize::try_from(offset).map_err(|_| {
            SqliteError::with_extended(extended::SQLITE_IOERR_WRITE, "offset overflow")
        })?;
        let end = start.checked_add(buf.len()).ok_or_else(|| {
            SqliteError::with_extended(extended::SQLITE_IOERR_WRITE, "write size overflow")
        })?;
        if end > guard.len() {
            guard.resize(end, 0);
        }
        guard[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn truncate(&self, size: u64) -> Result<()> {
        if self.readonly {
            return Err(SqliteError::readonly(
                "attempt to truncate a readonly memdb",
            ));
        }
        let mut guard = self
            .entry
            .data
            .write()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memdb lock"))?;
        let sz = usize::try_from(size).map_err(|_| {
            SqliteError::with_extended(extended::SQLITE_IOERR_TRUNCATE, "truncate size overflow")
        })?;
        guard.resize(sz, 0);
        Ok(())
    }

    fn sync(&self, _flags: SyncFlags) -> Result<()> {
        Ok(())
    }

    fn file_size(&self) -> Result<u64> {
        let guard = self
            .entry
            .data
            .read()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memdb lock"))?;
        Ok(guard.len() as u64)
    }

    fn lock(&self, level: LockLevel) -> Result<()> {
        let mut held = self
            .held_lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        let mut global = self
            .entry
            .lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        if level > *held {
            *held = level;
            if level > *global {
                *global = level;
            }
        }
        Ok(())
    }

    fn unlock(&self, level: LockLevel) -> Result<()> {
        let mut held = self
            .held_lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        let mut global = self
            .entry
            .lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        if level < *held {
            *held = level;
            *global = level;
        }
        Ok(())
    }

    fn lock_level(&self) -> LockLevel {
        self.held_lock.lock().map(|g| *g).unwrap_or(LockLevel::NoLock)
    }

    fn is_readonly(&self) -> bool {
        self.readonly
    }
}

/// In-memory VFS supporting anonymous (`None` / `":memory:"`) and named files (e.g. journals).
#[derive(Default)]
pub struct MemoryVfs {
    files: RwLock<HashMap<PathBuf, Arc<MemEntry>>>,
}

impl MemoryVfs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Vfs for MemoryVfs {
    fn name(&self) -> &str {
        "memdb"
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        let readonly = flags.is_readonly();
        let entry = match path {
            None => Arc::new(MemEntry::new()),
            Some(p) if p == Path::new(":memory:") => Arc::new(MemEntry::new()),
            Some(p) => {
                let mut map = self
                    .files
                    .write()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memvfs"))?;
                if let Some(existing) = map.get(p) {
                    if flags.contains(OpenFlags::EXCLUSIVE) && flags.contains(OpenFlags::CREATE) {
                        return Err(SqliteError::new(
                            ErrorCode::CantOpen,
                            format!("exclusive file already exists: {}", p.display()),
                        ));
                    }
                    Arc::clone(existing)
                } else if flags.contains(OpenFlags::CREATE) {
                    let e = Arc::new(MemEntry::new());
                    if !flags.contains(OpenFlags::DELETEONCLOSE) {
                        map.insert(p.to_path_buf(), Arc::clone(&e));
                    }
                    e
                } else {
                    return Err(SqliteError::new(
                        ErrorCode::CantOpen,
                        format!("unable to open in-memory file: {}", p.display()),
                    ));
                }
            }
        };

        Ok(Box::new(MemFile {
            entry,
            readonly,
            held_lock: Mutex::new(LockLevel::NoLock),
        }))
    }

    fn delete(&self, path: &Path, _sync_dir: bool) -> Result<()> {
        let mut map = self
            .files
            .write()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memvfs"))?;
        map.remove(path);
        Ok(())
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        let map = self
            .files
            .read()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memvfs"))?;
        Ok(map.contains_key(path))
    }

    fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
        Ok(path.to_path_buf())
    }
}

// ============================================================================
// Standard OS Filesystem VFS (`os_unix.c` / cross-platform safe Rust port)
// ============================================================================

struct OsFile {
    file: File,
    path: Option<PathBuf>,
    delete_on_close: bool,
    readonly: bool,
    lock_level: Mutex<LockLevel>,
}

impl Drop for OsFile {
    fn drop(&mut self) {
        if self.delete_on_close {
            if let Some(ref p) = self.path {
                let _ = fs::remove_file(p);
            }
        }
    }
}

impl VfsFile for OsFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut total = 0usize;
            while total < buf.len() {
                match self.file.read_at(&mut buf[total..], offset + total as u64) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        return Err(SqliteError::with_extended(
                            extended::SQLITE_IOERR_READ,
                            e.to_string(),
                        ));
                    }
                }
            }
            if total < buf.len() {
                buf[total..].fill(0);
            }
            Ok(total)
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom};
            let mut clone = self.file.try_clone().map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_READ, e.to_string())
            })?;
            clone.seek(SeekFrom::Start(offset)).map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_READ, e.to_string())
            })?;
            let mut total = 0usize;
            while total < buf.len() {
                match clone.read(&mut buf[total..]) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        return Err(SqliteError::with_extended(
                            extended::SQLITE_IOERR_READ,
                            e.to_string(),
                        ));
                    }
                }
            }
            if total < buf.len() {
                buf[total..].fill(0);
            }
            Ok(total)
        }
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        if self.readonly {
            return Err(SqliteError::readonly("attempt to write a readonly file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.write_all_at(buf, offset).map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_WRITE, e.to_string())
            })
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut clone = self.file.try_clone().map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_WRITE, e.to_string())
            })?;
            clone.seek(SeekFrom::Start(offset)).map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_WRITE, e.to_string())
            })?;
            clone.write_all(buf).map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_WRITE, e.to_string())
            })
        }
    }

    fn truncate(&self, size: u64) -> Result<()> {
        if self.readonly {
            return Err(SqliteError::readonly(
                "attempt to truncate a readonly file",
            ));
        }
        self.file.set_len(size).map_err(|e| {
            SqliteError::with_extended(extended::SQLITE_IOERR_TRUNCATE, e.to_string())
        })
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
        let res = match flags {
            SyncFlags::DataOnly => self.file.sync_data(),
            SyncFlags::Normal | SyncFlags::Full => self.file.sync_all(),
        };
        res.map_err(|e| SqliteError::with_extended(extended::SQLITE_IOERR_FSYNC, e.to_string()))
    }

    fn file_size(&self) -> Result<u64> {
        let meta = self
            .file
            .metadata()
            .map_err(|e| SqliteError::with_extended(extended::SQLITE_IOERR_FSTAT, e.to_string()))?;
        Ok(meta.len())
    }

    fn lock(&self, level: LockLevel) -> Result<()> {
        let mut guard = self
            .lock_level
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
        if level > *guard {
            *guard = level;
        }
        Ok(())
    }

    fn unlock(&self, level: LockLevel) -> Result<()> {
        let mut guard = self
            .lock_level
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
        if level < *guard {
            *guard = level;
        }
        Ok(())
    }

    fn lock_level(&self) -> LockLevel {
        self.lock_level
            .lock()
            .map(|g| *g)
            .unwrap_or(LockLevel::NoLock)
    }

    fn is_readonly(&self) -> bool {
        self.readonly
    }
}

/// Default OS filesystem VFS (`"unix"` on Unix platforms).
#[derive(Debug, Default, Clone, Copy)]
pub struct OsVfs;

impl OsVfs {
    pub const fn new() -> Self {
        Self
    }
}

impl Vfs for OsVfs {
    fn name(&self) -> &str {
        if cfg!(unix) { "unix" } else { "os" }
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        let target_path = match path {
            Some(p) => p.to_path_buf(),
            None => make_temp_file_path(),
        };

        let delete_on_close = flags.contains(OpenFlags::DELETEONCLOSE) || path.is_none();
        let mut readonly = flags.is_readonly();

        let file = if !readonly {
            let mut opts = OpenOptions::new();
            opts.read(true).write(true);
            if flags.contains(OpenFlags::CREATE) || path.is_none() {
                if flags.contains(OpenFlags::EXCLUSIVE) {
                    opts.create_new(true);
                } else {
                    opts.create(true);
                }
            }
            match opts.open(&target_path) {
                Ok(f) => f,
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied && !flags.contains(OpenFlags::CREATE) => {
                    // Fallback to read-only just like unixOpen in os_unix.c!
                    readonly = true;
                    OpenOptions::new()
                        .read(true)
                        .open(&target_path)
                        .map_err(|e| SqliteError::new(ErrorCode::CantOpen, e.to_string()))?
                }
                Err(err) => {
                    return Err(SqliteError::new(
                        ErrorCode::CantOpen,
                        format!("{}: {}", target_path.display(), err),
                    ));
                }
            }
        } else {
            OpenOptions::new()
                .read(true)
                .open(&target_path)
                .map_err(|err| {
                    SqliteError::new(
                        ErrorCode::CantOpen,
                        format!("{}: {}", target_path.display(), err),
                    )
                })?
        };

        Ok(Box::new(OsFile {
            file,
            path: Some(target_path),
            delete_on_close,
            readonly,
            lock_level: Mutex::new(LockLevel::NoLock),
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => {
                if sync_dir {
                    if let Some(parent) = path.parent() {
                        if let Ok(dir) = File::open(parent) {
                            let _ = dir.sync_all();
                        }
                    }
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_DELETE,
                e.to_string(),
            )),
        }
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        Ok(path.exists())
    }

    fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        let cwd = std::env::current_dir()
            .map_err(|e| SqliteError::new(ErrorCode::CantOpen, e.to_string()))?;
        Ok(cwd.join(path))
    }
}

fn fill_random_bytes(buf: &mut [u8]) {
    if let Ok(mut f) = File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    // SplitMix64 fallback seeded from system clock & stack address
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut state = nanos ^ (buf.as_ptr() as usize as u64);
    for chunk in buf.chunks_mut(8) {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        let bytes = z.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
}

fn make_temp_file_path() -> PathBuf {
    // Prefix "etilqs_" matches SQLITE_TEMP_FILE_PREFIX in sqlite/src/os.h!
    let mut rnd = [0u8; 10];
    fill_random_bytes(&mut rnd);
    let mut suffix = String::with_capacity(20);
    for b in rnd {
        suffix.push_str(&format!("{b:02x}"));
    }
    std::env::temp_dir().join(format!("etilqs_{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_vfs_read_write_short_read_zero_fill() {
        let vfs = MemoryVfs::new();
        let path = Path::new("test.db");
        let file = vfs
            .open(Some(path), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();

        file.write_at(b"hello", 0).unwrap();
        assert_eq!(file.file_size().unwrap(), 5);

        let mut buf = [0xffu8; 8];
        let n = file.read_at(&mut buf, 0).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello\0\0\0");

        file.truncate(2).unwrap();
        assert_eq!(file.file_size().unwrap(), 2);
        vfs.delete(path, false).unwrap();
        assert!(!vfs.exists(path).unwrap());
    }

    #[test]
    fn test_os_vfs_temp_file() {
        let vfs = OsVfs::new();
        let file = vfs
            .open(
                None,
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::DELETEONCLOSE,
            )
            .unwrap();
        file.write_at(b"SQLite format 3\0", 0).unwrap();
        let mut hdr = [0u8; 16];
        assert_eq!(file.read_at(&mut hdr, 0).unwrap(), 16);
        assert_eq!(&hdr, b"SQLite format 3\0");
    }
}
