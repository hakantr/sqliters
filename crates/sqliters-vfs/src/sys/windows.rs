//! Win32 FFI boundary (`sqlite/src/os_win.c` port) for per-HANDLE byte-range locking
//! (`LockFileEx`/`UnlockFileEx`, `bUseSharedLockHandle == 0` local model), volume+file-ID
//! identity (`GetFileInformationByHandle`), `GetFullPathNameW` + remote/UNC rejection,
//! and `dwAllocationGranularity`-aligned `CreateFileMappingW` / `MapViewOfFile` views.
#![allow(unsafe_code)]

#[cfg(not(all(
    target_pointer_width = "64",
    target_has_atomic = "16",
    target_has_atomic = "32"
)))]
compile_error!(
    "sqliters-vfs Windows sys backend requires 64-bit pointer width and native 16/32-bit atomics"
);

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, IntoRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering, fence};
use std::sync::{Arc, Mutex, OnceLock};

use sqliters_types::{ErrorCode, Result, SqliteError, extended};

pub(crate) const WIN_SHM_BASE: u64 = 120;
pub(crate) const WIN_SHM_DMS: u64 = 128;

const WAL_LOCK_BYTES_START: usize = 120;
const WAL_LOCK_BYTES_END: usize = 128;
const SHM_LOGICAL_REGION_BYTES: usize = 32_768;

const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;

const ERROR_SHARING_VIOLATION: u32 = 32;
const ERROR_LOCK_VIOLATION: u32 = 33;
const ERROR_HANDLE_DISK_FULL: u32 = 39;
const ERROR_DISK_FULL: u32 = 112;
const ERROR_IO_PENDING: u32 = 997;

const PAGE_READONLY: u32 = 0x02;
const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_WRITE: u32 = 0x0002;
const FILE_MAP_READ: u32 = 0x0004;

const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const FILE_SHARE_DELETE: u32 = 0x0000_0004;

const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOTE: u32 = 4;

type Handle = *mut core::ffi::c_void;
type Bool = i32;

#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    h_event: Handle,
}

impl Overlapped {
    #[inline]
    const fn at_offset(offset: u64) -> Self {
        Self {
            internal: 0,
            internal_high: 0,
            offset: offset as u32,
            offset_high: (offset >> 32) as u32,
            h_event: core::ptr::null_mut(),
        }
    }
}

#[repr(C)]
struct FileTime {
    dw_low_date_time: u32,
    dw_high_date_time: u32,
}

#[repr(C)]
struct ByHandleFileInformation {
    dw_file_attributes: u32,
    ft_creation_time: FileTime,
    ft_last_access_time: FileTime,
    ft_last_write_time: FileTime,
    dw_volume_serial_number: u32,
    n_file_size_high: u32,
    n_file_size_low: u32,
    n_number_of_links: u32,
    n_file_index_high: u32,
    n_file_index_low: u32,
}

#[repr(C)]
struct SystemInfo {
    w_processor_architecture: u16,
    w_reserved: u16,
    dw_page_size: u32,
    lp_minimum_application_address: *mut core::ffi::c_void,
    lp_maximum_application_address: *mut core::ffi::c_void,
    dw_active_processor_mask: usize,
    dw_number_of_processors: u32,
    dw_processor_type: u32,
    dw_allocation_granularity: u32,
    w_processor_level: u16,
    w_processor_revision: u16,
}

#[repr(C)]
struct LargeInteger {
    quad_part: i64,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
    fn LockFileEx(
        h_file: Handle,
        dw_flags: u32,
        dw_reserved: u32,
        n_number_of_bytes_to_lock_low: u32,
        n_number_of_bytes_to_lock_high: u32,
        lp_overlapped: *mut Overlapped,
    ) -> Bool;
    fn UnlockFileEx(
        h_file: Handle,
        dw_reserved: u32,
        n_number_of_bytes_to_unlock_low: u32,
        n_number_of_bytes_to_unlock_high: u32,
        lp_overlapped: *mut Overlapped,
    ) -> Bool;
    fn GetFileInformationByHandle(
        h_file: Handle,
        lp_file_information: *mut ByHandleFileInformation,
    ) -> Bool;
    fn GetFullPathNameW(
        lp_file_name: *const u16,
        n_buffer_length: u32,
        lp_buffer: *mut u16,
        lp_file_part: *mut *mut u16,
    ) -> u32;
    fn GetDriveTypeW(lp_root_path_name: *const u16) -> u32;
    fn GetSystemInfo(lp_system_info: *mut SystemInfo);
    fn CloseHandle(h_object: Handle) -> Bool;
    fn SetFilePointerEx(
        h_file: Handle,
        li_distance_to_move: LargeInteger,
        lp_new_file_pointer: *mut LargeInteger,
        dw_move_method: u32,
    ) -> Bool;
    fn SetEndOfFile(h_file: Handle) -> Bool;
    fn WriteFile(
        h_file: Handle,
        lp_buffer: *const u8,
        n_number_of_bytes_to_write: u32,
        lp_number_of_bytes_written: *mut u32,
        lp_overlapped: *mut Overlapped,
    ) -> Bool;
    fn CreateFileMappingW(
        h_file: Handle,
        lp_file_mapping_attributes: *mut core::ffi::c_void,
        fl_protect: u32,
        dw_maximum_size_high: u32,
        dw_maximum_size_low: u32,
        lp_name: *const u16,
    ) -> Handle;
    fn MapViewOfFile(
        h_file_mapping_object: Handle,
        dw_desired_access: u32,
        dw_file_offset_high: u32,
        dw_file_offset_low: u32,
        dw_number_of_bytes_to_map: usize,
    ) -> *mut core::ffi::c_void;
    fn UnmapViewOfFile(lp_base_address: *const core::ffi::c_void) -> Bool;
}

/// Unique `(volume_serial_number, 64-bit file_index)` identity for an open Windows HANDLE.
pub(crate) fn file_id_by_handle(file: &File) -> Result<(u64, u64)> {
    let mut info = ByHandleFileInformation {
        dw_file_attributes: 0,
        ft_creation_time: FileTime {
            dw_low_date_time: 0,
            dw_high_date_time: 0,
        },
        ft_last_access_time: FileTime {
            dw_low_date_time: 0,
            dw_high_date_time: 0,
        },
        ft_last_write_time: FileTime {
            dw_low_date_time: 0,
            dw_high_date_time: 0,
        },
        dw_volume_serial_number: 0,
        n_file_size_high: 0,
        n_file_size_low: 0,
        n_number_of_links: 0,
        n_file_index_high: 0,
        n_file_index_low: 0,
    };
    // SAFETY: `info` is a stack-owned, fully initialized `#[repr(C)]` `ByHandleFileInformation`,
    // and `file.as_raw_handle()` is a valid open Win32 HANDLE kept alive by `file`.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        return Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_FSTAT,
            err.to_string(),
        ));
    }
    let vol = u64::from(info.dw_volume_serial_number);
    let idx = (u64::from(info.n_file_index_high) << 32) | u64::from(info.n_file_index_low);
    Ok((vol, idx))
}

/// Returns `SYSTEM_INFO.dwAllocationGranularity` (typically 65,536 bytes on 64-bit Windows).
pub(crate) fn allocation_granularity() -> usize {
    static GRAN: OnceLock<usize> = OnceLock::new();
    *GRAN.get_or_init(|| {
        let mut info = SystemInfo {
            w_processor_architecture: 0,
            w_reserved: 0,
            dw_page_size: 0,
            lp_minimum_application_address: core::ptr::null_mut(),
            lp_maximum_application_address: core::ptr::null_mut(),
            dw_active_processor_mask: 0,
            dw_number_of_processors: 0,
            dw_processor_type: 0,
            dw_allocation_granularity: 0,
            w_processor_level: 0,
            w_processor_revision: 0,
        };
        // SAFETY: `info` is a valid, stack-owned `#[repr(C)]` `SystemInfo` struct written once
        // by `GetSystemInfo`; `GetSystemInfo` cannot fail and touches no shared memory.
        unsafe { GetSystemInfo(&mut info) };
        let gran = info.dw_allocation_granularity as usize;
        if gran >= 4096 && gran.is_power_of_two() {
            gran
        } else {
            65_536
        }
    })
}

/// Acquires a non-blocking Win32 byte-range lock (`LockFileEx` with `LOCKFILE_FAIL_IMMEDIATELY`).
pub(crate) fn lock_file_range(
    raw_handle: RawHandle,
    offset: u64,
    len: u64,
    exclusive: bool,
    fallback_ioerr: i32,
) -> Result<()> {
    let mut ov = Overlapped::at_offset(offset);
    let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
    if exclusive {
        flags |= LOCKFILE_EXCLUSIVE_LOCK;
    }
    let len_low = len as u32;
    let len_high = (len >> 32) as u32;
    // SAFETY: `ov` is a stack-owned, zero-event `OVERLAPPED` valid for this synchronous
    // (`LOCKFILE_FAIL_IMMEDIATELY`) `LockFileEx` call; `raw_handle` is a valid open file HANDLE
    // kept alive by the caller; `LockFileEx` is thread- and process-safe across Win32 handles.
    let ok = unsafe { LockFileEx(raw_handle, flags, 0, len_low, len_high, &mut ov) };
    if ok != 0 {
        return Ok(());
    }
    // SAFETY: `GetLastError` reads the calling thread's Win32 TEB error slot with no pointers.
    let code = unsafe { GetLastError() };
    if code == ERROR_LOCK_VIOLATION || code == ERROR_IO_PENDING || code == ERROR_SHARING_VIOLATION {
        return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
    }
    Err(SqliteError::with_extended(
        fallback_ioerr,
        format!("LockFileEx failed (Win32 error {code})"),
    ))
}

/// Releases a Win32 byte-range lock (`UnlockFileEx`).
pub(crate) fn unlock_file_range(
    raw_handle: RawHandle,
    offset: u64,
    len: u64,
    fallback_ioerr: i32,
) -> Result<()> {
    let mut ov = Overlapped::at_offset(offset);
    let len_low = len as u32;
    let len_high = (len >> 32) as u32;
    // SAFETY: `ov` is a stack-owned `OVERLAPPED` valid for this synchronous `UnlockFileEx` call,
    // and `raw_handle` is a valid open file HANDLE owned by the caller.
    let ok = unsafe { UnlockFileEx(raw_handle, 0, len_low, len_high, &mut ov) };
    if ok != 0 {
        return Ok(());
    }
    // SAFETY: `GetLastError` reads thread-local Win32 error state.
    let code = unsafe { GetLastError() };
    Err(SqliteError::with_extended(
        fallback_ioerr,
        format!("UnlockFileEx failed (Win32 error {code})"),
    ))
}

/// Explicitly closes a `File` via `CloseHandle`, propagating any OS error as `SQLITE_IOERR_CLOSE`.
pub(crate) fn close_file_handle(file: File) -> Result<()> {
    let raw = file.into_raw_handle();
    // SAFETY: `into_raw_handle()` transfers unique ownership of `raw` out of `File` without
    // running `Drop`; `CloseHandle` is called once and `raw` is never used afterward.
    let ok = unsafe { CloseHandle(raw) };
    if ok != 0 {
        return Ok(());
    }
    // SAFETY: `GetLastError` reads thread-local Win32 error state.
    let code = unsafe { GetLastError() };
    Err(SqliteError::with_extended(
        extended::SQLITE_IOERR_CLOSE,
        format!("CloseHandle failed (Win32 error {code})"),
    ))
}

/// Truncates `raw_handle` to `size` bytes (`winHandleTruncate` in `sqlite/src/os_win.c`).
pub(crate) fn truncate_handle(raw_handle: RawHandle, size: u64, ioerr_code: i32) -> Result<()> {
    let pos = i64::try_from(size)
        .map_err(|_| SqliteError::with_extended(ioerr_code, "truncate size overflows i64"))?;
    let li = LargeInteger { quad_part: pos };
    // SAFETY: `raw_handle` is a valid writable Win32 file HANDLE; `li` is passed by value and
    // `lp_new_file_pointer` is null (`FILE_BEGIN = 0`).
    let moved = unsafe { SetFilePointerEx(raw_handle, li, core::ptr::null_mut(), 0) };
    if moved == 0 {
        // SAFETY: `GetLastError` reads thread-local Win32 error state.
        let code = unsafe { GetLastError() };
        return Err(SqliteError::with_extended(
            ioerr_code,
            format!("SetFilePointerEx failed (Win32 error {code})"),
        ));
    }
    // SAFETY: `raw_handle` is positioned at `pos` and valid for `SetEndOfFile`.
    let ok = unsafe { SetEndOfFile(raw_handle) };
    if ok != 0 {
        return Ok(());
    }
    // SAFETY: `GetLastError` reads thread-local Win32 error state.
    let code = unsafe { GetLastError() };
    if code == ERROR_DISK_FULL || code == ERROR_HANDLE_DISK_FULL {
        return Err(SqliteError::new(
            ErrorCode::Full,
            format!("SetEndOfFile disk full (Win32 error {code})"),
        ));
    }
    Err(SqliteError::with_extended(
        ioerr_code,
        format!("SetEndOfFile failed (Win32 error {code})"),
    ))
}

/// Zeroes the first 8 bytes of `-shm` while holding the exclusive DMS lock when `SetEndOfFile(0)`
/// fails due to an active mapping in another process (`winZeroSharedMemory` in `os_win.c:3094`).
pub(crate) fn zero_first_eight_bytes(raw_handle: RawHandle) -> Result<()> {
    let zeroes = [0u8; 8];
    let mut written = 0u32;
    let mut ov = Overlapped::at_offset(0);
    // SAFETY: `zeroes` (8 bytes), `written`, and `ov` are stack-owned and valid throughout this
    // synchronous `WriteFile` call on `raw_handle` (opened without `FILE_FLAG_OVERLAPPED`).
    let ok = unsafe {
        WriteFile(
            raw_handle,
            zeroes.as_ptr(),
            zeroes.len() as u32,
            &mut written,
            &mut ov,
        )
    };
    if ok != 0 && written == 8 {
        return Ok(());
    }
    // SAFETY: `GetLastError` reads thread-local Win32 error state.
    let code = unsafe { GetLastError() };
    Err(SqliteError::with_extended(
        extended::SQLITE_IOERR_WRITE,
        format!("winZeroSharedMemory failed (Win32 error {code})"),
    ))
}

/// Resolves `path` via `GetFullPathNameW` (`winFullPathname` in `sqlite/src/os_win.c:4820`).
pub(crate) fn win_full_pathname(path: &Path) -> Result<PathBuf> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) {
        return Err(SqliteError::new(
            ErrorCode::CantOpen,
            "unable to open database file",
        ));
    }
    let mut z_wide = wide;
    z_wide.push(0);

    // SAFETY: `z_wide` is NUL-terminated UTF-16; passing buffer length 0 with null output queries
    // the required UTF-16 buffer capacity including the terminating NUL.
    let needed = unsafe {
        GetFullPathNameW(
            z_wide.as_ptr(),
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        )
    };
    if needed == 0 {
        return Err(SqliteError::with_extended(
            extended::SQLITE_CANTOPEN_FULLPATH,
            "GetFullPathNameW failed",
        ));
    }
    let mut out = vec![0u16; (needed as usize) + 2];
    // SAFETY: `out` has capacity `needed + 2` UTF-16 code units and `z_wide` is NUL-terminated.
    let len = unsafe {
        GetFullPathNameW(
            z_wide.as_ptr(),
            out.len() as u32,
            out.as_mut_ptr(),
            core::ptr::null_mut(),
        )
    };
    if len == 0 || (len as usize) >= out.len() {
        return Err(SqliteError::with_extended(
            extended::SQLITE_CANTOPEN_FULLPATH,
            "GetFullPathNameW buffer overflow",
        ));
    }
    let mut slice = &out[..len as usize];
    // Trim trailing separators (`test.db//` -> `test.db`) unless at drive root (`C:\`).
    while slice.len() > 3
        && (slice.last() == Some(&b'\\'.into()) || slice.last() == Some(&b'/'.into()))
    {
        slice = &slice[..slice.len() - 1];
    }
    Ok(PathBuf::from(OsString::from_wide(slice)))
}

/// Rejects UNC (`\\server\share`, `\\?\UNC\`) and `DRIVE_REMOTE` paths for shared WAL-index
/// (`implementation_plan.md` line 91: local `bUseSharedLockHandle == 0` model only).
pub(crate) fn verify_local_path_for_shm(full_path: &Path) -> Result<()> {
    let s = full_path.to_string_lossy();
    if s.starts_with("\\\\?\\UNC\\")
        || s.starts_with("//?/UNC/")
        || (s.starts_with("\\\\") && !s.starts_with("\\\\?\\"))
        || (s.starts_with("//") && !s.starts_with("//?/"))
    {
        return Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_SHMOPEN,
            "UNC/remote paths are not supported for shared WAL-index",
        ));
    }
    let clean = s.strip_prefix("\\\\?\\").unwrap_or(&s);
    let bytes = clean.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        let root: [u16; 4] = [u16::from(bytes[0]), u16::from(b':'), u16::from(b'\\'), 0];
        // SAFETY: `root` is a NUL-terminated 4-element `X:\0` UTF-16 drive root on the stack.
        let dtype = unsafe { GetDriveTypeW(root.as_ptr()) };
        if dtype == DRIVE_REMOTE || dtype == DRIVE_UNKNOWN || dtype == DRIVE_NO_ROOT_DIR {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHMOPEN,
                "remote or unknown Windows drive type is not supported for shared WAL-index",
            ));
        }
    }
    Ok(())
}

/// Opens a `-shm` `File` with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`.
pub(crate) fn open_shm_handle(shm_path: &Path, create_writable: bool) -> Result<(File, bool)> {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    if create_writable {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(share)
            .open(shm_path)
        {
            Ok(f) => return Ok((f, false)),
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {}
            Err(e) => {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMOPEN,
                    e.to_string(),
                ));
            }
        }
    }
    let f = OpenOptions::new()
        .read(true)
        .share_mode(share)
        .open(shm_path)
        .map_err(|e| SqliteError::with_extended(extended::SQLITE_IOERR_SHMOPEN, e.to_string()))?;
    Ok((f, true))
}

pub(crate) fn pread_until_eof(file: &File, buf: &mut [u8], offset: u64) -> Result<usize> {
    let mut total = 0usize;
    while total < buf.len() {
        let cur_off = offset.checked_add(total as u64).ok_or_else(|| {
            SqliteError::with_extended(extended::SQLITE_IOERR_READ, "read offset overflow")
        })?;
        match file.seek_read(&mut buf[total..], cur_off) {
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

pub(crate) fn pwrite_all(file: &File, buf: &[u8], offset: u64, ioerr_code: i32) -> Result<()> {
    let mut written = 0usize;
    while written < buf.len() {
        let cur_off = offset
            .checked_add(written as u64)
            .ok_or_else(|| SqliteError::with_extended(ioerr_code, "write offset overflow"))?;
        match file.seek_write(&buf[written..], cur_off) {
            Ok(0) => {
                return Err(SqliteError::with_extended(
                    ioerr_code,
                    "seek_write wrote 0 bytes",
                ));
            }
            Ok(n) => written += n,
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                if matches!(
                    e.raw_os_error(),
                    Some(c) if c == ERROR_DISK_FULL as i32 || c == ERROR_HANDLE_DISK_FULL as i32
                ) {
                    return Err(SqliteError::new(ErrorCode::Full, e.to_string()));
                }
                return Err(SqliteError::with_extended(ioerr_code, e.to_string()));
            }
        }
    }
    Ok(())
}

/// Shared `-shm` file backing (`hSharedShm` in `os_win.c`) that outlives all [`SysMapView`]s.
pub(crate) struct SysShmBacking {
    file: Mutex<Option<File>>,
    shm_path: PathBuf,
    file_id: (u64, u64),
    readonly: bool,
    delete_on_drop: Mutex<bool>,
}

impl SysShmBacking {
    pub(crate) fn new(file: File, shm_path: PathBuf, file_id: (u64, u64), readonly: bool) -> Self {
        Self {
            file: Mutex::new(Some(file)),
            shm_path,
            file_id,
            readonly,
            delete_on_drop: Mutex::new(false),
        }
    }

    #[inline]
    pub(crate) fn file_id(&self) -> (u64, u64) {
        self.file_id
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
        if let Ok(mut g) = self.delete_on_drop.lock() {
            *g = true;
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

    pub(crate) fn close_explicit(&self, delete_file: bool) -> Result<()> {
        let maybe_file = {
            let mut guard = self
                .file
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned shm backing mutex"))?;
            guard.take()
        };
        let should_delete = delete_file || self.delete_on_drop.lock().map(|g| *g).unwrap_or(false);
        let mut first_err = None;
        if let Some(file) = maybe_file {
            let _ = unlock_file_range(
                file.as_raw_handle(),
                WIN_SHM_DMS,
                1,
                extended::SQLITE_IOERR_SHMLOCK,
            );
            if let Err(e) = close_file_handle(file) {
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
        }
        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(())
    }
}

impl Drop for SysShmBacking {
    fn drop(&mut self) {
        let _ = self.close_explicit(false);
    }
}

/// Allocation-granularity-aligned Win32 `MapViewOfFile` view (`os_win.c:3698-3738`).
pub(crate) struct SysMapView {
    ptr: NonNull<u8>,
    len: usize,
    writable: bool,
    map_handle: Handle,
    _backing: Arc<SysShmBacking>,
}

// SAFETY: `SysMapView` owns a shared file-mapping view `[ptr, ptr + len)` and `map_handle`,
// unmapped and closed in `Drop` strictly before `_backing` closes `hSharedShm`. No raw pointers
// or slices escape `SysMapView`, and all accesses are naturally aligned 16/32-bit atomics.
unsafe impl Send for SysMapView {}

// SAFETY: Shared references perform bounds-, alignment-, permission-, and `120..128` firewall-
// checked atomic operations safe across concurrent Rust threads and C SQLite processes.
unsafe impl Sync for SysMapView {}

impl SysMapView {
    pub(crate) fn map(
        backing: Arc<SysShmBacking>,
        file_offset: u64,
        len: usize,
        writable: bool,
    ) -> Result<Self> {
        let gran = allocation_granularity() as u64;
        if len == 0 || !file_offset.is_multiple_of(gran) {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHMMAP,
                "MapViewOfFile offset must be a multiple of dwAllocationGranularity",
            ));
        }
        if writable && backing.is_readonly() {
            return Err(SqliteError::readonly(
                "cannot map readonly -shm as writable",
            ));
        }
        let end_offset = file_offset
            .checked_add(u64::try_from(len).map_err(|_| {
                SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "view length overflow")
            })?)
            .ok_or_else(|| {
                SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "view end overflow")
            })?;

        let (ptr, map_handle) = backing.with_file(|file| {
            let meta = file.metadata().map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_SHMSIZE, e.to_string())
            })?;
            if meta.len() < end_offset {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMSIZE,
                    "backing -shm file shorter than requested MapViewOfFile range",
                ));
            }
            let protect = if writable {
                PAGE_READWRITE
            } else {
                PAGE_READONLY
            };
            let access = if writable {
                FILE_MAP_READ | FILE_MAP_WRITE
            } else {
                FILE_MAP_READ
            };
            let max_high = (end_offset >> 32) as u32;
            let max_low = end_offset as u32;
            // SAFETY: `file.as_raw_handle()` is an open `-shm` file HANDLE whose length covers
            // `end_offset`; null attributes and null name create an unnamed section object.
            let h_map = unsafe {
                CreateFileMappingW(
                    file.as_raw_handle(),
                    core::ptr::null_mut(),
                    protect,
                    max_high,
                    max_low,
                    core::ptr::null(),
                )
            };
            if h_map.is_null() {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMMAP,
                    "CreateFileMappingW failed",
                ));
            }
            let off_high = (file_offset >> 32) as u32;
            let off_low = file_offset as u32;
            // SAFETY: `h_map` is a valid file-mapping HANDLE covering `end_offset`, and
            // `file_offset` is aligned to `dwAllocationGranularity`.
            let raw = unsafe { MapViewOfFile(h_map, access, off_high, off_low, len) };
            let Some(nn) = NonNull::new(raw.cast::<u8>()) else {
                // SAFETY: `h_map` was created above and `MapViewOfFile` failed, so close `h_map`.
                let _ = unsafe { CloseHandle(h_map) };
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMMAP,
                    "MapViewOfFile returned null",
                ));
            };
            Ok((nn, h_map))
        })?;

        Ok(Self {
            ptr,
            len,
            writable,
            map_handle,
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
        // SAFETY: Verified in-bounds, 4-byte aligned, and outside mandatory-locked `120..128`
        // (note: mandatory lock at DMS byte 128 only blocks `ReadFile`/`WriteFile`, NOT shared
        // `MapViewOfFile` memory loads/stores of `nBackfillAttempted` at `128..132`!).
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
        // SAFETY: `self.writable` was verified, `abs_offset..abs_offset + 4` is in-bounds,
        // 4-byte aligned, and outside `120..128`.
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
        // SAFETY: Verified in-bounds, 2-byte aligned, and outside `120..128`.
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
        // SAFETY: `self.writable` verified, in-bounds, 2-byte aligned, and outside `120..128`.
        unsafe {
            let p = self.ptr.as_ptr().add(abs_offset).cast::<u16>();
            AtomicU16::from_ptr(p).store(value, ordering);
        }
        Ok(())
    }

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
        // SAFETY: `self.ptr` and `self.map_handle` were created by `CreateFileMappingW` +
        // `MapViewOfFile` in `SysMapView::map`; we unmap the view first, then close the mapping
        // handle, strictly before `_backing` drops and closes `hSharedShm` (`os_win.c:3063-3072`).
        let _ = unsafe { UnmapViewOfFile(self.ptr.as_ptr().cast::<core::ffi::c_void>()) };
        let _ = unsafe { CloseHandle(self.map_handle) };
    }
}

/// Full hardware memory barrier (`winShmBarrier` / `MemoryBarrier()` in `sqlite/src/os_win.c`).
#[inline]
pub(crate) fn shm_barrier() {
    fence(Ordering::SeqCst);
}
