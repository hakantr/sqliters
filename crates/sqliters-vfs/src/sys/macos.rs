//! macOS-specific Darwin syscall ABI constants, `F_FULLFSYNC` durability helper,
//! and `fstatfs` local-filesystem verification (`sqlite/src/os_unix.c`).
#![allow(unsafe_code)]

#[cfg(not(target_pointer_width = "64"))]
compile_error!("sqliters-vfs macOS sys backend requires a 64-bit Darwin target");

use std::os::fd::RawFd;

use sqliters_types::{Result, SqliteError, extended};

pub(crate) const F_RDLCK: i16 = 1;
pub(crate) const F_UNLCK: i16 = 2;
pub(crate) const F_WRLCK: i16 = 3;
pub(crate) const F_GETLK: i32 = 7;
pub(crate) const F_SETLK: i32 = 8;
pub(crate) const F_FULLFSYNC: i32 = 51;

pub(crate) const EINTR: i32 = 4;
pub(crate) const EACCES: i32 = 13;
pub(crate) const ENOSPC: i32 = 28;
pub(crate) const EAGAIN: i32 = 35;

pub(crate) const PROT_READ: i32 = 0x01;
pub(crate) const PROT_WRITE: i32 = 0x02;
pub(crate) const MAP_SHARED: i32 = 0x0001;
pub(crate) const SC_PAGESIZE: i32 = 29;

const MNT_LOCAL: u32 = 0x0000_1000;

/// Darwin `<sys/fcntl.h>` `struct flock` layout.
#[repr(C)]
pub(crate) struct PosixFlock {
    pub(crate) l_start: i64,
    pub(crate) l_len: i64,
    pub(crate) l_pid: i32,
    pub(crate) l_type: i16,
    pub(crate) l_whence: i16,
}

impl PosixFlock {
    #[inline]
    pub(crate) const fn new(l_type: i16, l_whence: i16, l_start: i64, l_len: i64) -> Self {
        Self {
            l_start,
            l_len,
            l_pid: 0,
            l_type,
            l_whence,
        }
    }
}

/// 64-bit inode Darwin `<sys/mount.h>` `struct statfs`.
#[repr(C)]
struct DarwinStatfs {
    f_bsize: u32,
    f_iosize: i32,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_fsid: [i32; 2],
    f_owner: u32,
    f_type: u32,
    f_flags: u32,
    f_fssubtype: u32,
    f_fstypename: [u8; 16],
    f_mntonname: [u8; 1024],
    f_mntfromname: [u8; 1024],
    f_flags_ext: u32,
    f_reserved: [u32; 7],
}

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn fsync(fd: i32) -> i32;
    #[cfg_attr(target_arch = "x86_64", link_name = "fstatfs$INODE64")]
    fn fstatfs(fd: i32, buf: *mut DarwinStatfs) -> i32;
}

/// Flushes `fd` on macOS following `full_fsync` in `sqlite/src/os_unix.c`:
/// tries `fcntl(fd, F_FULLFSYNC)` when `full` is requested and falls back to `fsync(fd)`
/// if the filesystem does not support `F_FULLFSYNC`; uses `fsync(fd)` even for `data_only`
/// because Darwin `fdatasync` historically did not flush file-size metadata on HFS+.
pub(crate) fn sync_fd_macos(fd: RawFd, full: bool, _data_only: bool) -> Result<()> {
    if full {
        // SAFETY: `fd` is a valid open file descriptor owned by the caller for the duration
        // of this call; `F_FULLFSYNC` takes no variadic pointer argument on Darwin (`os_unix.c`
        // passes `fcntl(fd, F_FULLFSYNC, 0)`), mutates no Rust memory, is thread-safe across
        // concurrent FDs/processes, and if unsupported returns `-1` so we fall back to `fsync`.
        let rc = unsafe { fcntl(fd, F_FULLFSYNC, 0i32) };
        if rc == 0 {
            return Ok(());
        }
    }

    loop {
        // SAFETY: `fd` is a valid open file descriptor borrowed from the caller; `fsync`
        // touches no user-space pointers, is thread-safe with respect to Rust memory and
        // cross-process SQLite readers/writers, and can safely be retried on `EINTR`.
        let rc = unsafe { fsync(fd) };
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

/// Rejects non-local mounts (`(f_flags & MNT_LOCAL) == 0`) and unsupported proxy-locking
/// overrides when opening shared WAL-index memory on macOS.
pub(crate) fn verify_local_filesystem_macos(fd: RawFd) -> Result<()> {
    if let Ok(val) = std::env::var("SQLITE_FORCE_PROXY_LOCKING")
        && val.trim().parse::<i32>().unwrap_or(0) > 0
    {
        return Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_SHMOPEN,
            "macOS proxy locking style is not supported for shared WAL-index",
        ));
    }

    let mut info = DarwinStatfs {
        f_bsize: 0,
        f_iosize: 0,
        f_blocks: 0,
        f_bfree: 0,
        f_bavail: 0,
        f_files: 0,
        f_ffree: 0,
        f_fsid: [0; 2],
        f_owner: 0,
        f_type: 0,
        f_flags: 0,
        f_fssubtype: 0,
        f_fstypename: [0; 16],
        f_mntonname: [0; 1024],
        f_mntfromname: [0; 1024],
        f_flags_ext: 0,
        f_reserved: [0; 7],
    };

    loop {
        // SAFETY: `info` is a stack-owned, fully initialized, naturally aligned 64-bit inode
        // `DarwinStatfs` (`#[repr(C)]`), and `&mut info` is valid for writes throughout the
        // `fstatfs` syscall; `fd` is a valid open descriptor borrowed from the caller; no
        // shared memory or cross-thread alias exists to `info`, and error cleanup requires no
        // deallocation beyond stack drop.
        let rc = unsafe { fstatfs(fd, &mut info as *mut DarwinStatfs) };
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

    if (info.f_flags & MNT_LOCAL) == 0 {
        return Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_SHMOPEN,
            "shared WAL-index requires a local filesystem (MNT_LOCAL not set)",
        ));
    }
    Ok(())
}
