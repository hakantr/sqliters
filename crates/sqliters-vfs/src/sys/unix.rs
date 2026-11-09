//! Unix/POSIX FFI boundary for advisory byte-range locking (`fcntl`), memory-mapped
//! WAL-index views (`mmap`/`munmap`), component-by-component pathname/symlink resolution
//! (`unixFullPathname`), durability (`fsync`/`fdatasync`), and explicit close error reporting.
#![allow(unsafe_code)]

#[cfg(not(all(
    target_pointer_width = "64",
    target_has_atomic = "16",
    target_has_atomic = "32"
)))]
compile_error!(
    "sqliters-vfs Unix sys backend requires 64-bit pointer width and native 16/32-bit atomics"
);

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!(
    "sqliters-vfs Unix sys backend is currently verified only on 64-bit Linux and macOS"
);

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering, fence};
use std::sync::{Arc, Mutex, OnceLock};

use sqliters_types::{ErrorCode, Result, SqliteError, extended};

const SEEK_SET: i16 = 0;

/// Maximum pathname length for the Unix VFS (`MAX_PATHNAME` in `sqlite/src/os_unix.c:180`).
pub(crate) const MAX_PATHNAME: usize = 512;

/// Maximum symlink expansions allowed during pathname resolution (`SQLITE_MAX_SYMLINK` in `sqlite/src/os.h:46`).
pub(crate) const SQLITE_MAX_SYMLINK: usize = 200;

/// Byte offset of the Deadman Switch lock in `-shm` (`UNIX_SHM_DMS` in `sqlite/src/os_unix.c:4674`).
pub(crate) const UNIX_SHM_DMS: i64 = 128;

/// Base byte offset of `WalCkptInfo.aLock[0..8]` in `-shm` (`UNIX_SHM_BASE` in `sqlite/src/os_unix.c:4673`).
pub(crate) const UNIX_SHM_BASE: i64 = 120;

const WAL_LOCK_BYTES_START: usize = 120;
const WAL_LOCK_BYTES_END: usize = 128;
const SHM_LOGICAL_REGION_BYTES: usize = 32_768;

#[cfg(target_os = "linux")]
mod platform {
    pub(crate) const F_RDLCK: i16 = 0;
    pub(crate) const F_WRLCK: i16 = 1;
    pub(crate) const F_UNLCK: i16 = 2;
    pub(crate) const F_GETLK: i32 = 5;
    pub(crate) const F_SETLK: i32 = 6;

    pub(crate) const EINTR: i32 = 4;
    pub(crate) const EAGAIN: i32 = 11;
    pub(crate) const EACCES: i32 = 13;
    pub(crate) const ENOSPC: i32 = 28;

    pub(crate) const PROT_READ: i32 = 0x01;
    pub(crate) const PROT_WRITE: i32 = 0x02;
    pub(crate) const MAP_SHARED: i32 = 0x01;
    pub(crate) const SC_PAGESIZE: i32 = 30;

    /// Linux `<bits/fcntl.h>` 64-bit `struct flock`.
    #[repr(C)]
    pub(crate) struct PosixFlock {
        pub(crate) l_type: i16,
        pub(crate) l_whence: i16,
        pub(crate) l_start: i64,
        pub(crate) l_len: i64,
        pub(crate) l_pid: i32,
    }

    impl PosixFlock {
        #[inline]
        pub(crate) const fn new(l_type: i16, l_whence: i16, l_start: i64, l_len: i64) -> Self {
            Self {
                l_type,
                l_whence,
                l_start,
                l_len,
                l_pid: 0,
            }
        }
    }
}

#[cfg(target_os = "macos")]
use super::macos as platform;

use platform::{
    EACCES, EAGAIN, EINTR, ENOSPC, F_GETLK, F_SETLK, MAP_SHARED, PROT_READ, PROT_WRITE, PosixFlock,
    SC_PAGESIZE,
};
pub(crate) use platform::{F_RDLCK, F_UNLCK, F_WRLCK};

const MAP_FAILED_ADDR: usize = usize::MAX;

#[cfg(target_os = "linux")]
#[repr(C, align(8))]
struct LinuxStatfsBuf([usize; 32]);

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn ftruncate(fd: i32, length: i64) -> i32;
    fn close(fd: i32) -> i32;
    fn fsync(fd: i32) -> i32;
    #[cfg(target_os = "linux")]
    fn fdatasync(fd: i32) -> i32;
    fn mmap(
        addr: *mut core::ffi::c_void,
        length: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut core::ffi::c_void;
    fn munmap(addr: *mut core::ffi::c_void, length: usize) -> i32;
    fn sysconf(name: i32) -> isize;
    #[cfg(target_os = "linux")]
    fn fstatfs(fd: i32, buf: *mut LinuxStatfsBuf) -> i32;
}

/// Applies a non-blocking POSIX advisory byte-range lock (`F_SETLK`) on `fd`.
pub(crate) fn set_posix_lock(
    fd: RawFd,
    l_type: i16,
    l_start: i64,
    l_len: i64,
    fallback_ioerr: i32,
) -> Result<()> {
    if l_start < 0 || l_len < 0 {
        return Err(SqliteError::misuse("negative POSIX lock offset or length"));
    }
    loop {
        let mut fl = PosixFlock::new(l_type, SEEK_SET, l_start, l_len);
        // SAFETY: `fl` is a stack-owned, fully initialized, naturally aligned `#[repr(C)]`
        // `PosixFlock` valid for read/write for the duration of this `fcntl(F_SETLK)` call;
        // `fd` is an open file descriptor kept alive by the caller; `F_SETLK` is non-blocking
        // and safe across Rust threads and cross-process C SQLite 3.54.1 advisory locks; on
        // error no memory ownership changes and `EINTR` is safely retried.
        let rc = unsafe { fcntl(fd, F_SETLK, &mut fl as *mut PosixFlock) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(e) if e == EINTR => continue,
            Some(e) if e == EACCES || e == EAGAIN => {
                return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
            }
            _ => {
                return Err(SqliteError::with_extended(fallback_ioerr, err.to_string()));
            }
        }
    }
}

/// Queries POSIX advisory byte-range lock state (`F_GETLK`) on `[l_start, l_start + l_len)`,
/// returning the conflicting `l_type` (`F_UNLCK`, `F_RDLCK`, or `F_WRLCK`).
///
/// Following `unixLockSharedMemory` and `unixCheckReservedLock` in `sqlite/src/os_unix.c`,
/// probes with `F_WRLCK` so BOTH foreign `F_RDLCK` and foreign `F_WRLCK` holders are reported!
pub(crate) fn get_posix_lock_type(
    fd: RawFd,
    l_start: i64,
    l_len: i64,
    fallback_ioerr: i32,
) -> Result<i16> {
    if l_start < 0 || l_len <= 0 {
        return Err(SqliteError::misuse("invalid F_GETLK byte range"));
    }
    loop {
        let mut fl = PosixFlock::new(F_WRLCK, SEEK_SET, l_start, l_len);
        // SAFETY: `fl` is a stack-owned, fully initialized, naturally aligned `#[repr(C)]`
        // `PosixFlock` valid for kernel read/write during `fcntl(F_GETLK)`; `fd` is a valid
        // open file descriptor kept alive by the caller; `F_GETLK` does not mutate file state
        // or block, is safe concurrently with C SQLite processes, and safely retries `EINTR`.
        let rc = unsafe { fcntl(fd, F_GETLK, &mut fl as *mut PosixFlock) };
        if rc == 0 {
            return Ok(fl.l_type);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(EINTR) {
            continue;
        }
        return Err(SqliteError::with_extended(fallback_ioerr, err.to_string()));
    }
}

/// Checks whether another process holds a lock on `reserved_byte_offset`.
pub(crate) fn check_reserved_lock(fd: RawFd, reserved_byte_offset: u64) -> Result<bool> {
    let offset = i64::try_from(reserved_byte_offset)
        .map_err(|_| SqliteError::misuse("reserved_byte_offset overflows i64"))?;
    let l_type = get_posix_lock_type(fd, offset, 1, extended::SQLITE_IOERR_CHECKRESERVEDLOCK)?;
    Ok(l_type != F_UNLCK)
}

/// Reads from `file` at `offset` until `buf` is filled or EOF is reached, zero-filling any
/// unread tail (`unixRead` in `sqlite/src/os_unix.c`).
pub(crate) fn pread_until_eof(file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
    let mut total = 0usize;
    while total < buf.len() {
        let cur_off = offset.checked_add(total as u64).ok_or_else(|| {
            SqliteError::with_extended(extended::SQLITE_IOERR_READ, "read offset overflow")
        })?;
        match file.read_at(&mut buf[total..], cur_off) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
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

/// Writes all of `buf` at `offset` (`seekAndWriteFd` in `sqlite/src/os_unix.c`), looping across
/// partial writes and `EINTR`, translating `ENOSPC` to `SQLITE_FULL`, and reporting persistent
/// write errors with `ioerr_code` (e.g. `SQLITE_IOERR_WRITE` or `SQLITE_IOERR_SHMSIZE`).
pub(crate) fn pwrite_all(file: &File, buf: &[u8], offset: u64, ioerr_code: i32) -> Result<()> {
    let mut written = 0usize;
    while written < buf.len() {
        let cur_off = offset
            .checked_add(written as u64)
            .ok_or_else(|| SqliteError::with_extended(ioerr_code, "write offset overflow"))?;
        match file.write_at(&buf[written..], cur_off) {
            Ok(0) => {
                return Err(SqliteError::with_extended(
                    ioerr_code,
                    "pwrite returned 0 bytes written",
                ));
            }
            Ok(n) => written += n,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                if e.raw_os_error() == Some(ENOSPC) {
                    return Err(SqliteError::new(ErrorCode::Full, e.to_string()));
                }
                return Err(SqliteError::with_extended(ioerr_code, e.to_string()));
            }
        }
    }
    Ok(())
}

/// Resizes `fd` to `size` bytes via `ftruncate`, retrying `EINTR` and mapping `ENOSPC` to `SQLITE_FULL`.
pub(crate) fn ftruncate_fd(fd: RawFd, size: u64, ioerr_code: i32) -> Result<()> {
    let len = i64::try_from(size)
        .map_err(|_| SqliteError::with_extended(ioerr_code, "ftruncate length overflows i64"))?;
    loop {
        // SAFETY: `fd` is a valid open writable file descriptor borrowed from the caller, and
        // `len >= 0` was validated via `i64::try_from`; `ftruncate` touches no user pointers,
        // is safe across threads when coordinated by SQLite/DMS locks, and safely retries `EINTR`.
        let rc = unsafe { ftruncate(fd, len) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(e) if e == EINTR => continue,
            Some(e) if e == ENOSPC => {
                return Err(SqliteError::new(ErrorCode::Full, err.to_string()));
            }
            _ => return Err(SqliteError::with_extended(ioerr_code, err.to_string())),
        }
    }
}

/// Flushes `file` to persistent storage (`unixSync` / `full_fsync` in `sqlite/src/os_unix.c`).
pub(crate) fn sync_fd(file: &File, full: bool, data_only: bool) -> Result<()> {
    let fd = file.as_raw_fd();
    #[cfg(target_os = "macos")]
    {
        return platform::sync_fd_macos(fd, full, data_only);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = full;
        loop {
            // SAFETY: `fd` is a valid open file descriptor borrowed from `file` for the duration
            // of this syscall; `fdatasync` and `fsync` touch no user-space memory, are thread-
            // and process-safe, and safely retry `EINTR`.
            let rc = unsafe { if data_only { fdatasync(fd) } else { fsync(fd) } };
            if rc == 0 {
                return Ok(());
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(EINTR) {
                continue;
            }
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_FSYNC,
                err.to_string(),
            ));
        }
    }
}

/// Flushes a parent directory entry after file creation or deletion (`unixSync` / `unixDelete`
/// in `sqlite/src/os_unix.c`).
pub(crate) fn sync_directory(dir_path: &Path) -> Result<()> {
    let dir = match File::open(dir_path) {
        Ok(d) => d,
        // Mirrors `openDirectory` in `os_unix.c` (`lines 3945-3952, 6859-6868`) where an
        // unopenable parent directory (`ENOENT` or `EACCES` in restricted sandboxes) maps to
        // `SQLITE_CANTOPEN` and is treated as benign, while real `fsync` errors return
        // `SQLITE_IOERR_DIR_FSYNC`.
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied) => {
            return Ok(());
        }
        Err(e) => {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_DIR_FSYNC,
                e.to_string(),
            ));
        }
    };
    let fd = dir.as_raw_fd();
    let sync_res = loop {
        // SAFETY: `fd` is a valid open directory descriptor owned by `dir`; `fsync` touches no
        // user memory, is thread-safe, and safely retries `EINTR`.
        let rc = unsafe { fsync(fd) };
        if rc == 0 {
            break Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(EINTR) {
            continue;
        }
        break Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_DIR_FSYNC,
            err.to_string(),
        ));
    };
    let close_res = close_file_fd(dir);
    sync_res?;
    if let Err(e) = close_res {
        return Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_DIR_CLOSE,
            e.message,
        ));
    }
    Ok(())
}

/// Explicitly closes `file` and surfaces any OS close error (`SQLITE_IOERR_CLOSE`), never
/// retrying `close()` on `EINTR` (since Linux and Darwin deallocate the FD table entry before
/// returning `EINTR`, so retrying could close a concurrently opened descriptor).
pub(crate) fn close_file_fd(file: File) -> Result<()> {
    let fd = file.into_raw_fd();
    // SAFETY: `into_raw_fd()` transferred unique ownership of the open OS file descriptor `fd`
    // out of `File` without running `Drop`. We invoke `close(fd)` exactly once and never touch
    // `fd` afterward, preventing double-close or concurrent FD-reuse races across threads.
    let rc = unsafe { close(fd) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(EINTR) {
        return Ok(());
    }
    Err(SqliteError::with_extended(
        extended::SQLITE_IOERR_CLOSE,
        err.to_string(),
    ))
}

/// Returns the OS virtual-memory page size (`sysconf(_SC_PAGESIZE)`), guaranteed to be a power
/// of two `>= 4096`.
pub(crate) fn os_page_size() -> usize {
    static PAGE_SIZE: OnceLock<usize> = OnceLock::new();
    *PAGE_SIZE.get_or_init(|| {
        // SAFETY: `sysconf(SC_PAGESIZE)` reads a process-constant kernel configuration value,
        // takes no pointers, mutates no state, and is thread-safe.
        let raw = unsafe { sysconf(SC_PAGESIZE) };
        let sz = usize::try_from(raw).unwrap_or(4096);
        if sz >= 4096 && sz.is_power_of_two() {
            sz
        } else {
            4096
        }
    })
}

/// Best-effort verification that `fd` resides on a local filesystem suitable for shared
/// memory-mapped WAL-index access (`implementation_plan.md` line 30 & line 92).
pub(crate) fn verify_local_filesystem_for_shm(fd: RawFd) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return platform::verify_local_filesystem_macos(fd);
    }
    #[cfg(target_os = "linux")]
    {
        let mut buf = LinuxStatfsBuf([0usize; 32]);
        loop {
            // SAFETY: `buf` is a 256-byte, 8-byte-aligned stack buffer (`>= sizeof(struct statfs)`
            // which is 120 bytes on 64-bit Linux), valid for kernel writes during `fstatfs`;
            // `fd` is a valid open descriptor borrowed from the caller; no external aliases exist.
            let rc = unsafe { fstatfs(fd, &mut buf as *mut LinuxStatfsBuf) };
            if rc == 0 {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(EINTR) {
                continue;
            }
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHMOPEN,
                err.to_string(),
            ));
        }

        // First field of Linux `struct statfs` (`__fsword_t f_type`) at byte offset 0.
        let f_type = (buf.0[0] as u64) & 0xffff_ffff;
        const REMOTE_FS_MAGICS: &[u64] = &[
            0x0000_6969, // NFS_SUPER_MAGIC
            0x0000_517b, // SMB_SUPER_MAGIC
            0xfe53_4d42, // SMB2_SUPER_MAGIC
            0xff53_4d42, // CIFS_MAGIC_NUMBER
            0x5346_414f, // AFS_SUPER_MAGIC
            0x6b41_4653, // kAFS_SUPER_MAGIC
            0x7375_7245, // CODA_SUPER_MAGIC
            0x0000_564c, // NCP_SUPER_MAGIC
            0x00c3_6400, // CEPH_SUPER_MAGIC
            0x0116_1970, // GFS2_MAGIC
            0x7461_636f, // OCFS2_SUPER_MAGIC
            0x0bd0_0bd0, // LUSTRE_SUPER_MAGIC
        ];
        if REMOTE_FS_MAGICS.contains(&f_type) {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHMOPEN,
                format!("shared WAL-index is not supported on network filesystem ({f_type:#x})"),
            ));
        }
        Ok(())
    }
}

struct DbPathBuilder {
    out: Vec<u8>,
    n_symlink: usize,
    err: Option<SqliteError>,
}

impl DbPathBuilder {
    fn new() -> Self {
        Self {
            out: Vec::with_capacity(MAX_PATHNAME),
            n_symlink: 0,
            err: None,
        }
    }

    fn append_one_element(&mut self, elem: &[u8]) {
        if self.err.is_some() || elem.is_empty() {
            return;
        }
        if elem == b"." {
            return;
        }
        if elem == b".." {
            if self.out.len() > 1 {
                debug_assert_eq!(self.out[0], b'/');
                while let Some(b) = self.out.pop() {
                    if b == b'/' {
                        break;
                    }
                }
            }
            return;
        }

        // Matches `if (pPath->nUsed + nName + 2 >= pPath->nOut)` in `os_unix.c:6943`
        // (`unixFullPathname` returns `SQLITE_CANTOPEN_BKPT` at `os_unix.c:7033`).
        if self.out.len().saturating_add(elem.len()).saturating_add(2) > MAX_PATHNAME {
            self.err = Some(SqliteError::new(
                ErrorCode::CantOpen,
                "unable to open database file: pathname exceeds MAX_PATHNAME",
            ));
            return;
        }

        self.out.push(b'/');
        self.out.extend_from_slice(elem);

        let current = Path::new(OsStr::from_bytes(&self.out));
        match fs::symlink_metadata(current) {
            Err(e) => {
                if e.kind() != ErrorKind::NotFound {
                    self.err = Some(SqliteError::new(
                        ErrorCode::CantOpen,
                        format!("unable to open database file: {e}"),
                    ));
                }
            }
            Ok(meta) if meta.file_type().is_symlink() => {
                let prev_symlinks = self.n_symlink;
                self.n_symlink += 1;
                if prev_symlinks > SQLITE_MAX_SYMLINK {
                    self.err = Some(SqliteError::new(
                        ErrorCode::CantOpen,
                        "unable to open database file: too many symbolic links",
                    ));
                    return;
                }
                let link_target = match fs::read_link(current) {
                    Ok(t) => t,
                    Err(e) => {
                        self.err = Some(SqliteError::new(
                            ErrorCode::CantOpen,
                            format!("unable to open database file: {e}"),
                        ));
                        return;
                    }
                };
                let target_bytes = link_target.as_os_str().as_bytes();
                if target_bytes.is_empty()
                    || target_bytes.contains(&0)
                    || target_bytes.len() >= 4096
                {
                    self.err = Some(SqliteError::new(
                        ErrorCode::CantOpen,
                        "unable to open database file: invalid symlink target",
                    ));
                    return;
                }
                if target_bytes[0] == b'/' {
                    self.out.clear();
                } else {
                    self.out.truncate(self.out.len() - (elem.len() + 1));
                }
                self.append_all_elements(target_bytes);
            }
            Ok(_) => {}
        }
    }

    fn append_all_elements(&mut self, raw_path: &[u8]) {
        for elem in raw_path.split(|&b| b == b'/') {
            if !elem.is_empty() {
                self.append_one_element(elem);
                if self.err.is_some() {
                    return;
                }
            }
        }
    }
}

/// Resolves `path` into a canonicalized SQLite pathname matching `unixFullPathname` in
/// `sqlite/src/os_unix.c:7011-7036`, returning `(resolved_path, traversed_any_symlink)`.
pub(crate) fn unix_full_pathname(path: &Path) -> Result<(PathBuf, bool)> {
    let raw = path.as_os_str().as_bytes();
    if raw.is_empty() || raw.contains(&0) {
        return Err(SqliteError::new(
            ErrorCode::CantOpen,
            "unable to open database file",
        ));
    }

    let mut builder = DbPathBuilder::new();
    if raw[0] != b'/' {
        let cwd = std::env::current_dir().map_err(|e| {
            SqliteError::new(
                ErrorCode::CantOpen,
                format!("unable to open database file: {e}"),
            )
        })?;
        let cwd_bytes = cwd.as_os_str().as_bytes();
        if cwd_bytes.is_empty() || cwd_bytes.contains(&0) {
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                "unable to open database file",
            ));
        }
        builder.append_all_elements(cwd_bytes);
    }
    builder.append_all_elements(raw);

    if let Some(err) = builder.err {
        return Err(err);
    }
    if builder.out.len() < 2 {
        return Err(SqliteError::new(
            ErrorCode::CantOpen,
            "unable to open database file",
        ));
    }
    let resolved = PathBuf::from(OsString::from_vec(builder.out));
    Ok((resolved, builder.n_symlink > 0))
}

/// Callback type that inspects whether `File` must be deferred in `pending_close_fds`:
/// returns `None` if ownership of `File` was stored for deferred close, or `Some(file)` if
/// it is safe to close immediately.
pub(crate) type DeferFileCloseHook = Box<dyn Fn(File) -> Option<File> + Send + Sync>;

/// Shared `-shm` file descriptor owner that outlives every [`SysMapView`] created from it.
///
/// Guarantees that all `mmap` views are `munmap`ped before the underlying `-shm` file
/// descriptor is unlocked (DMS byte 128) and closed.
pub(crate) struct SysShmBacking {
    file: Mutex<Option<File>>,
    shm_path: PathBuf,
    inode_key: (u64, u64),
    readonly: bool,
    delete_on_drop: Mutex<bool>,
    defer_close_fn: Option<DeferFileCloseHook>,
}

impl SysShmBacking {
    pub(crate) fn new(
        file: File,
        shm_path: PathBuf,
        inode_key: (u64, u64),
        readonly: bool,
        defer_close_fn: Option<DeferFileCloseHook>,
    ) -> Self {
        Self {
            file: Mutex::new(Some(file)),
            shm_path,
            inode_key,
            readonly,
            delete_on_drop: Mutex::new(false),
            defer_close_fn,
        }
    }

    #[inline]
    pub(crate) fn inode_key(&self) -> (u64, u64) {
        self.inode_key
    }

    #[inline]
    pub(crate) fn shm_path(&self) -> &Path {
        &self.shm_path
    }

    #[inline]
    pub(crate) fn is_readonly(&self) -> bool {
        self.readonly
    }

    pub(crate) fn mark_delete_on_close(&self) {
        if let Ok(mut guard) = self.delete_on_drop.lock() {
            *guard = true;
        }
    }

    pub(crate) fn with_file<T>(&self, f: impl FnOnce(&File) -> Result<T>) -> Result<T> {
        let guard = self
            .file
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned shm backing mutex"))?;
        let file = guard
            .as_ref()
            .ok_or_else(|| SqliteError::misuse("WAL-index backing file already closed"))?;
        f(file)
    }

    /// Explicitly unlocks DMS, optionally unlinks `-shm`, and closes (or defers) the `-shm` FD.
    /// Must only be called once all [`SysMapView`] instances have been dropped.
    pub(crate) fn close_explicit(&self, delete_file: bool) -> Result<()> {
        let maybe_file = {
            let mut guard = self
                .file
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned shm backing mutex"))?;
            guard.take()
        };
        let should_delete = delete_file || self.delete_on_drop.lock().map(|g| *g).unwrap_or(false);

        let mut first_err: Option<SqliteError> = None;
        if let Some(file) = maybe_file {
            // Release DMS lock on byte 128 before unlinking/closing (`unixShmPurge` in `os_unix.c`).
            if let Err(e) = set_posix_lock(
                file.as_raw_fd(),
                F_UNLCK,
                UNIX_SHM_DMS,
                1,
                extended::SQLITE_IOERR_SHMLOCK,
            ) {
                first_err = Some(e);
            }

            if should_delete
                && !self.readonly
                && let Err(e) = fs::remove_file(&self.shm_path)
                && e.kind() != ErrorKind::NotFound
                && first_err.is_none()
            {
                first_err = Some(SqliteError::with_extended(
                    extended::SQLITE_IOERR_DELETE,
                    e.to_string(),
                ));
            }

            let maybe_to_close = match self.defer_close_fn {
                Some(ref hook) => hook(file),
                None => Some(file),
            };
            if let Some(file_to_close) = maybe_to_close
                && let Err(e) = close_file_fd(file_to_close)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }

        if let Some(err) = first_err {
            return Err(err);
        }
        Ok(())
    }
}

impl Drop for SysShmBacking {
    fn drop(&mut self) {
        let _ = self.close_explicit(false);
    }
}

/// OS page-aligned `mmap` view over a portion of the `-shm` file (`MapView` OS backend).
///
/// Raw pointers never leave this struct: upper layers access WAL-index fields strictly through
/// bounds-, alignment-, and firewall-checked short-lived 16/32-bit atomic operations.
pub(crate) struct SysMapView {
    ptr: NonNull<u8>,
    len: usize,
    writable: bool,
    // Declared AFTER `ptr`/`len` so `Drop::drop` runs `munmap` strictly BEFORE `_backing` drops!
    _backing: Arc<SysShmBacking>,
}

// SAFETY: `SysMapView` owns a `MAP_SHARED` OS virtual memory mapping (`[ptr, ptr + len)`) whose
// lifetime is tied to `self` (`munmap`ped in `Drop` only after the last `Arc<MapView>` drops,
// while `_backing: Arc<SysShmBacking>` keeps the underlying `-shm` FD alive past `munmap`).
// No `&[u8]` or `&mut [u8]` or raw pointer is ever exposed outside `SysMapView`; all concurrent
// reads and writes from Rust threads use naturally aligned `AtomicU16`/`AtomicU32` operations,
// and cross-process concurrency with C SQLite 3.54.1 is coordinated by WAL byte-range locks
// (`120..128`), `shm_barrier` (`SeqCst` fence), and double-header + checksum verification.
unsafe impl Send for SysMapView {}

// SAFETY: Every method on `&SysMapView` validates bounds, overflow, natural alignment, write
// permission, and the region-0 `120..128` lock-byte firewall before performing an atomic
// 16-bit or 32-bit operation, making concurrent shared access across Rust threads sound.
unsafe impl Sync for SysMapView {}

impl SysMapView {
    /// Creates a page-aligned `MAP_SHARED` view of `[file_offset, file_offset + len)` on `backing`.
    pub(crate) fn map(
        backing: Arc<SysShmBacking>,
        file_offset: u64,
        len: usize,
        writable: bool,
    ) -> Result<Self> {
        let page_sz = os_page_size() as u64;
        if len == 0 || !file_offset.is_multiple_of(page_sz) {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHMMAP,
                "invalid mmap length or unaligned file_offset",
            ));
        }
        if writable && backing.is_readonly() {
            return Err(SqliteError::readonly(
                "cannot map readonly -shm as writable",
            ));
        }
        let end_offset = file_offset
            .checked_add(u64::try_from(len).map_err(|_| {
                SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "mmap len overflow")
            })?)
            .ok_or_else(|| {
                SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "mmap end overflow")
            })?;
        let off_i64 = i64::try_from(file_offset).map_err(|_| {
            SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "mmap offset overflows i64")
        })?;
        let _ = i64::try_from(end_offset).map_err(|_| {
            SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "mmap end overflows i64")
        })?;

        let ptr = backing.with_file(|file| {
            let meta = file.metadata().map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_SHMSIZE, e.to_string())
            })?;
            if meta.len() < end_offset {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMSIZE,
                    "backing -shm file shorter than requested mmap view",
                ));
            }

            let prot = if writable {
                PROT_READ | PROT_WRITE
            } else {
                PROT_READ
            };
            // SAFETY: `file` is a valid open `-shm` descriptor whose size was verified to cover
            // `[file_offset, end_offset)`; `off_i64 >= 0` is a multiple of `os_page_size()`, and
            // `len > 0`; `addr = null_mut()` asks the kernel to choose a fresh page-aligned
            // virtual address range without clobbering existing mappings; `MAP_SHARED` shares
            // coherent page-cache pages with C SQLite processes; we check `MAP_FAILED` and null
            // before constructing `NonNull<u8>`.
            let raw = unsafe {
                mmap(
                    core::ptr::null_mut(),
                    len,
                    prot,
                    MAP_SHARED,
                    file.as_raw_fd(),
                    off_i64,
                )
            };
            if (raw as usize) == MAP_FAILED_ADDR {
                let err = std::io::Error::last_os_error();
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMMAP,
                    err.to_string(),
                ));
            }
            let nn = NonNull::new(raw.cast::<u8>()).ok_or_else(|| {
                SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "mmap returned null")
            })?;
            if !(nn.as_ptr() as usize).is_multiple_of(4) {
                // SAFETY: `raw` was just returned by `mmap` with length `len` and is not yet
                // stored in `SysMapView`, so we unmap it once before returning an error.
                let _ = unsafe { munmap(raw, len) };
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMMAP,
                    "mmap returned unaligned base address",
                ));
            }
            Ok(nn)
        })?;

        Ok(Self {
            ptr,
            len,
            writable,
            _backing: backing,
        })
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub(crate) fn is_writable(&self) -> bool {
        self.writable
    }

    /// Opaque numeric identifier of the underlying OS view (used only by tests to assert that
    /// grouped 32 KiB chunks share one OS mapping and that existing chunks never relocate).
    #[inline]
    pub(crate) fn debug_view_id(&self) -> usize {
        self.ptr.as_ptr() as usize
    }

    #[inline]
    fn validate_offset(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        width: usize,
        write: bool,
    ) -> Result<usize> {
        if write && !self.writable {
            return Err(SqliteError::readonly(
                "attempt to write to a read-only WAL-index mapping",
            ));
        }
        if (width != 2 && width != 4) || !rel_offset.is_multiple_of(width) {
            return Err(SqliteError::misuse(
                "unaligned or unsupported WAL-index atomic width",
            ));
        }
        let rel_end = rel_offset
            .checked_add(width)
            .ok_or_else(|| SqliteError::misuse("WAL-index relative offset overflow"))?;
        if rel_end > SHM_LOGICAL_REGION_BYTES {
            return Err(SqliteError::misuse(
                "WAL-index relative offset exceeds 32 KiB region",
            ));
        }
        // Region 0 lock-byte firewall: `WalCkptInfo.aLock[0..8]` occupies bytes `120..128` and
        // MUST NEVER be read or written as memory (`implementation_plan.md` lines 25, 90, 101).
        // Bytes `128..132` (`nBackfillAttempted`) ARE normal mapped data despite DMS OS lock at 128!
        if region_index == 0 && rel_offset < WAL_LOCK_BYTES_END && rel_end > WAL_LOCK_BYTES_START {
            return Err(SqliteError::misuse(
                "access to reserved WAL lock byte range 120..128 is forbidden",
            ));
        }
        let abs_offset = subrange_offset
            .checked_add(rel_offset)
            .ok_or_else(|| SqliteError::misuse("WAL-index view offset overflow"))?;
        let abs_end = abs_offset
            .checked_add(width)
            .ok_or_else(|| SqliteError::misuse("WAL-index view end overflow"))?;
        if abs_end > self.len {
            return Err(SqliteError::misuse(
                "WAL-index access out of MapView bounds",
            ));
        }
        debug_assert_eq!((self.ptr.as_ptr() as usize + abs_offset) % width, 0);
        Ok(abs_offset)
    }

    #[inline]
    pub(crate) fn load_u32(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        ordering: Ordering,
    ) -> Result<u32> {
        let abs_offset =
            self.validate_offset(subrange_offset, region_index, rel_offset, 4, false)?;
        // SAFETY: `abs_offset..abs_offset + 4` was checked against `self.len`, does not overlap
        // `aLock` (`120..128` in region 0), and `(ptr + abs_offset)` is 4-byte aligned because
        // both `self.ptr` (page-aligned) and `abs_offset` are multiples of 4. The mapping stays
        // valid for `&self`'s lifetime (`munmap` only runs in `Drop`), and `AtomicU32::from_ptr`
        // forms a transient atomic reference confined to this single hardware load instruction
        // (valid on `PROT_READ` and `PROT_READ|PROT_WRITE` mappings on 64-bit `target_has_atomic="32"`
        // platforms, interoperating with C SQLite 3.54.1 via WAL locks, `shm_barrier`, and
        // double-header + checksum validation).
        let word = unsafe {
            let p = self.ptr.as_ptr().add(abs_offset).cast::<u32>();
            AtomicU32::from_ptr(p).load(ordering)
        };
        Ok(word)
    }

    #[inline]
    pub(crate) fn store_u32(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        value: u32,
        ordering: Ordering,
    ) -> Result<()> {
        let abs_offset =
            self.validate_offset(subrange_offset, region_index, rel_offset, 4, true)?;
        // SAFETY: `self.writable` was verified (`PROT_READ | PROT_WRITE`), `abs_offset..abs_offset + 4`
        // is in-bounds of `[self.ptr, self.ptr + self.len)`, 4-byte aligned, and outside `120..128`
        // in region 0. The mapping outlives `&self`, callers hold the required WAL protocol lock
        // (`WRITE`, `CKPT`, `RECOVER`, or `READ(slot)` exclusive), and `AtomicU32::from_ptr` is
        // confined strictly to this single atomic store without exposing references to callers.
        unsafe {
            let p = self.ptr.as_ptr().add(abs_offset).cast::<u32>();
            AtomicU32::from_ptr(p).store(value, ordering);
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn load_u16(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        ordering: Ordering,
    ) -> Result<u16> {
        let abs_offset =
            self.validate_offset(subrange_offset, region_index, rel_offset, 2, false)?;
        // SAFETY: `abs_offset..abs_offset + 2` is verified in-bounds of `self.len`, 2-byte
        // aligned, and outside region-0 `120..128`. On 64-bit `target_has_atomic="16"` platforms,
        // `AtomicU16::load` is a pure lock-free read safe on both `PROT_READ` and `PROT_WRITE`
        // mappings and interoperating with C SQLite `ht_slot` (`volatile u16`) accesses.
        let half = unsafe {
            let p = self.ptr.as_ptr().add(abs_offset).cast::<u16>();
            AtomicU16::from_ptr(p).load(ordering)
        };
        Ok(half)
    }

    #[inline]
    pub(crate) fn store_u16(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        value: u16,
        ordering: Ordering,
    ) -> Result<()> {
        let abs_offset =
            self.validate_offset(subrange_offset, region_index, rel_offset, 2, true)?;
        // SAFETY: `self.writable` was verified, `abs_offset..abs_offset + 2` is in-bounds,
        // 2-byte aligned, and outside region-0 `120..128`. The transient `AtomicU16` reference
        // lives only for this store and publishes `ht_slot` updates atomically to concurrent
        // readers in Rust threads and C SQLite processes.
        unsafe {
            let p = self.ptr.as_ptr().add(abs_offset).cast::<u16>();
            AtomicU16::from_ptr(p).store(value, ordering);
        }
        Ok(())
    }

    /// Zeroes `[start_rel, end_rel)` in 4-byte atomic stores, rejecting unaligned ranges or any
    /// overlap with `120..128` in region 0.
    pub(crate) fn zero_aligned_u32_range(
        &self,
        subrange_offset: usize,
        region_index: u32,
        start_rel: usize,
        end_rel: usize,
    ) -> Result<()> {
        if start_rel > end_rel || !start_rel.is_multiple_of(4) || !end_rel.is_multiple_of(4) {
            return Err(SqliteError::misuse("invalid WAL-index zero range"));
        }
        let mut off = start_rel;
        while off < end_rel {
            self.store_u32(subrange_offset, region_index, off, 0, Ordering::Relaxed)?;
            off += 4;
        }
        Ok(())
    }
}

impl Drop for SysMapView {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` and `self.len` come from a successful `mmap` in `SysMapView::map`,
        // no `ShmChunk` or external reference can outlive `self` (since `ShmChunk` holds
        // `Arc<MapView>`), and `_backing: Arc<SysShmBacking>` has not yet been dropped so the
        // underlying `-shm` file descriptor is still open during `munmap`.
        let _ = unsafe { munmap(self.ptr.as_ptr().cast::<core::ffi::c_void>(), self.len) };
    }
}

/// Full hardware memory barrier (`xShmBarrier` / `unixShmBarrier` in `sqlite/src/os_unix.c`).
#[inline]
pub(crate) fn shm_barrier() {
    fence(Ordering::SeqCst);
}
