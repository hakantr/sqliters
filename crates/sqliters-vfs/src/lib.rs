//! Virtual File System (`sqlite3_vfs` & `sqlite3_file`) ported from
//! `sqlite/src/os.h`, `sqlite/src/os.c`, `sqlite/src/os_unix.c`, and `sqlite/src/memdb.c`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqliters_types::{ErrorCode, Result, SqliteError, extended};

/// SQLite Julian Day millisecond offset for `1970-01-01 00:00:00 UTC`.
pub const UNIX_EPOCH_JD_MS: i64 = 210_866_760_000_000;

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

    /// Checks whether any handle (in this process or another process) holds a `RESERVED`,
    /// `PENDING`, or `EXCLUSIVE` lock on this database file (`xCheckReservedLock`).
    fn check_reserved_lock(&self) -> Result<bool> {
        Ok(self.lock_level() >= LockLevel::Reserved)
    }

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
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        UNIX_EPOCH_JD_MS + ms
    }

    /// Current UTC time as Unix epoch milliseconds derived from [`Vfs::current_time_int64`].
    fn current_unix_millis(&self) -> i64 {
        self.current_time_int64() - UNIX_EPOCH_JD_MS
    }
}

// ============================================================================
// In-Memory VFS (`memdb.c` port with full per-handle SQLite lock semantics)
// ============================================================================

static NEXT_MEM_HANDLE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct MemLockState {
    shared_holders: HashSet<u64>,
    writer: Option<(u64, LockLevel)>,
}

struct MemEntry {
    data: RwLock<Vec<u8>>,
    lock: Mutex<MemLockState>,
}

impl MemEntry {
    fn new() -> Self {
        Self {
            data: RwLock::new(Vec::new()),
            lock: Mutex::new(MemLockState::default()),
        }
    }
}

struct MemFile {
    id: u64,
    entry: Arc<MemEntry>,
    readonly: bool,
    held_lock: Mutex<LockLevel>,
}

impl Drop for MemFile {
    fn drop(&mut self) {
        let _ = self.unlock(LockLevel::NoLock);
    }
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
        if level == LockLevel::NoLock {
            return Ok(());
        }
        let mut held = self
            .held_lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        if *held >= level {
            return Ok(());
        }
        let mut state = self
            .entry
            .lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;

        // Ensure SHARED is acquired first if jumping from NoLock
        if *held == LockLevel::NoLock {
            if let Some((wid, wlvl)) = state.writer
                && wid != self.id
                && wlvl >= LockLevel::Pending
            {
                return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
            }
            state.shared_holders.insert(self.id);
            *held = LockLevel::Shared;
            if level == LockLevel::Shared {
                return Ok(());
            }
        }

        match level {
            LockLevel::NoLock | LockLevel::Shared => Ok(()),
            LockLevel::Reserved | LockLevel::Pending => {
                if let Some((wid, _)) = state.writer
                    && wid != self.id
                {
                    return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
                }
                state.writer = Some((self.id, level));
                *held = level;
                Ok(())
            }
            LockLevel::Exclusive => {
                if let Some((wid, _)) = state.writer
                    && wid != self.id
                {
                    return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
                }
                if *held == LockLevel::Reserved {
                    state.writer = Some((self.id, LockLevel::Pending));
                    *held = LockLevel::Pending;
                }
                if state.shared_holders.iter().any(|&hid| hid != self.id) {
                    return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
                }
                state.shared_holders.insert(self.id);
                state.writer = Some((self.id, LockLevel::Exclusive));
                *held = LockLevel::Exclusive;
                Ok(())
            }
        }
    }

    fn unlock(&self, level: LockLevel) -> Result<()> {
        let target = level.min(LockLevel::Shared);
        let mut held = self
            .held_lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        if *held <= target {
            return Ok(());
        }
        let mut state = self
            .entry
            .lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;

        if state.writer.is_some_and(|(wid, _)| wid == self.id) {
            state.writer = None;
        }
        if target == LockLevel::NoLock {
            state.shared_holders.remove(&self.id);
        } else {
            state.shared_holders.insert(self.id);
        }
        *held = target;
        Ok(())
    }

    fn check_reserved_lock(&self) -> Result<bool> {
        let state = self
            .entry
            .lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        Ok(state.writer.is_some())
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
            id: NEXT_MEM_HANDLE_ID.fetch_add(1, AtomicOrdering::Relaxed),
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
// POSIX Advisory Byte-Range Locking (`sqlite/src/os_unix.c` port)
// ============================================================================

#[cfg(unix)]
#[allow(unsafe_code)]
mod sys_unix {
    use std::os::fd::RawFd;
    use sqliters_types::{ErrorCode, PENDING_BYTE_OFFSET, Result, SqliteError, extended};

    pub const PENDING_BYTE: i64 = PENDING_BYTE_OFFSET as i64; // 0x4000_0000
    pub const RESERVED_BYTE: i64 = PENDING_BYTE + 1; // 0x4000_0001
    pub const SHARED_FIRST: i64 = PENDING_BYTE + 2; // 0x4000_0002
    pub const SHARED_SIZE: i64 = 510;

    const SEEK_SET: i16 = 0;

    #[cfg(target_os = "linux")]
    mod consts {
        pub const F_RDLCK: i16 = 0;
        pub const F_WRLCK: i16 = 1;
        pub const F_UNLCK: i16 = 2;
        pub const F_GETLK: i32 = 5;
        pub const F_SETLK: i32 = 6;
        pub const EAGAIN: i32 = 11;
        pub const EACCES: i32 = 13;
        pub const EINTR: i32 = 4;
    }

    #[cfg(target_os = "macos")]
    mod consts {
        pub const F_RDLCK: i16 = 1;
        pub const F_UNLCK: i16 = 2;
        pub const F_WRLCK: i16 = 3;
        pub const F_GETLK: i32 = 7;
        pub const F_SETLK: i32 = 8;
        pub const EACCES: i32 = 13;
        pub const EAGAIN: i32 = 35;
        pub const EINTR: i32 = 4;
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    mod consts {
        pub const F_RDLCK: i16 = 0;
        pub const F_WRLCK: i16 = 1;
        pub const F_UNLCK: i16 = 2;
        pub const F_GETLK: i32 = 5;
        pub const F_SETLK: i32 = 6;
        pub const EAGAIN: i32 = 11;
        pub const EACCES: i32 = 13;
        pub const EINTR: i32 = 4;
    }

    pub use consts::{F_RDLCK, F_UNLCK, F_WRLCK};

    #[cfg(not(target_os = "macos"))]
    #[repr(C)]
    struct PosixFlock {
        l_type: i16,
        l_whence: i16,
        l_start: i64,
        l_len: i64,
        l_pid: i32,
    }

    #[cfg(target_os = "macos")]
    #[repr(C)]
    struct PosixFlock {
        l_start: i64,
        l_len: i64,
        l_pid: i32,
        l_type: i16,
        l_whence: i16,
    }

    unsafe extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }

    pub fn set_posix_lock(
        fd: RawFd,
        l_type: i16,
        l_start: i64,
        l_len: i64,
        fallback_ioerr: i32,
    ) -> Result<()> {
        loop {
            let mut fl = PosixFlock {
                l_type,
                l_whence: SEEK_SET,
                l_start,
                l_len,
                l_pid: 0,
            };
            // SAFETY: `fl` is a valid, properly aligned `struct flock` passed to POSIX `fcntl(F_SETLK)`.
            let rc = unsafe { fcntl(fd, consts::F_SETLK, &mut fl as *mut PosixFlock) };
            if rc == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(e) if e == consts::EINTR => continue,
                Some(e) if e == consts::EACCES || e == consts::EAGAIN => {
                    return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
                }
                _ => {
                    return Err(SqliteError::with_extended(fallback_ioerr, err.to_string()));
                }
            }
        }
    }

    pub fn check_reserved_lock(fd: RawFd) -> Result<bool> {
        loop {
            let mut fl = PosixFlock {
                l_type: F_WRLCK,
                l_whence: SEEK_SET,
                l_start: RESERVED_BYTE,
                l_len: 1,
                l_pid: 0,
            };
            // SAFETY: `fl` is a valid, properly aligned `struct flock` passed to POSIX `fcntl(F_GETLK)`.
            let rc = unsafe { fcntl(fd, consts::F_GETLK, &mut fl as *mut PosixFlock) };
            if rc == 0 {
                return Ok(fl.l_type != F_UNLCK);
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(consts::EINTR) {
                continue;
            }
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_CHECKRESERVEDLOCK,
                err.to_string(),
            ));
        }
    }
}

/// Process-wide per-inode lock state (`unixInodeInfo` in `sqlite/src/os_unix.c`).
///
/// POSIX `fcntl` advisory locks are owned by `(pid, inode)` rather than per file descriptor,
/// and closing *any* file descriptor for that inode in this process drops all kernel `fcntl`
/// locks held by this PID on that inode. We therefore track in-process lock state per
/// `(st_dev, st_ino)` and defer closing file descriptors while `n_lock > 0` (`pending_close_fds`).
#[cfg(unix)]
#[derive(Default)]
struct UnixInodeInfo {
    e_file_lock: LockLevel,
    n_shared: usize,
    n_lock: usize,
    pending_close_fds: Vec<File>,
}

#[cfg(unix)]
fn inode_table() -> &'static Mutex<HashMap<(u64, u64), Arc<Mutex<UnixInodeInfo>>>> {
    static TABLE: OnceLock<Mutex<HashMap<(u64, u64), Arc<Mutex<UnixInodeInfo>>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(unix)]
fn lookup_inode_info(key: (u64, u64)) -> Arc<Mutex<UnixInodeInfo>> {
    let mut map = inode_table().lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        map.entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(UnixInodeInfo::default()))),
    )
}

// ============================================================================
// Standard OS Filesystem VFS (`os_unix.c` / cross-platform Rust port)
// ============================================================================

struct OsFile {
    file: Option<File>,
    path: Option<PathBuf>,
    delete_on_close: bool,
    readonly: bool,
    lock_level: Mutex<LockLevel>,
    #[cfg(unix)]
    inode_key: (u64, u64),
    #[cfg(unix)]
    inode_info: Arc<Mutex<UnixInodeInfo>>,
}

impl OsFile {
    #[inline]
    fn file_ref(&self) -> Result<&File> {
        self.file
            .as_ref()
            .ok_or_else(|| SqliteError::misuse("file handle already closed"))
    }
}

impl Drop for OsFile {
    fn drop(&mut self) {
        let _ = self.unlock(LockLevel::NoLock);

        #[cfg(unix)]
        if let Some(f) = self.file.take() {
            let mut deferred = false;
            if let Ok(mut inode) = self.inode_info.lock()
                && inode.n_lock > 0
            {
                inode.pending_close_fds.push(f);
                deferred = true;
            }
            if !deferred {
                let mut map = inode_table().lock().unwrap_or_else(|e| e.into_inner());
                // Strong count 2 means only `self.inode_info` and `map` reference this entry.
                if Arc::strong_count(&self.inode_info) == 2 {
                    map.remove(&self.inode_key);
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = self.file.take();
        }

        if self.delete_on_close
            && let Some(ref p) = self.path
        {
            let _ = fs::remove_file(p);
        }
    }
}

impl VfsFile for OsFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let file = self.file_ref()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            let mut total = 0usize;
            while total < buf.len() {
                match file.read_at(&mut buf[total..], offset + total as u64) {
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
            let mut clone = file.try_clone().map_err(|e| {
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
        let file = self.file_ref()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            file.write_all_at(buf, offset).map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_WRITE, e.to_string())
            })
        }
        #[cfg(not(unix))]
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut clone = file.try_clone().map_err(|e| {
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
        self.file_ref()?.set_len(size).map_err(|e| {
            SqliteError::with_extended(extended::SQLITE_IOERR_TRUNCATE, e.to_string())
        })
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
        let file = self.file_ref()?;
        let res = match flags {
            SyncFlags::DataOnly => file.sync_data(),
            SyncFlags::Normal | SyncFlags::Full => file.sync_all(),
        };
        res.map_err(|e| SqliteError::with_extended(extended::SQLITE_IOERR_FSYNC, e.to_string()))
    }

    fn file_size(&self) -> Result<u64> {
        let meta = self
            .file_ref()?
            .metadata()
            .map_err(|e| SqliteError::with_extended(extended::SQLITE_IOERR_FSTAT, e.to_string()))?;
        Ok(meta.len())
    }

    fn lock(&self, level: LockLevel) -> Result<()> {
        if level == LockLevel::NoLock {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let fd = self.file_ref()?.as_raw_fd();

            // Follow SQLite's transition rule: always enter SHARED before RESERVED/EXCLUSIVE.
            {
                let file_lock = self
                    .lock_level
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
                if *file_lock >= level {
                    return Ok(());
                }
                if *file_lock == LockLevel::NoLock && level > LockLevel::Shared {
                    drop(file_lock);
                    self.lock(LockLevel::Shared)?;
                }
            }

            let mut file_lock = self
                .lock_level
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
            if *file_lock >= level {
                return Ok(());
            }

            let mut inode = self
                .inode_info
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode lock"))?;

            // In-process conflict check (`unixLock` lines 1946-1951 in `os_unix.c`)
            if *file_lock != inode.e_file_lock
                && (inode.e_file_lock >= LockLevel::Pending || level > LockLevel::Shared)
            {
                return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
            }

            // In-process shared lock reuse (`unixLock` lines 1957-1966)
            if level == LockLevel::Shared
                && (inode.e_file_lock == LockLevel::Shared
                    || inode.e_file_lock == LockLevel::Reserved)
            {
                *file_lock = LockLevel::Shared;
                inode.n_shared += 1;
                inode.n_lock += 1;
                return Ok(());
            }

            if level == LockLevel::Exclusive && inode.n_shared > 1 {
                return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
            }

            // Acquire PENDING_BYTE when entering SHARED or upgrading SHARED/RESERVED -> EXCLUSIVE
            if level == LockLevel::Shared
                || (level == LockLevel::Exclusive
                    && (*file_lock == LockLevel::Shared || *file_lock == LockLevel::Reserved))
            {
                let l_type = if level == LockLevel::Shared {
                    sys_unix::F_RDLCK
                } else {
                    sys_unix::F_WRLCK
                };
                sys_unix::set_posix_lock(
                    fd,
                    l_type,
                    sys_unix::PENDING_BYTE,
                    1,
                    extended::SQLITE_IOERR_LOCK,
                )?;
                if level == LockLevel::Exclusive {
                    *file_lock = LockLevel::Pending;
                    inode.e_file_lock = LockLevel::Pending;
                }
            }

            if level == LockLevel::Shared {
                let shared_res = sys_unix::set_posix_lock(
                    fd,
                    sys_unix::F_RDLCK,
                    sys_unix::SHARED_FIRST,
                    sys_unix::SHARED_SIZE,
                    extended::SQLITE_IOERR_LOCK,
                );
                let unlock_pending_res = sys_unix::set_posix_lock(
                    fd,
                    sys_unix::F_UNLCK,
                    sys_unix::PENDING_BYTE,
                    1,
                    extended::SQLITE_IOERR_UNLOCK,
                );
                shared_res?;
                unlock_pending_res?;

                *file_lock = LockLevel::Shared;
                inode.e_file_lock = LockLevel::Shared;
                inode.n_lock += 1;
                inode.n_shared = 1;
                return Ok(());
            }

            let (start, len) = match level {
                LockLevel::Reserved => (sys_unix::RESERVED_BYTE, 1),
                LockLevel::Pending => (sys_unix::PENDING_BYTE, 1),
                LockLevel::Exclusive => (sys_unix::SHARED_FIRST, sys_unix::SHARED_SIZE),
                LockLevel::NoLock | LockLevel::Shared => unreachable!(),
            };
            sys_unix::set_posix_lock(
                fd,
                sys_unix::F_WRLCK,
                start,
                len,
                extended::SQLITE_IOERR_LOCK,
            )?;

            *file_lock = level;
            inode.e_file_lock = level;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let mut guard = self
                .lock_level
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
            if level > *guard {
                *guard = level;
            }
            Ok(())
        }
    }

    fn unlock(&self, level: LockLevel) -> Result<()> {
        let target = level.min(LockLevel::Shared);
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let mut file_lock = self
                .lock_level
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
            if *file_lock <= target {
                return Ok(());
            }
            let fd = self.file_ref()?.as_raw_fd();
            let mut inode = self
                .inode_info
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode lock"))?;

            if *file_lock > LockLevel::Shared {
                if target == LockLevel::Shared {
                    sys_unix::set_posix_lock(
                        fd,
                        sys_unix::F_RDLCK,
                        sys_unix::SHARED_FIRST,
                        sys_unix::SHARED_SIZE,
                        extended::SQLITE_IOERR_RDLOCK,
                    )?;
                }
                // Release both PENDING_BYTE and RESERVED_BYTE (contiguous 2-byte range)
                sys_unix::set_posix_lock(
                    fd,
                    sys_unix::F_UNLCK,
                    sys_unix::PENDING_BYTE,
                    2,
                    extended::SQLITE_IOERR_UNLOCK,
                )?;
                inode.e_file_lock = LockLevel::Shared;
                *file_lock = LockLevel::Shared;
            }

            if target == LockLevel::NoLock {
                inode.n_shared = inode.n_shared.saturating_sub(1);
                let mut unlock_res = Ok(());
                if inode.n_shared == 0 {
                    unlock_res = sys_unix::set_posix_lock(
                        fd,
                        sys_unix::F_UNLCK,
                        0,
                        0,
                        extended::SQLITE_IOERR_UNLOCK,
                    );
                    inode.e_file_lock = LockLevel::NoLock;
                }
                inode.n_lock = inode.n_lock.saturating_sub(1);
                if inode.n_lock == 0 {
                    inode.pending_close_fds.clear();
                }
                *file_lock = LockLevel::NoLock;
                unlock_res?;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let mut guard = self
                .lock_level
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
            if target < *guard {
                *guard = target;
            }
            Ok(())
        }
    }

    fn check_reserved_lock(&self) -> Result<bool> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let fd = self.file_ref()?.as_raw_fd();
            let inode = self
                .inode_info
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode lock"))?;
            if inode.e_file_lock > LockLevel::Shared {
                return Ok(true);
            }
            sys_unix::check_reserved_lock(fd)
        }
        #[cfg(not(unix))]
        {
            Ok(self.lock_level() >= LockLevel::Reserved)
        }
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
                Err(err)
                    if err.kind() == std::io::ErrorKind::PermissionDenied
                        && !flags.contains(OpenFlags::CREATE) =>
                {
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

        #[cfg(unix)]
        let (inode_key, inode_info) = {
            use std::os::unix::fs::MetadataExt;
            let meta = file.metadata().map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_FSTAT, e.to_string())
            })?;
            let key = (meta.dev(), meta.ino());
            (key, lookup_inode_info(key))
        };

        Ok(Box::new(OsFile {
            file: Some(file),
            path: Some(target_path),
            delete_on_close,
            readonly,
            lock_level: Mutex::new(LockLevel::NoLock),
            #[cfg(unix)]
            inode_key,
            #[cfg(unix)]
            inode_info,
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => {
                if sync_dir
                    && let Some(parent) = path.parent()
                    && let Ok(dir) = File::open(parent)
                {
                    let _ = dir.sync_all();
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
    if let Ok(mut f) = File::open("/dev/urandom")
        && f.read_exact(buf).is_ok()
    {
        return;
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
    fn test_memory_vfs_lock_conflicts() {
        let vfs = MemoryVfs::new();
        let path = Path::new("locks.db");
        let h1 = vfs
            .open(Some(path), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();
        let h2 = vfs.open(Some(path), OpenFlags::READWRITE).unwrap();

        // Both handles can hold SHARED simultaneously
        h1.lock(LockLevel::Shared).unwrap();
        h2.lock(LockLevel::Shared).unwrap();
        assert!(!h2.check_reserved_lock().unwrap());

        // h1 upgrades to RESERVED; h2 sees reserved lock and cannot also take RESERVED
        h1.lock(LockLevel::Reserved).unwrap();
        assert!(h2.check_reserved_lock().unwrap());
        assert_eq!(h2.lock(LockLevel::Reserved).unwrap_err().code, ErrorCode::Busy);

        // h1 cannot upgrade to EXCLUSIVE while h2 still holds SHARED (leaves h1 in PENDING)
        assert_eq!(h1.lock(LockLevel::Exclusive).unwrap_err().code, ErrorCode::Busy);
        assert_eq!(h1.lock_level(), LockLevel::Pending);

        // Once h2 releases SHARED, h1 acquires EXCLUSIVE
        h2.unlock(LockLevel::NoLock).unwrap();
        h1.lock(LockLevel::Exclusive).unwrap();
        assert_eq!(h2.lock(LockLevel::Shared).unwrap_err().code, ErrorCode::Busy);

        // Downgrading h1 to SHARED allows h2 to acquire SHARED again
        h1.unlock(LockLevel::Shared).unwrap();
        h2.lock(LockLevel::Shared).unwrap();
    }

    #[test]
    fn test_os_vfs_temp_file_and_in_process_locks() {
        let vfs = OsVfs::new();
        let tmp = make_temp_file_path();
        let h1 = vfs
            .open(Some(&tmp), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();
        let h2 = vfs.open(Some(&tmp), OpenFlags::READWRITE).unwrap();

        h1.write_at(b"SQLite format 3\0", 0).unwrap();
        let mut hdr = [0u8; 16];
        assert_eq!(h2.read_at(&mut hdr, 0).unwrap(), 16);
        assert_eq!(&hdr, b"SQLite format 3\0");

        h1.lock(LockLevel::Shared).unwrap();
        h1.lock(LockLevel::Reserved).unwrap();
        h2.lock(LockLevel::Shared).unwrap();
        assert!(h2.check_reserved_lock().unwrap());
        assert_eq!(h2.lock(LockLevel::Reserved).unwrap_err().code, ErrorCode::Busy);

        // Dropping h2 while h1 holds RESERVED must NOT release h1's lock
        h2.unlock(LockLevel::NoLock).unwrap();
        drop(h2);
        assert!(h1.check_reserved_lock().unwrap());

        h1.unlock(LockLevel::NoLock).unwrap();
        drop(h1);
        vfs.delete(&tmp, false).unwrap();
    }
}
