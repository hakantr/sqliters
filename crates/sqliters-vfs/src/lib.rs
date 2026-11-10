//! Virtual File System (`sqlite3_vfs`, `sqlite3_file`, and protocol-safe `WalShm`) ported from
//! `sqlite/src/os.h`, `sqlite/src/os.c`, `sqlite/src/os_unix.c`, `sqlite/src/os_win.c`,
//! `sqlite/src/wal.c`, and `sqlite/src/memdb.c` for SQLite 3.54.1.
#![deny(unsafe_code, unsafe_op_in_unsafe_fn)]

mod sys {
    #[cfg(target_os = "macos")]
    pub(crate) mod macos;
    #[cfg(unix)]
    pub(crate) mod unix;
    #[cfg(windows)]
    pub(crate) mod windows;
}

pub mod shm;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqliters_types::{ErrorCode, LockGeometry, Result, SqliteError, extended};

pub use shm::{
    CheckpointMode, CheckpointTxnGuard, CkptRecoveryGuard, HASHTABLE_HASH_PRIME, HASHTABLE_NPAGE,
    HASHTABLE_NPAGE_ONE, HASHTABLE_NSLOT, InitRecoveryGuard, MapView, OwnedInitWriteGuard,
    OwnedWalReadGuard, OwnedWriterTxnGuard, READMARK_NOT_USED, ReadonlyHeaderProbe,
    RecoveryTxnGuard, SHM_CHUNK_SIZE, ShmChunk, ShmDmsState, ShmError, ShmLockKind, ShmOpenOptions,
    WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET, WAL_CKPT_INFO_OFFSET, WAL_CKPT_LOCK,
    WAL_CKPT_LOCK_ARRAY_OFFSET, WAL_CKPT_NBACKFILL_OFFSET, WAL_CKPT_READMARK_OFFSET,
    WAL_CKPT_RESERVED_OFFSET, WAL_LOCK_LEN, WAL_LOCK_OFFSET, WAL_NREADER, WAL_READ_LOCK_BASE,
    WAL_RECOVER_LOCK, WAL_SHM_DMS_OFFSET, WAL_SHM_NLOCK, WAL_WRITE_LOCK, WALINDEX_HDR_COPY_LEN,
    WALINDEX_HDR_COPY1_OFFSET, WALINDEX_HDR_COPY2_OFFSET, WALINDEX_HDR_TOTAL_SIZE,
    WALINDEX_MAX_VERSION, WalChecksumEndian, WalCkptGuard, WalCkptInfoSnapshot, WalIndexHeaderData,
    WalLockMode, WalLockState, WalReadGuard, WalShm, WalShmOps, WalWriteGuard,
    checked_region_byte_range, shm_barrier, wal_checksum_bytes, wal_checksum_with_endian,
    wal_frame_page, wal_hash_slot, wal_lock_byte_offset, wal_read_lock_slot,
};

/// Device characteristic bit indicating writes occur sequentially (`SQLITE_IOCAP_SEQUENTIAL`).
pub const IOCAP_SEQUENTIAL: u32 = 0x0000_0400;
/// Device characteristic bit indicating power-safe overwrites (`SQLITE_IOCAP_POWERSAFE_OVERWRITE`).
pub const IOCAP_POWERSAFE_OVERWRITE: u32 = 0x0000_1000;

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

impl LockLevel {
    /// Lowercase SQLite diagnostic lock name (`PRAGMA lock_status` in `sqlite/src/pragma.c`).
    pub const fn as_diagnostic_str(self) -> &'static str {
        match self {
            Self::NoLock => "unlocked",
            Self::Shared => "shared",
            Self::Reserved => "reserved",
            Self::Pending => "pending",
            Self::Exclusive => "exclusive",
        }
    }
}

/// Flags passed to [`Vfs::open`] (`SQLITE_OPEN_*` in `sqlite/src/sqlite.h.in` lines 603-629).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OpenFlags(pub u32);

impl OpenFlags {
    pub const READONLY: Self = Self(0x0000_0001);
    pub const READWRITE: Self = Self(0x0000_0002);
    pub const CREATE: Self = Self(0x0000_0004);
    pub const DELETEONCLOSE: Self = Self(0x0000_0008);
    pub const EXCLUSIVE: Self = Self(0x0000_0010);
    pub const AUTOPROXY: Self = Self(0x0000_0020);
    pub const URI: Self = Self(0x0000_0040);
    pub const MEMORY: Self = Self(0x0000_0080);
    pub const MAIN_DB: Self = Self(0x0000_0100);
    pub const TEMP_DB: Self = Self(0x0000_0200);
    pub const TRANSIENT_DB: Self = Self(0x0000_0400);
    pub const MAIN_JOURNAL: Self = Self(0x0000_0800);
    pub const TEMP_JOURNAL: Self = Self(0x0000_1000);
    pub const SUBJOURNAL: Self = Self(0x0000_2000);
    pub const SUPER_JOURNAL: Self = Self(0x0000_4000);
    /// Legacy alias for [`Self::SUPER_JOURNAL`] (`SQLITE_OPEN_MASTER_JOURNAL` in `sqlite.h.in:628`).
    pub const MASTER_JOURNAL: Self = Self::SUPER_JOURNAL;
    pub const NOMUTEX: Self = Self(0x0000_8000);
    pub const FULLMUTEX: Self = Self(0x0001_0000);
    pub const SHAREDCACHE: Self = Self(0x0002_0000);
    pub const PRIVATECACHE: Self = Self(0x0004_0000);
    pub const WAL: Self = Self(0x0008_0000);
    pub const NOFOLLOW: Self = Self(0x0100_0000);
    pub const EXRESCODE: Self = Self(0x0200_0000);

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub const fn is_readonly(self) -> bool {
        self.contains(Self::READONLY) && !self.contains(Self::READWRITE)
    }

    #[inline]
    pub const fn is_main_journal(self) -> bool {
        self.contains(Self::MAIN_JOURNAL)
    }

    #[inline]
    pub const fn is_super_journal(self) -> bool {
        self.contains(Self::SUPER_JOURNAL)
    }

    #[inline]
    pub const fn is_wal(self) -> bool {
        self.contains(Self::WAL)
    }

    /// Returns whether opening with `self` requires a one-shot parent-directory sync on the first
    /// [`VfsFile::sync`] call (`isNewJrnl` / `UNIXFILE_DIRSYNC` in `sqlite/src/os_unix.c:6557-6561`).
    #[inline]
    pub const fn requires_initial_dir_sync(self) -> bool {
        !self.is_readonly()
            && !self.contains(Self::DELETEONCLOSE)
            && self.contains(Self::CREATE)
            && (self.contains(Self::MAIN_JOURNAL)
                || self.contains(Self::SUPER_JOURNAL)
                || self.contains(Self::WAL))
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

    /// Queries lock state for `SQLITE_FCNTL_LOCKSTATE` / `PRAGMA lock_status`:
    /// returns `Ok(None)` when the VFS does not implement diagnostic lock reporting (matching
    /// `memdb`'s `SQLITE_NOTFOUND` -> `"unknown"`), `Ok(Some(level))` on OS files, or `Err` on
    /// closed/faulted handles.
    fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
        Ok(Some(self.lock_level()))
    }

    /// Explicitly releases locks, closes (or defers closing under active POSIX locks) the
    /// underlying OS handle, applies `DELETEONCLOSE`, and surfaces any unlock/close/unlink error
    /// (`xClose`). `Drop` serves only as the final safety net.
    fn close(&mut self) -> Result<()> {
        self.unlock(LockLevel::NoLock)
    }

    /// Opens a WAL-index (`-shm`) session bound to this open `.db` file handle's verified
    /// physical identity.
    fn open_wal_shm(&self) -> Result<WalShm> {
        self.open_wal_shm_with_options(ShmOpenOptions::default())
    }

    /// Opens a WAL-index (`-shm`) session bound to this open `.db` handle with [`ShmOpenOptions`].
    fn open_wal_shm_with_options(&self, _options: ShmOpenOptions) -> Result<WalShm> {
        Err(SqliteError::with_extended(
            extended::SQLITE_IOERR_SHMOPEN,
            "VFS file handle does not support WAL shared memory",
        ))
    }

    /// Returns whether this file handle implements WAL shared-memory primitives (`sqlite3PagerWalSupported`).
    fn supports_wal_shm(&self) -> bool {
        false
    }

    /// Returns device characteristics bitmask (`xDeviceCharacteristics`, default `0`).
    fn device_characteristics(&self) -> u32 {
        0
    }

    /// Returns device sector size in bytes (`xSectorSize`, default 4096).
    fn sector_size(&self) -> u32 {
        4096
    }

    /// Whether this handle was opened read-only.
    fn is_readonly(&self) -> bool;
}

/// Operating system VFS interface corresponding to `sqlite3_vfs`.
pub trait Vfs: Send + Sync {
    /// Name of this VFS (e.g. `"unix"`, `"win32"`, or `"memdb"`).
    fn name(&self) -> &str;

    /// Byte-range lock geometry configured for this VFS (`LockGeometry::PRODUCTION` by default).
    fn lock_geometry(&self) -> LockGeometry {
        LockGeometry::PRODUCTION
    }

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
// Shared in-process lock table helper for 8-slot WAL lock arrays (`0..8`)
// ============================================================================

#[derive(Default)]
struct InProcessShmLocks {
    /// Per-slot state (`0` = unlocked, `-1` = exclusive, `> 0` = shared holder count).
    slots: [i32; WAL_SHM_NLOCK as usize],
    /// Per-connection `(shared_mask, excl_mask)`.
    connections: HashMap<u64, (u16, u16)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalLockAction {
    AcquireOsLock,
    ReuseInProcessShared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalUnlockAction {
    ReleaseOsLock,
    DecrementInProcessSharedOnly,
}

impl InProcessShmLocks {
    fn register_connection(&mut self, conn_id: u64) {
        self.connections.entry(conn_id).or_insert((0, 0));
    }

    fn precheck_lock(
        &self,
        conn_id: u64,
        lock_idx: u8,
        kind: ShmLockKind,
    ) -> Result<LocalLockAction> {
        if lock_idx >= WAL_SHM_NLOCK {
            return Err(SqliteError::misuse("WAL lock index out of range"));
        }
        let bit = 1u16 << lock_idx;
        let &(shared_mask, excl_mask) = self
            .connections
            .get(&conn_id)
            .ok_or_else(|| SqliteError::misuse("unregistered WAL-index connection"))?;
        if (shared_mask & bit) != 0 || (excl_mask & bit) != 0 {
            return Err(SqliteError::misuse(
                "duplicate or direct shared<->exclusive WAL lock transition on same connection",
            ));
        }

        let slot_state = self.slots[usize::from(lock_idx)];
        match kind {
            ShmLockKind::Shared => {
                if slot_state < 0 {
                    Err(SqliteError::new(
                        ErrorCode::Busy,
                        "WAL lock held exclusively",
                    ))
                } else if slot_state == 0 {
                    Ok(LocalLockAction::AcquireOsLock)
                } else {
                    Ok(LocalLockAction::ReuseInProcessShared)
                }
            }
            ShmLockKind::Exclusive => {
                if slot_state != 0 {
                    Err(SqliteError::new(ErrorCode::Busy, "WAL lock is busy"))
                } else {
                    Ok(LocalLockAction::AcquireOsLock)
                }
            }
        }
    }

    fn commit_lock(&mut self, conn_id: u64, lock_idx: u8, kind: ShmLockKind) {
        let bit = 1u16 << lock_idx;
        if let Some((shared_mask, excl_mask)) = self.connections.get_mut(&conn_id) {
            match kind {
                ShmLockKind::Shared => {
                    *shared_mask |= bit;
                    self.slots[usize::from(lock_idx)] += 1;
                }
                ShmLockKind::Exclusive => {
                    *excl_mask |= bit;
                    self.slots[usize::from(lock_idx)] = -1;
                }
            }
        }
    }

    fn precheck_unlock(
        &self,
        conn_id: u64,
        lock_idx: u8,
        kind: ShmLockKind,
    ) -> Result<LocalUnlockAction> {
        if lock_idx >= WAL_SHM_NLOCK {
            return Err(SqliteError::misuse("WAL lock index out of range"));
        }
        let bit = 1u16 << lock_idx;
        let &(shared_mask, excl_mask) = self
            .connections
            .get(&conn_id)
            .ok_or_else(|| SqliteError::misuse("unregistered WAL-index connection"))?;
        let slot_state = self.slots[usize::from(lock_idx)];
        match kind {
            ShmLockKind::Shared => {
                if (shared_mask & bit) == 0 || slot_state < 1 {
                    return Err(SqliteError::misuse(
                        "connection does not hold shared WAL lock",
                    ));
                }
                if slot_state > 1 {
                    Ok(LocalUnlockAction::DecrementInProcessSharedOnly)
                } else {
                    Ok(LocalUnlockAction::ReleaseOsLock)
                }
            }
            ShmLockKind::Exclusive => {
                if (excl_mask & bit) == 0 || slot_state != -1 {
                    return Err(SqliteError::misuse(
                        "connection does not hold exclusive WAL lock",
                    ));
                }
                Ok(LocalUnlockAction::ReleaseOsLock)
            }
        }
    }

    fn commit_unlock(&mut self, conn_id: u64, lock_idx: u8, kind: ShmLockKind) {
        let bit = 1u16 << lock_idx;
        if let Some((shared_mask, excl_mask)) = self.connections.get_mut(&conn_id) {
            match kind {
                ShmLockKind::Shared => {
                    *shared_mask &= !bit;
                    self.slots[usize::from(lock_idx)] -= 1;
                }
                ShmLockKind::Exclusive => {
                    *excl_mask &= !bit;
                    self.slots[usize::from(lock_idx)] = 0;
                }
            }
        }
    }

    fn unregister_connection(&mut self, conn_id: u64) -> usize {
        if let Some((shared_mask, excl_mask)) = self.connections.remove(&conn_id) {
            for idx in 0..(WAL_SHM_NLOCK as usize) {
                let bit = 1u16 << idx;
                if (excl_mask & bit) != 0 && self.slots[idx] == -1 {
                    self.slots[idx] = 0;
                } else if (shared_mask & bit) != 0 && self.slots[idx] > 0 {
                    self.slots[idx] -= 1;
                }
            }
        }
        self.connections.len()
    }
}

// ============================================================================
// In-Memory VFS (`memdb.c` port with full per-handle lock + safe `WalShm`)
// ============================================================================

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct MemLockState {
    shared_holders: HashSet<u64>,
    writer: Option<(u64, LockLevel)>,
    n_open_handles: usize,
    lock_geometry: Option<LockGeometry>,
}

struct MemShmNodeInner {
    locks: InProcessShmLocks,
    dms_initialized: bool,
    views: Vec<(Arc<MapView>, Arc<MapView>)>,
    allocated_regions: u32,
    regions_per_view: usize,
}

struct MemShmNode {
    inner: Mutex<MemShmNodeInner>,
}

impl MemShmNode {
    fn new(min_granularity: usize) -> Self {
        let gran = min_granularity.max(SHM_CHUNK_SIZE);
        let regions_per_view = (gran / SHM_CHUNK_SIZE).max(1);
        Self {
            inner: Mutex::new(MemShmNodeInner {
                locks: InProcessShmLocks::default(),
                dms_initialized: false,
                views: Vec::new(),
                allocated_regions: 0,
                regions_per_view,
            }),
        }
    }
}

struct MemShmSession {
    node: Arc<MemShmNode>,
    entry: Arc<MemEntry>,
    readonly: bool,
}

impl shm::ShmBackend for MemShmSession {
    fn is_readonly(&self) -> bool {
        self.readonly
    }

    fn dms_state(&self) -> ShmDmsState {
        let Ok(guard) = self.node.inner.lock() else {
            return ShmDmsState::ReadonlyCantInit;
        };
        if guard.dms_initialized {
            ShmDmsState::ActiveShared
        } else {
            ShmDmsState::ReadonlyCantInit
        }
    }

    fn ensure_dms_ready(&self) -> Result<()> {
        let mut guard = self
            .node
            .inner
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode"))?;
        if guard.dms_initialized {
            return Ok(());
        }
        if self.readonly {
            return Err(SqliteError::with_extended(
                extended::SQLITE_READONLY_CANTINIT,
                "cannot initialize read-only in-memory WAL-index",
            ));
        }
        guard.dms_initialized = true;
        guard.views.clear();
        guard.allocated_regions = 0;
        Ok(())
    }

    fn lock_slot(&self, connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()> {
        let mut guard = self
            .node
            .inner
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode"))?;
        let _ = guard.locks.precheck_lock(connection_id, lock_idx, kind)?;
        guard.locks.commit_lock(connection_id, lock_idx, kind);
        Ok(())
    }

    fn unlock_slot(&self, connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()> {
        let mut guard = self
            .node
            .inner
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode"))?;
        let _ = guard.locks.precheck_unlock(connection_id, lock_idx, kind)?;
        guard.locks.commit_unlock(connection_id, lock_idx, kind);
        Ok(())
    }

    fn map_region(&self, region: u32, extend: bool) -> Result<Option<ShmChunk>> {
        let _ = checked_region_byte_range(region, 1)?;
        let mut guard = self
            .node
            .inner
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode"))?;
        if region >= guard.allocated_regions {
            if !extend {
                return Ok(None);
            }
            if self.readonly {
                return Err(SqliteError::readonly(
                    "cannot extend read-only in-memory WAL-index",
                ));
            }
            let rpv = guard.regions_per_view as u32;
            let req_regions = ((region + rpv) / rpv) * rpv;
            let req_views = (req_regions / rpv) as usize;
            let view_bytes = guard.regions_per_view * SHM_CHUNK_SIZE;
            while guard.views.len() < req_views {
                let words = Arc::new(shm::mapping::MemRegionWords::new_zeroed(view_bytes)?);
                let rw_view = Arc::new(MapView::from_memory(Arc::clone(&words), true));
                let ro_view = Arc::new(MapView::from_memory(words, false));
                guard.views.push((rw_view, ro_view));
            }
            guard.allocated_regions = req_regions;
        }

        let rpv = guard.regions_per_view;
        let view_idx = (region as usize) / rpv;
        let sub_idx = (region as usize) % rpv;
        let subrange_offset = sub_idx * SHM_CHUNK_SIZE;
        let view = if self.readonly {
            Arc::clone(&guard.views[view_idx].1)
        } else {
            Arc::clone(&guard.views[view_idx].0)
        };
        drop(guard);

        Ok(Some(ShmChunk::try_new(view, subrange_offset, region)?))
    }

    fn mapped_region_count(&self) -> Result<u32> {
        let guard = self
            .node
            .inner
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode"))?;
        Ok(guard.allocated_regions)
    }

    fn close_session(&self, connection_id: u64, delete_on_last: bool) -> Result<()> {
        let mut dropped_views = Vec::new();
        {
            // Strict lock order: `entry.shm_node` -> `node.inner` (matching `open_wal_shm_with_options`)
            // so a concurrent `open_wal_shm_with_options` can never attach to a node mid-teardown.
            let mut shm_slot =
                self.entry.shm_node.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned entry shm mutex")
                })?;
            let mut guard = self
                .node
                .inner
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode"))?;
            let rem = guard.locks.unregister_connection(connection_id);
            if rem == 0 {
                guard.dms_initialized = false;
                if delete_on_last {
                    dropped_views = std::mem::take(&mut guard.views);
                    guard.allocated_regions = 0;
                    if let Some(ref cur) = *shm_slot
                        && Arc::ptr_eq(cur, &self.node)
                    {
                        *shm_slot = None;
                    }
                }
            }
        }
        // Drop region buffers outside both `entry.shm_node` and `node.inner` mutexes.
        drop(dropped_views);
        Ok(())
    }
}

struct MemEntry {
    data: RwLock<Vec<u8>>,
    lock: Mutex<MemLockState>,
    shm_node: Mutex<Option<Arc<MemShmNode>>>,
}

impl MemEntry {
    fn new() -> Self {
        Self {
            data: RwLock::new(Vec::new()),
            lock: Mutex::new(MemLockState::default()),
            shm_node: Mutex::new(None),
        }
    }
}

struct MemFile {
    id: u64,
    entry: Arc<MemEntry>,
    readonly: bool,
    min_shm_granularity: usize,
    held_lock: Mutex<LockLevel>,
    closed: Mutex<bool>,
}

impl MemFile {
    fn ensure_open(&self) -> Result<()> {
        let is_closed = self
            .closed
            .lock()
            .map(|g| *g)
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned closed lock"))?;
        if is_closed {
            return Err(SqliteError::misuse("memdb file handle already closed"));
        }
        Ok(())
    }
}

impl Drop for MemFile {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl VfsFile for MemFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.ensure_open()?;
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
        self.ensure_open()?;
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
        self.ensure_open()?;
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
        self.ensure_open()
    }

    fn file_size(&self) -> Result<u64> {
        self.ensure_open()?;
        let guard = self
            .entry
            .data
            .read()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memdb lock"))?;
        Ok(guard.len() as u64)
    }

    fn lock(&self, level: LockLevel) -> Result<()> {
        self.ensure_open()?;
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
        self.ensure_open()?;
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
        self.ensure_open()?;
        let state = self
            .entry
            .lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
        Ok(state.writer.is_some())
    }

    fn lock_level(&self) -> LockLevel {
        self.held_lock
            .lock()
            .map(|g| *g)
            .unwrap_or(LockLevel::NoLock)
    }

    fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
        self.ensure_open()?;
        // Matches `memdbFileControl` returning `SQLITE_NOTFOUND` -> `PRAGMA lock_status` `"unknown"`.
        Ok(None)
    }

    fn close(&mut self) -> Result<()> {
        let already_closed = {
            let g = self
                .closed
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned closed lock"))?;
            *g
        };
        if already_closed {
            return Ok(());
        }
        let unlock_res = self.unlock(LockLevel::NoLock);
        {
            let mut g = self
                .closed
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned closed lock"))?;
            *g = true;
        }
        {
            let mut state = self
                .entry
                .lock
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned lock"))?;
            state.n_open_handles = state.n_open_handles.saturating_sub(1);
            if state.n_open_handles == 0 {
                state.lock_geometry = None;
            }
        }
        unlock_res
    }

    fn open_wal_shm_with_options(&self, options: ShmOpenOptions) -> Result<WalShm> {
        self.ensure_open()?;
        let shm_readonly = self.readonly || options.readonly_shm;
        let conn_id = NEXT_CONNECTION_ID.fetch_add(1, AtomicOrdering::Relaxed);

        let mut views_to_drop_outside = Vec::new();
        let node = {
            // Strict lock order: `entry.shm_node` -> `node.inner` (matching `MemShmSession::close_session`)
            // so DMS initialization + connection registration is atomic with `entry.shm_node` lookup.
            let mut slot =
                self.entry.shm_node.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned entry shm mutex")
                })?;
            match *slot {
                Some(ref existing) => {
                    let node = Arc::clone(existing);
                    let mut inner = node.inner.lock().map_err(|_| {
                        SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode")
                    })?;
                    if !inner.dms_initialized {
                        if shm_readonly {
                            if !options.allow_unreliable_readonly {
                                return Err(SqliteError::with_extended(
                                    extended::SQLITE_READONLY_CANTINIT,
                                    "cannot initialize read-only in-memory WAL-index",
                                ));
                            }
                        } else {
                            inner.dms_initialized = true;
                            views_to_drop_outside = std::mem::take(&mut inner.views);
                            inner.allocated_regions = 0;
                        }
                    }
                    inner.locks.register_connection(conn_id);
                    drop(inner);
                    node
                }
                None => {
                    if shm_readonly && !options.allow_unreliable_readonly {
                        return Err(SqliteError::with_extended(
                            extended::SQLITE_READONLY_CANTINIT,
                            "cannot initialize read-only in-memory WAL-index",
                        ));
                    }
                    let created = Arc::new(MemShmNode::new(self.min_shm_granularity));
                    {
                        let mut inner = created.inner.lock().map_err(|_| {
                            SqliteError::new(ErrorCode::Internal, "poisoned MemShmNode")
                        })?;
                        if !shm_readonly {
                            inner.dms_initialized = true;
                        }
                        inner.locks.register_connection(conn_id);
                    }
                    *slot = Some(Arc::clone(&created));
                    created
                }
            }
        };
        drop(views_to_drop_outside);

        let session = Arc::new(MemShmSession {
            node,
            entry: Arc::clone(&self.entry),
            readonly: shm_readonly,
        });
        Ok(WalShm::new(conn_id, session))
    }

    fn is_readonly(&self) -> bool {
        self.readonly
    }

    fn supports_wal_shm(&self) -> bool {
        true
    }
}

/// In-memory VFS supporting anonymous (`None` / `":memory:"`) and named files (e.g. journals & WAL).
pub struct MemoryVfs {
    files: RwLock<HashMap<PathBuf, Arc<MemEntry>>>,
    lock_geometry: LockGeometry,
    min_shm_granularity: usize,
}

impl Default for MemoryVfs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryVfs {
    pub fn new() -> Self {
        Self::with_lock_geometry(LockGeometry::PRODUCTION)
    }

    pub fn with_lock_geometry(lock_geometry: LockGeometry) -> Self {
        Self {
            files: RwLock::new(HashMap::new()),
            lock_geometry,
            min_shm_granularity: SHM_CHUNK_SIZE,
        }
    }

    /// Configures minimum `MapView` granularity in bytes (e.g. `65_536` to test 64 KiB page chunk
    /// grouping where two 32 KiB [`ShmChunk`]s share one [`MapView`]).
    pub fn with_min_shm_granularity(mut self, min_shm_granularity: usize) -> Self {
        assert!(
            min_shm_granularity >= SHM_CHUNK_SIZE && min_shm_granularity.is_power_of_two(),
            "min_shm_granularity must be a power of two >= 32768"
        );
        self.min_shm_granularity = min_shm_granularity;
        self
    }

    /// Shares `entry_from` under `alias_path` (used in tests to simulate hardlinks/aliases).
    pub fn create_hardlink(&self, existing: &Path, alias: &Path) -> Result<()> {
        let mut map = self
            .files
            .write()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memvfs"))?;
        let entry = map
            .get(existing)
            .cloned()
            .ok_or_else(|| SqliteError::new(ErrorCode::CantOpen, "source file does not exist"))?;
        map.insert(alias.to_path_buf(), entry);
        Ok(())
    }

    /// Opens `path` using a caller-supplied `override_geometry` against this `MemoryVfs`'s shared
    /// file table (used to test conflicting `LockGeometry` rejection on the same active file).
    pub fn open_with_lock_geometry(
        &self,
        path: Option<&Path>,
        flags: OpenFlags,
        override_geometry: LockGeometry,
    ) -> Result<Box<dyn VfsFile>> {
        self.open_internal(path, flags, override_geometry)
    }

    fn open_internal(
        &self,
        path: Option<&Path>,
        flags: OpenFlags,
        geometry: LockGeometry,
    ) -> Result<Box<dyn VfsFile>> {
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

        {
            let mut state = entry
                .lock
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned memdb lock"))?;
            if state.n_open_handles > 0
                && let Some(active_geom) = state.lock_geometry
                && active_geom != geometry
            {
                return Err(SqliteError::misuse(format!(
                    "conflicting LockGeometry on active in-memory file ({:#x} vs {:#x})",
                    active_geom.pending_byte_offset(),
                    geometry.pending_byte_offset()
                )));
            }
            state.lock_geometry = Some(geometry);
            state.n_open_handles += 1;
        }

        Ok(Box::new(MemFile {
            id: NEXT_CONNECTION_ID.fetch_add(1, AtomicOrdering::Relaxed),
            entry,
            readonly,
            min_shm_granularity: self.min_shm_granularity,
            held_lock: Mutex::new(LockLevel::NoLock),
            closed: Mutex::new(false),
        }))
    }
}

impl Vfs for MemoryVfs {
    fn name(&self) -> &str {
        "memdb"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.lock_geometry
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        self.open_internal(path, flags, self.lock_geometry)
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
// Unix Central Inode & Shared `UnixShmNode` Manager (`sqlite/src/os_unix.c`)
// ============================================================================

#[cfg(unix)]
struct UnixShmNodeState {
    locks: InProcessShmLocks,
    is_unlocked: bool,
    chunks: Vec<ShmChunk>,
    regions_per_map: usize,
}

#[cfg(unix)]
struct UnixShmNode {
    db_inode_key: (u64, u64),
    db_inode_info: Arc<Mutex<UnixInodeInfo>>,
    shm_init_lock: Arc<Mutex<()>>,
    backing: Arc<sys::unix::SysShmBacking>,
    state: Mutex<UnixShmNodeState>,
}

#[cfg(unix)]
impl UnixShmNode {
    fn ensure_dms_locked(&self) -> Result<()> {
        use std::os::fd::AsRawFd;

        let mut guard = self
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned UnixShmNode"))?;
        if !guard.is_unlocked {
            return Ok(());
        }

        let res = self.backing.with_file(|file| {
            let fd = file.as_raw_fd();
            let lock_type = sys::unix::get_posix_lock_type(
                fd,
                sys::unix::UNIX_SHM_DMS,
                1,
                extended::SQLITE_IOERR_LOCK,
            )?;
            if lock_type == sys::unix::F_WRLCK {
                return Err(SqliteError::new(
                    ErrorCode::Busy,
                    "WAL-index DMS locked exclusively by another process",
                ));
            }
            if lock_type == sys::unix::F_UNLCK {
                if self.backing.is_readonly() {
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_READONLY_CANTINIT,
                        "cannot initialize read-only WAL-index (-shm)",
                    ));
                }
                sys::unix::set_posix_lock(
                    fd,
                    sys::unix::F_WRLCK,
                    sys::unix::UNIX_SHM_DMS,
                    1,
                    extended::SQLITE_IOERR_SHMLOCK,
                )?;
                if let Err(e) = sys::unix::ftruncate_fd(fd, 3, extended::SQLITE_IOERR_SHMOPEN) {
                    let _ = sys::unix::set_posix_lock(
                        fd,
                        sys::unix::F_UNLCK,
                        sys::unix::UNIX_SHM_DMS,
                        1,
                        extended::SQLITE_IOERR_SHMLOCK,
                    );
                    return Err(e);
                }
            }
            sys::unix::set_posix_lock(
                fd,
                sys::unix::F_RDLCK,
                sys::unix::UNIX_SHM_DMS,
                1,
                extended::SQLITE_IOERR_SHMLOCK,
            )
        });

        res?;
        guard.is_unlocked = false;
        Ok(())
    }
}

#[cfg(unix)]
struct UnixShmSession {
    node: Arc<UnixShmNode>,
}

#[cfg(unix)]
impl shm::ShmBackend for UnixShmSession {
    fn is_readonly(&self) -> bool {
        self.node.backing.is_readonly()
    }

    fn dms_state(&self) -> ShmDmsState {
        let Ok(guard) = self.node.state.lock() else {
            return ShmDmsState::ReadonlyCantInit;
        };
        if guard.is_unlocked {
            ShmDmsState::ReadonlyCantInit
        } else {
            ShmDmsState::ActiveShared
        }
    }

    fn ensure_dms_ready(&self) -> Result<()> {
        self.node.ensure_dms_locked()
    }

    fn lock_slot(&self, connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()> {
        use std::os::fd::AsRawFd;

        let mut guard = self
            .node
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned UnixShmNode"))?;
        let action = guard.locks.precheck_lock(connection_id, lock_idx, kind)?;
        if action == LocalLockAction::AcquireOsLock {
            let l_type = match kind {
                ShmLockKind::Shared => sys::unix::F_RDLCK,
                ShmLockKind::Exclusive => sys::unix::F_WRLCK,
            };
            let offset = sys::unix::UNIX_SHM_BASE + i64::from(lock_idx);
            self.node.backing.with_file(|file| {
                sys::unix::set_posix_lock(
                    file.as_raw_fd(),
                    l_type,
                    offset,
                    1,
                    extended::SQLITE_IOERR_SHMLOCK,
                )
            })?;
        }
        guard.locks.commit_lock(connection_id, lock_idx, kind);
        Ok(())
    }

    fn unlock_slot(&self, connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()> {
        use std::os::fd::AsRawFd;

        let mut guard = self
            .node
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned UnixShmNode"))?;
        let action = guard.locks.precheck_unlock(connection_id, lock_idx, kind)?;
        if action == LocalUnlockAction::ReleaseOsLock {
            let offset = sys::unix::UNIX_SHM_BASE + i64::from(lock_idx);
            self.node.backing.with_file(|file| {
                sys::unix::set_posix_lock(
                    file.as_raw_fd(),
                    sys::unix::F_UNLCK,
                    offset,
                    1,
                    extended::SQLITE_IOERR_SHMLOCK,
                )
            })?;
        }
        guard.locks.commit_unlock(connection_id, lock_idx, kind);
        Ok(())
    }

    fn map_region(&self, region: u32, extend: bool) -> Result<Option<ShmChunk>> {
        let _ = checked_region_byte_range(region, 1)?;
        self.node.ensure_dms_locked()?;

        let mut guard = self
            .node
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned UnixShmNode"))?;

        let region_usize = region as usize;
        if region_usize < guard.chunks.len() {
            return Ok(Some(guard.chunks[region_usize].clone()));
        }

        let n_per_map = guard.regions_per_map;
        let req_regions = ((region_usize + n_per_map) / n_per_map) * n_per_map;
        let (_, req_total_bytes_usize) = checked_region_byte_range(0, req_regions)?;
        let req_total_bytes = req_total_bytes_usize as u64;

        let file_large_enough = self.node.backing.with_file(|file| {
            let cur_size = file
                .metadata()
                .map_err(|e| {
                    SqliteError::with_extended(extended::SQLITE_IOERR_SHMSIZE, e.to_string())
                })?
                .len();
            if cur_size < req_total_bytes {
                if !extend {
                    return Ok(false);
                }
                if self.node.backing.is_readonly() {
                    return Err(SqliteError::readonly("cannot extend read-only -shm file"));
                }
                // Matches `unixShmMap` (`os_unix.c:5180-5187`): write 1 zero byte to the end of
                // each newly allocated 4096-byte page so disk blocks are materialized immediately,
                // reducing `SIGBUS` hazard on sparse filesystems!
                const ALLOC_PAGE: u64 = 4096;
                let mut page_idx = cur_size / ALLOC_PAGE;
                let end_page = req_total_bytes / ALLOC_PAGE;
                while page_idx < end_page {
                    let byte_pos = page_idx * ALLOC_PAGE + (ALLOC_PAGE - 1);
                    sys::unix::pwrite_all(file, &[0u8], byte_pos, extended::SQLITE_IOERR_SHMSIZE)?;
                    page_idx += 1;
                }
            }
            Ok(true)
        })?;

        if !file_large_enough {
            return Ok(None);
        }

        let writable = !self.node.backing.is_readonly();
        while guard.chunks.len() < req_regions {
            let base_region = guard.chunks.len() as u32;
            let (file_offset, map_bytes) = checked_region_byte_range(base_region, n_per_map)?;
            let sys_view = sys::unix::SysMapView::map(
                Arc::clone(&self.node.backing),
                file_offset,
                map_bytes,
                writable,
            )?;
            let map_view = Arc::new(MapView::from_os(sys_view));
            for sub in 0..n_per_map {
                let chunk = ShmChunk::try_new(
                    Arc::clone(&map_view),
                    sub * SHM_CHUNK_SIZE,
                    base_region + (sub as u32),
                )?;
                guard.chunks.push(chunk);
            }
        }

        Ok(Some(guard.chunks[region_usize].clone()))
    }

    fn mapped_region_count(&self) -> Result<u32> {
        let guard = self
            .node
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned UnixShmNode"))?;
        Ok(guard.chunks.len() as u32)
    }

    fn close_session(&self, connection_id: u64, delete_on_last: bool) -> Result<()> {
        use std::os::fd::AsRawFd;

        // Strict lock order: `shm_init_lock` -> (`UnixInodeInfo` OR `UnixShmNode.state`).
        // Holding `shm_init_lock` across `remaining == 0`, `inode.shm_node.take()`,
        // `has_active_shm_node = false`, and `close_explicit` prevents any concurrent
        // `open_wal_shm_with_options` from attaching to a closing `UnixShmNode`.
        let shm_init_guard = self
            .node
            .shm_init_lock
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned shm init mutex"))?;

        let mut chunks_to_drop: Vec<ShmChunk> = Vec::new();
        let mut release_os_masks: Vec<u8> = Vec::new();
        let remaining = {
            let mut guard = self
                .node
                .state
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned UnixShmNode"))?;
            if let Some(&(s_mask, e_mask)) = guard.locks.connections.get(&connection_id) {
                for idx in 0..WAL_SHM_NLOCK {
                    let bit = 1u16 << idx;
                    if (e_mask & bit) != 0
                        || ((s_mask & bit) != 0 && guard.locks.slots[usize::from(idx)] == 1)
                    {
                        release_os_masks.push(idx);
                    }
                }
            }
            let rem = guard.locks.unregister_connection(connection_id);
            if rem == 0 {
                chunks_to_drop = std::mem::take(&mut guard.chunks);
            }
            rem
        };

        let mut first_err: Option<SqliteError> = None;
        for idx in release_os_masks {
            let offset = sys::unix::UNIX_SHM_BASE + i64::from(idx);
            if let Err(e) = self.node.backing.with_file(|file| {
                sys::unix::set_posix_lock(
                    file.as_raw_fd(),
                    sys::unix::F_UNLCK,
                    offset,
                    1,
                    extended::SQLITE_IOERR_SHMLOCK,
                )
            }) && first_err.is_none()
            {
                first_err = Some(e);
            }
        }

        // Drop `ShmChunk`s OUTSIDE the `UnixShmNode` mutex so `munmap` runs before FD closes!
        drop(chunks_to_drop);

        let mut prune_targets = None;
        if remaining == 0 {
            let shm_inode_key = self.node.backing.inode_key();
            if delete_on_last {
                self.node.backing.mark_delete_on_close();
            }
            let detach_node = {
                let mut inode =
                    self.node.db_inode_info.lock().map_err(|_| {
                        SqliteError::new(ErrorCode::Internal, "poisoned inode mutex")
                    })?;
                if let Some(ref cur) = inode.shm_node
                    && Arc::ptr_eq(cur, &self.node)
                {
                    inode.shm_node.take()
                } else {
                    None
                }
            };
            // Drop `Arc<UnixShmNode>` OUTSIDE `UnixInodeInfo` mutex!
            drop(detach_node);

            let shm_info = lookup_inode_info(shm_inode_key);
            let mut deferred_shm_fds = Vec::new();
            {
                let mut sg = shm_info.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned shm inode mutex")
                })?;
                sg.has_active_shm_node = false;
                if sg.n_lock == 0 {
                    deferred_shm_fds = std::mem::take(&mut sg.pending_close_fds);
                }
            }

            if Arc::strong_count(&self.node.backing) == 1
                && let Err(e) = self.node.backing.close_explicit(delete_on_last)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
            if let Err(e) = close_files_outside_mutex(deferred_shm_fds)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
            prune_targets = Some((shm_inode_key, shm_info));
        }

        // Release `shm_init_guard` BEFORE calling `maybe_prune_unix_inode`!
        drop(shm_init_guard);

        if let Some((shm_inode_key, shm_info)) = prune_targets {
            maybe_prune_unix_inode(shm_inode_key, &shm_info);
            maybe_prune_unix_inode(self.node.db_inode_key, &self.node.db_inode_info);
        }

        if let Some(err) = first_err {
            return Err(err);
        }
        Ok(())
    }
}

/// Process-wide per-inode lock and WAL-index state (`unixInodeInfo` in `sqlite/src/os_unix.c`).
#[cfg(unix)]
#[derive(Default)]
struct UnixInodeInfo {
    e_file_lock: LockLevel,
    n_shared: usize,
    n_lock: usize,
    n_open_handles: usize,
    lock_geometry: Option<LockGeometry>,
    pending_close_fds: Vec<File>,
    shm_node: Option<Arc<UnixShmNode>>,
    shm_init_lock: Arc<Mutex<()>>,
    has_active_shm_node: bool,
}

#[cfg(unix)]
type UnixInodeTable = HashMap<(u64, u64), Arc<Mutex<UnixInodeInfo>>>;

#[cfg(unix)]
fn inode_table() -> &'static Mutex<UnixInodeTable> {
    static TABLE: OnceLock<Mutex<UnixInodeTable>> = OnceLock::new();
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

#[cfg(unix)]
fn maybe_prune_unix_inode(key: (u64, u64), info: &Arc<Mutex<UnixInodeInfo>>) {
    let mut map = inode_table().lock().unwrap_or_else(|e| e.into_inner());
    let can_prune = if let Ok(guard) = info.lock() {
        guard.n_open_handles == 0
            && guard.n_lock == 0
            && guard.pending_close_fds.is_empty()
            && guard.shm_node.is_none()
            && !guard.has_active_shm_node
            && Arc::strong_count(&guard.shm_init_lock) == 1
    } else {
        false
    };
    if can_prune
        && let Some(existing) = map.get(&key)
        && Arc::ptr_eq(existing, info)
    {
        map.remove(&key);
    }
}

#[cfg(unix)]
fn close_files_outside_mutex(files: Vec<File>) -> Result<()> {
    let mut first_err: Option<SqliteError> = None;
    for f in files {
        if let Err(e) = sys::unix::close_file_fd(f)
            && first_err.is_none()
        {
            first_err = Some(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(unix)]
fn safe_close_shm_fd(shm_inode_key: (u64, u64), file: File) -> Result<()> {
    let shm_info = lookup_inode_info(shm_inode_key);
    let maybe_file = {
        let mut guard = shm_info
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned shm inode mutex"))?;
        if guard.n_lock > 0 || guard.has_active_shm_node {
            guard.pending_close_fds.push(file);
            None
        } else {
            Some(file)
        }
    };
    let res = match maybe_file {
        Some(f) => sys::unix::close_file_fd(f),
        None => Ok(()),
    };
    maybe_prune_unix_inode(shm_inode_key, &shm_info);
    res
}

// ============================================================================
// Windows Central File-ID Registry & Per-HANDLE `WinShmNode` (`os_win.c`)
// ============================================================================

#[cfg(windows)]
#[derive(Default)]
struct WinFileIdentityInfo {
    n_open_handles: usize,
    lock_geometry: Option<LockGeometry>,
    shm_node: Option<Arc<WinShmNode>>,
    shm_init_lock: Arc<Mutex<()>>,
}

#[cfg(windows)]
type WinFileIdentityTable = HashMap<(u64, u64), Arc<Mutex<WinFileIdentityInfo>>>;

#[cfg(windows)]
fn win_file_table() -> &'static Mutex<WinFileIdentityTable> {
    static TABLE: OnceLock<Mutex<WinFileIdentityTable>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(windows)]
fn lookup_win_file_info(key: (u64, u64)) -> Arc<Mutex<WinFileIdentityInfo>> {
    let mut map = win_file_table().lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        map.entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(WinFileIdentityInfo::default()))),
    )
}

#[cfg(windows)]
fn maybe_prune_win_file_info(key: (u64, u64), info: &Arc<Mutex<WinFileIdentityInfo>>) {
    let mut map = win_file_table().lock().unwrap_or_else(|e| e.into_inner());
    let can_prune = if let Ok(guard) = info.lock() {
        guard.n_open_handles == 0
            && guard.shm_node.is_none()
            && Arc::strong_count(&guard.shm_init_lock) == 1
    } else {
        false
    };
    if can_prune
        && let Some(existing) = map.get(&key)
        && Arc::ptr_eq(existing, info)
    {
        map.remove(&key);
    }
}

#[cfg(windows)]
struct WinShmNodeState {
    n_ref: usize,
    is_unlocked: bool,
    chunks: Vec<ShmChunk>,
}

#[cfg(windows)]
struct WinShmNode {
    db_file_id: (u64, u64),
    db_info: Arc<Mutex<WinFileIdentityInfo>>,
    shm_init_lock: Arc<Mutex<()>>,
    backing: Arc<sys::windows::SysShmBacking>,
    state: Mutex<WinShmNodeState>,
}

#[cfg(windows)]
impl WinShmNode {
    fn ensure_dms_locked(&self) -> Result<()> {
        use std::os::windows::io::AsRawHandle;

        let mut guard = self
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmNode"))?;
        if !guard.is_unlocked {
            return Ok(());
        }

        self.backing.with_file(|file| {
            let h = file.as_raw_handle();
            match sys::windows::lock_file_range(
                h,
                sys::windows::WIN_SHM_DMS,
                1,
                true,
                extended::SQLITE_IOERR_SHMLOCK,
            ) {
                Ok(()) => {
                    let init_res = if self.backing.is_readonly() {
                        Err(SqliteError::with_extended(
                            extended::SQLITE_READONLY_CANTINIT,
                            "cannot initialize read-only WAL-index (-shm)",
                        ))
                    } else if sys::windows::truncate_handle(h, 0, extended::SQLITE_IOERR_SHMSIZE)
                        .is_err()
                    {
                        sys::windows::zero_first_eight_bytes(h)
                    } else {
                        Ok(())
                    };
                    let _ = sys::windows::unlock_file_range(
                        h,
                        sys::windows::WIN_SHM_DMS,
                        1,
                        extended::SQLITE_IOERR_SHMLOCK,
                    );
                    init_res?;
                }
                Err(e) if e.code == ErrorCode::Busy => {}
                Err(other) => return Err(other),
            }

            sys::windows::lock_file_range(
                h,
                sys::windows::WIN_SHM_DMS,
                1,
                false,
                extended::SQLITE_IOERR_SHMLOCK,
            )
        })?;

        guard.is_unlocked = false;
        Ok(())
    }
}

/// Windows local model (`bUseSharedLockHandle == 0` in `sqlite/src/os_win.c`):
/// each connection owns its own `lock_file` HANDLE on `-shm` and tracks only its own masks!
#[cfg(windows)]
struct WinShmSession {
    node: Arc<WinShmNode>,
    lock_file: Mutex<Option<File>>,
    held_masks: Mutex<(u16, u16)>,
}

#[cfg(windows)]
impl shm::ShmBackend for WinShmSession {
    fn is_readonly(&self) -> bool {
        self.node.backing.is_readonly()
    }

    fn dms_state(&self) -> ShmDmsState {
        let Ok(guard) = self.node.state.lock() else {
            return ShmDmsState::ReadonlyCantInit;
        };
        if guard.is_unlocked {
            ShmDmsState::ReadonlyCantInit
        } else {
            ShmDmsState::ActiveShared
        }
    }

    fn ensure_dms_ready(&self) -> Result<()> {
        self.node.ensure_dms_locked()
    }

    fn lock_slot(&self, _connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()> {
        use std::os::windows::io::AsRawHandle;

        if lock_idx >= WAL_SHM_NLOCK {
            return Err(SqliteError::misuse("WAL lock index out of range"));
        }
        let bit = 1u16 << lock_idx;
        let mut masks = self
            .held_masks
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmSession masks"))?;
        if (masks.0 & bit) != 0 || (masks.1 & bit) != 0 {
            return Err(SqliteError::misuse(
                "duplicate WAL lock request on same Windows HANDLE",
            ));
        }

        let file_guard = self
            .lock_file
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmSession file"))?;
        let f = file_guard
            .as_ref()
            .ok_or_else(|| SqliteError::misuse("WinShmSession already closed"))?;
        let offset = sys::windows::WIN_SHM_BASE + u64::from(lock_idx);
        sys::windows::lock_file_range(
            f.as_raw_handle(),
            offset,
            1,
            kind == ShmLockKind::Exclusive,
            extended::SQLITE_IOERR_SHMLOCK,
        )?;
        match kind {
            ShmLockKind::Shared => masks.0 |= bit,
            ShmLockKind::Exclusive => masks.1 |= bit,
        }
        Ok(())
    }

    fn unlock_slot(&self, _connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()> {
        use std::os::windows::io::AsRawHandle;

        if lock_idx >= WAL_SHM_NLOCK {
            return Err(SqliteError::misuse("WAL lock index out of range"));
        }
        let bit = 1u16 << lock_idx;
        let mut masks = self
            .held_masks
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmSession masks"))?;
        let held = match kind {
            ShmLockKind::Shared => (masks.0 & bit) != 0,
            ShmLockKind::Exclusive => (masks.1 & bit) != 0,
        };
        if !held {
            return Err(SqliteError::misuse(
                "Windows HANDLE does not hold requested WAL lock",
            ));
        }

        let file_guard = self
            .lock_file
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmSession file"))?;
        let f = file_guard
            .as_ref()
            .ok_or_else(|| SqliteError::misuse("WinShmSession already closed"))?;
        let offset = sys::windows::WIN_SHM_BASE + u64::from(lock_idx);
        sys::windows::unlock_file_range(
            f.as_raw_handle(),
            offset,
            1,
            extended::SQLITE_IOERR_SHMLOCK,
        )?;
        match kind {
            ShmLockKind::Shared => masks.0 &= !bit,
            ShmLockKind::Exclusive => masks.1 &= !bit,
        }
        Ok(())
    }

    fn map_region(&self, region: u32, extend: bool) -> Result<Option<ShmChunk>> {
        use std::os::windows::io::AsRawHandle;

        let _ = checked_region_byte_range(region, 1)?;
        self.node.ensure_dms_locked()?;

        let mut guard = self
            .node
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmNode"))?;
        let region_usize = region as usize;
        if region_usize < guard.chunks.len() {
            return Ok(Some(guard.chunks[region_usize].clone()));
        }

        let (_, min_bytes_usize) = checked_region_byte_range(0, region_usize + 1)?;
        let min_bytes = min_bytes_usize as u64;
        let file_large_enough = self.node.backing.with_file(|file| {
            let sz = file
                .metadata()
                .map_err(|e| {
                    SqliteError::with_extended(extended::SQLITE_IOERR_SHMSIZE, e.to_string())
                })?
                .len();
            if sz < min_bytes {
                if !extend {
                    return Ok(false);
                }
                if self.node.backing.is_readonly() {
                    return Err(SqliteError::readonly("cannot extend read-only -shm file"));
                }
                sys::windows::truncate_handle(
                    file.as_raw_handle(),
                    min_bytes,
                    extended::SQLITE_IOERR_SHMSIZE,
                )?;
            }
            Ok(true)
        })?;
        if !file_large_enough {
            return Ok(None);
        }

        let gran = sys::windows::allocation_granularity();
        let writable = !self.node.backing.is_readonly();
        while guard.chunks.len() <= region_usize {
            let next_region = guard.chunks.len() as u32;
            let (raw_off, _) = checked_region_byte_range(next_region, 1)?;
            let shift = (raw_off as usize) % gran;
            let aligned_off = raw_off - (shift as u64);
            let map_len = SHM_CHUNK_SIZE + shift;
            let sys_view = sys::windows::SysMapView::map(
                Arc::clone(&self.node.backing),
                aligned_off,
                map_len,
                writable,
            )?;
            let view = Arc::new(MapView::from_os(sys_view));
            let chunk = ShmChunk::try_new(view, shift, next_region)?;
            guard.chunks.push(chunk);
        }
        Ok(Some(guard.chunks[region_usize].clone()))
    }

    fn mapped_region_count(&self) -> Result<u32> {
        let guard = self
            .node
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmNode"))?;
        Ok(guard.chunks.len() as u32)
    }

    fn close_session(&self, _connection_id: u64, delete_on_last: bool) -> Result<()> {
        use std::os::windows::io::AsRawHandle;

        // Strict lock order: `shm_init_lock` -> (`WinFileIdentityInfo` OR `WinShmNode.state`),
        // preventing any concurrent `open_wal_shm_with_options` from attaching to a closing node.
        let shm_init_guard =
            self.node.shm_init_lock.lock().map_err(|_| {
                SqliteError::new(ErrorCode::Internal, "poisoned win shm init mutex")
            })?;

        let mut first_err: Option<SqliteError> = None;
        let maybe_lock_file = {
            let mut fg = self.lock_file.lock().map_err(|_| {
                SqliteError::new(ErrorCode::Internal, "poisoned WinShmSession file")
            })?;
            fg.take()
        };
        if let Some(f) = maybe_lock_file {
            if let Ok(mut masks) = self.held_masks.lock() {
                let combined = masks.0 | masks.1;
                for idx in 0..WAL_SHM_NLOCK {
                    if (combined & (1u16 << idx)) != 0 {
                        let _ = sys::windows::unlock_file_range(
                            f.as_raw_handle(),
                            sys::windows::WIN_SHM_BASE + u64::from(idx),
                            1,
                            extended::SQLITE_IOERR_SHMLOCK,
                        );
                    }
                }
                *masks = (0, 0);
            }
            if let Err(e) = sys::windows::close_file_handle(f) {
                first_err = Some(e);
            }
        }

        let mut chunks_to_drop = Vec::new();
        let remaining = {
            let mut guard = self
                .node
                .state
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WinShmNode"))?;
            guard.n_ref = guard.n_ref.saturating_sub(1);
            if guard.n_ref == 0 {
                chunks_to_drop = std::mem::take(&mut guard.chunks);
            }
            guard.n_ref
        };
        drop(chunks_to_drop);

        if remaining == 0 {
            if delete_on_last {
                self.node.backing.mark_delete_on_close();
            }
            let detach = {
                let mut db_info = self.node.db_info.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned WinFileIdentity")
                })?;
                if let Some(ref cur) = db_info.shm_node
                    && Arc::ptr_eq(cur, &self.node)
                {
                    let removed = db_info.shm_node.take();
                    if db_info.n_open_handles == 0 {
                        db_info.lock_geometry = None;
                    }
                    removed
                } else {
                    None
                }
            };
            drop(detach);
            if Arc::strong_count(&self.node.backing) == 1
                && let Err(e) = self.node.backing.close_explicit(delete_on_last)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }

        // Release `shm_init_guard` BEFORE calling `maybe_prune_win_file_info`!
        drop(shm_init_guard);

        if remaining == 0 {
            maybe_prune_win_file_info(self.node.db_file_id, &self.node.db_info);
        }

        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(())
    }
}

// ============================================================================
// Standard OS Filesystem VFS (`OsFile` & `OsVfs`)
// ============================================================================

#[cfg(test)]
thread_local! {
    static TEST_OS_DIR_SYNC_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct OsFile {
    file: Option<File>,
    path: Option<PathBuf>,
    delete_on_close: bool,
    readonly: bool,
    pending_dir_sync: AtomicBool,
    lock_geometry: LockGeometry,
    min_shm_granularity: usize,
    lock_level: Mutex<LockLevel>,
    #[cfg(unix)]
    inode_key: (u64, u64),
    #[cfg(unix)]
    inode_info: Arc<Mutex<UnixInodeInfo>>,
    #[cfg(windows)]
    file_id: (u64, u64),
    #[cfg(windows)]
    file_info: Arc<Mutex<WinFileIdentityInfo>>,
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
        let _ = self.close();
    }
}

impl VfsFile for OsFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let file = self.file_ref()?;
        #[cfg(unix)]
        {
            sys::unix::pread_until_eof(file, buf, offset)
        }
        #[cfg(windows)]
        {
            sys::windows::pread_until_eof(file, buf, offset)
        }
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        if self.readonly {
            return Err(SqliteError::readonly("attempt to write a readonly file"));
        }
        let file = self.file_ref()?;
        #[cfg(unix)]
        {
            sys::unix::pwrite_all(file, buf, offset, extended::SQLITE_IOERR_WRITE)
        }
        #[cfg(windows)]
        {
            sys::windows::pwrite_all(file, buf, offset, extended::SQLITE_IOERR_WRITE)
        }
    }

    fn truncate(&self, size: u64) -> Result<()> {
        if self.readonly {
            return Err(SqliteError::readonly("attempt to truncate a readonly file"));
        }
        let file = self.file_ref()?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            sys::unix::ftruncate_fd(file.as_raw_fd(), size, extended::SQLITE_IOERR_TRUNCATE)
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            sys::windows::truncate_handle(
                file.as_raw_handle(),
                size,
                extended::SQLITE_IOERR_TRUNCATE,
            )
        }
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
        let file = self.file_ref()?;
        #[cfg(unix)]
        {
            sys::unix::sync_fd(file, flags == SyncFlags::Full, flags == SyncFlags::DataOnly)?;
        }
        #[cfg(windows)]
        {
            let res = match flags {
                SyncFlags::DataOnly => file.sync_data(),
                SyncFlags::Normal | SyncFlags::Full => file.sync_all(),
            };
            res.map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_FSYNC, e.to_string())
            })?;
        }
        if self.pending_dir_sync.swap(false, AtomicOrdering::SeqCst)
            && let Some(ref p) = self.path
            && let Some(parent) = p.parent()
        {
            let dir_target = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            #[cfg(unix)]
            {
                sys::unix::sync_directory(dir_target)?;
            }
            #[cfg(not(unix))]
            {
                if let Ok(dir) = File::open(dir_target) {
                    let _ = dir.sync_all();
                }
            }
            #[cfg(test)]
            TEST_OS_DIR_SYNC_COUNT.with(|c| c.set(c.get() + 1));
        }
        Ok(())
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
            let pending_byte = self.lock_geometry.pending_byte_offset() as i64;
            let reserved_byte = self.lock_geometry.reserved_byte_offset() as i64;
            let shared_first = self.lock_geometry.shared_first_offset() as i64;
            let shared_size = self.lock_geometry.shared_size() as i64;

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

            // In-process conflict check (`unixLock` in `sqlite/src/os_unix.c`)
            if *file_lock != inode.e_file_lock
                && (inode.e_file_lock >= LockLevel::Pending || level > LockLevel::Shared)
            {
                return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
            }

            // In-process shared lock reuse
            if level == LockLevel::Shared
                && (inode.e_file_lock == LockLevel::Shared
                    || inode.e_file_lock == LockLevel::Reserved)
            {
                *file_lock = LockLevel::Shared;
                inode.n_shared += 1;
                inode.n_lock += 1;
                return Ok(());
            }

            // Acquire PENDING_BYTE when entering SHARED or upgrading SHARED/RESERVED -> EXCLUSIVE
            // (`unixLock` in `sqlite/src/os_unix.c:1975-1991` acquires `PENDING_BYTE` and sets
            // `PENDING_LOCK` BEFORE checking `nShared > 1` at `os_unix.c:2030` so new readers are
            // blocked while existing in-process readers drain).
            if level == LockLevel::Shared
                || (level == LockLevel::Exclusive
                    && (*file_lock == LockLevel::Shared || *file_lock == LockLevel::Reserved))
            {
                let l_type = if level == LockLevel::Shared {
                    sys::unix::F_RDLCK
                } else {
                    sys::unix::F_WRLCK
                };
                sys::unix::set_posix_lock(
                    fd,
                    l_type,
                    pending_byte,
                    1,
                    extended::SQLITE_IOERR_LOCK,
                )?;
                if level == LockLevel::Exclusive {
                    *file_lock = LockLevel::Pending;
                    inode.e_file_lock = LockLevel::Pending;
                }
            }

            if level == LockLevel::Shared {
                let shared_res = sys::unix::set_posix_lock(
                    fd,
                    sys::unix::F_RDLCK,
                    shared_first,
                    shared_size,
                    extended::SQLITE_IOERR_LOCK,
                );
                let unlock_pending_res = sys::unix::set_posix_lock(
                    fd,
                    sys::unix::F_UNLCK,
                    pending_byte,
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

            if level == LockLevel::Exclusive && inode.n_shared > 1 {
                return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
            }

            let (start, len) = match level {
                LockLevel::Reserved => (reserved_byte, 1),
                LockLevel::Pending => (pending_byte, 1),
                LockLevel::Exclusive => (shared_first, shared_size),
                LockLevel::NoLock | LockLevel::Shared => unreachable!(),
            };
            sys::unix::set_posix_lock(
                fd,
                sys::unix::F_WRLCK,
                start,
                len,
                extended::SQLITE_IOERR_LOCK,
            )?;

            *file_lock = level;
            inode.e_file_lock = level;
            Ok(())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            let raw = self.file_ref()?.as_raw_handle();
            let pending_byte = self.lock_geometry.pending_byte_offset();
            let reserved_byte = self.lock_geometry.reserved_byte_offset();
            let shared_first = self.lock_geometry.shared_first_offset();
            let shared_size = self.lock_geometry.shared_size();

            {
                let cur = self
                    .lock_level
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
                if *cur >= level {
                    return Ok(());
                }
                if *cur == LockLevel::NoLock && level > LockLevel::Shared {
                    drop(cur);
                    self.lock(LockLevel::Shared)?;
                }
            }

            let mut cur = self
                .lock_level
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
            if *cur >= level {
                return Ok(());
            }

            if level == LockLevel::Shared {
                sys::windows::lock_file_range(
                    raw,
                    pending_byte,
                    1,
                    false,
                    extended::SQLITE_IOERR_LOCK,
                )?;
                let shared_res = sys::windows::lock_file_range(
                    raw,
                    shared_first,
                    shared_size,
                    false,
                    extended::SQLITE_IOERR_LOCK,
                );
                let unlock_pending = sys::windows::unlock_file_range(
                    raw,
                    pending_byte,
                    1,
                    extended::SQLITE_IOERR_UNLOCK,
                );
                shared_res?;
                unlock_pending?;
                *cur = LockLevel::Shared;
                return Ok(());
            }

            if level == LockLevel::Reserved {
                sys::windows::lock_file_range(
                    raw,
                    reserved_byte,
                    1,
                    true,
                    extended::SQLITE_IOERR_LOCK,
                )?;
                *cur = LockLevel::Reserved;
                return Ok(());
            }

            if *cur < LockLevel::Pending {
                sys::windows::lock_file_range(
                    raw,
                    pending_byte,
                    1,
                    true,
                    extended::SQLITE_IOERR_LOCK,
                )?;
                *cur = LockLevel::Pending;
            }
            if level == LockLevel::Pending {
                return Ok(());
            }

            // Win32 cannot upgrade SHARED->EXCLUSIVE in place: release SHARED first (`winLock`).
            sys::windows::unlock_file_range(
                raw,
                shared_first,
                shared_size,
                extended::SQLITE_IOERR_UNLOCK,
            )?;
            match sys::windows::lock_file_range(
                raw,
                shared_first,
                shared_size,
                true,
                extended::SQLITE_IOERR_LOCK,
            ) {
                Ok(()) => {
                    *cur = LockLevel::Exclusive;
                    Ok(())
                }
                Err(err) => {
                    let _ = sys::windows::lock_file_range(
                        raw,
                        shared_first,
                        shared_size,
                        false,
                        extended::SQLITE_IOERR_LOCK,
                    );
                    Err(err)
                }
            }
        }
    }

    fn unlock(&self, level: LockLevel) -> Result<()> {
        let target = level.min(LockLevel::Shared);
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let pending_byte = self.lock_geometry.pending_byte_offset() as i64;
            let shared_first = self.lock_geometry.shared_first_offset() as i64;
            let shared_size = self.lock_geometry.shared_size() as i64;

            let mut file_lock = self
                .lock_level
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
            if *file_lock <= target {
                return Ok(());
            }
            let fd = self.file_ref()?.as_raw_fd();

            let mut deferred_fds_to_close: Vec<File> = Vec::new();
            let unlock_res = {
                let mut inode = self
                    .inode_info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode lock"))?;

                if *file_lock > LockLevel::Shared {
                    if target == LockLevel::Shared {
                        sys::unix::set_posix_lock(
                            fd,
                            sys::unix::F_RDLCK,
                            shared_first,
                            shared_size,
                            extended::SQLITE_IOERR_RDLOCK,
                        )?;
                    }
                    // Release both PENDING_BYTE and RESERVED_BYTE (contiguous 2-byte range)
                    sys::unix::set_posix_lock(
                        fd,
                        sys::unix::F_UNLCK,
                        pending_byte,
                        2,
                        extended::SQLITE_IOERR_UNLOCK,
                    )?;
                    inode.e_file_lock = LockLevel::Shared;
                    *file_lock = LockLevel::Shared;
                }

                let mut res = Ok(());
                if target == LockLevel::NoLock {
                    inode.n_shared = inode.n_shared.saturating_sub(1);
                    if inode.n_shared == 0 {
                        res = sys::unix::set_posix_lock(
                            fd,
                            sys::unix::F_UNLCK,
                            0,
                            0,
                            extended::SQLITE_IOERR_UNLOCK,
                        );
                        inode.e_file_lock = LockLevel::NoLock;
                    }
                    inode.n_lock = inode.n_lock.saturating_sub(1);
                    if inode.n_lock == 0 && !inode.has_active_shm_node {
                        // Take deferred FDs out so they are closed OUTSIDE `UnixInodeInfo` mutex!
                        deferred_fds_to_close = std::mem::take(&mut inode.pending_close_fds);
                    }
                    *file_lock = LockLevel::NoLock;
                }
                res
            };

            drop(file_lock);
            let close_res = close_files_outside_mutex(deferred_fds_to_close);
            maybe_prune_unix_inode(self.inode_key, &self.inode_info);
            unlock_res?;
            close_res
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            let mut cur = self
                .lock_level
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
            if *cur <= target {
                return Ok(());
            }
            let raw = self.file_ref()?.as_raw_handle();
            let pending_byte = self.lock_geometry.pending_byte_offset();
            let reserved_byte = self.lock_geometry.reserved_byte_offset();
            let shared_first = self.lock_geometry.shared_first_offset();
            let shared_size = self.lock_geometry.shared_size();

            if *cur == LockLevel::Exclusive {
                sys::windows::unlock_file_range(
                    raw,
                    shared_first,
                    shared_size,
                    extended::SQLITE_IOERR_UNLOCK,
                )?;
                if target == LockLevel::Shared {
                    sys::windows::lock_file_range(
                        raw,
                        shared_first,
                        shared_size,
                        false,
                        extended::SQLITE_IOERR_RDLOCK,
                    )?;
                }
                *cur = LockLevel::Pending;
            }
            if *cur >= LockLevel::Pending {
                let _ = sys::windows::unlock_file_range(
                    raw,
                    pending_byte,
                    1,
                    extended::SQLITE_IOERR_UNLOCK,
                );
            }
            if *cur >= LockLevel::Reserved {
                let _ = sys::windows::unlock_file_range(
                    raw,
                    reserved_byte,
                    1,
                    extended::SQLITE_IOERR_UNLOCK,
                );
            }
            if target == LockLevel::NoLock && *cur >= LockLevel::Shared {
                sys::windows::unlock_file_range(
                    raw,
                    shared_first,
                    shared_size,
                    extended::SQLITE_IOERR_UNLOCK,
                )?;
            }
            *cur = target;
            Ok(())
        }
    }

    fn check_reserved_lock(&self) -> Result<bool> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let fd = self.file_ref()?.as_raw_fd();
            {
                let inode = self
                    .inode_info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode lock"))?;
                if inode.e_file_lock > LockLevel::Shared {
                    return Ok(true);
                }
            }
            sys::unix::check_reserved_lock(fd, self.lock_geometry.reserved_byte_offset())
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            if self.lock_level() >= LockLevel::Reserved {
                return Ok(true);
            }
            let raw = self.file_ref()?.as_raw_handle();
            let reserved_byte = self.lock_geometry.reserved_byte_offset();
            match sys::windows::lock_file_range(
                raw,
                reserved_byte,
                1,
                true,
                extended::SQLITE_IOERR_CHECKRESERVEDLOCK,
            ) {
                Ok(()) => {
                    let _ = sys::windows::unlock_file_range(
                        raw,
                        reserved_byte,
                        1,
                        extended::SQLITE_IOERR_CHECKRESERVEDLOCK,
                    );
                    Ok(false)
                }
                Err(e) if e.code == ErrorCode::Busy => Ok(true),
                Err(other) => Err(other),
            }
        }
    }

    fn lock_level(&self) -> LockLevel {
        self.lock_level
            .lock()
            .map(|g| *g)
            .unwrap_or(LockLevel::NoLock)
    }

    fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
        let _ = self.file_ref()?;
        let guard = self
            .lock_level
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned file lock"))?;
        Ok(Some(*guard))
    }

    fn close(&mut self) -> Result<()> {
        if self.file.is_none() {
            return Ok(());
        }

        let unlock_res = self.unlock(LockLevel::NoLock);
        let mut first_err = unlock_res.err();

        #[cfg(unix)]
        if let Some(f) = self.file.take() {
            let mut to_close_outside: Vec<File> = Vec::new();
            {
                let mut inode = self
                    .inode_info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode lock"))?;
                inode.n_open_handles = inode.n_open_handles.saturating_sub(1);
                if inode.n_lock > 0 || inode.has_active_shm_node {
                    inode.pending_close_fds.push(f);
                } else {
                    to_close_outside = std::mem::take(&mut inode.pending_close_fds);
                    to_close_outside.push(f);
                    if inode.n_open_handles == 0 && inode.shm_node.is_none() {
                        inode.lock_geometry = None;
                    }
                }
            }
            // Close all non-deferred FDs OUTSIDE `UnixInodeInfo` mutex!
            if let Err(e) = close_files_outside_mutex(to_close_outside)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
            maybe_prune_unix_inode(self.inode_key, &self.inode_info);
        }

        #[cfg(windows)]
        if let Some(f) = self.file.take() {
            if let Ok(mut info) = self.file_info.lock() {
                info.n_open_handles = info.n_open_handles.saturating_sub(1);
                if info.n_open_handles == 0 && info.shm_node.is_none() {
                    info.lock_geometry = None;
                }
            }
            maybe_prune_win_file_info(self.file_id, &self.file_info);
            if let Err(e) = sys::windows::close_file_handle(f)
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }

        if self.delete_on_close
            && let Some(ref p) = self.path
            && let Err(e) = fs::remove_file(p)
            && e.kind() != ErrorKind::NotFound
            && first_err.is_none()
        {
            first_err = Some(SqliteError::with_extended(
                extended::SQLITE_IOERR_DELETE,
                e.to_string(),
            ));
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn open_wal_shm_with_options(&self, options: ShmOpenOptions) -> Result<WalShm> {
        let db_file = self.file_ref()?;
        let db_path = self
            .path
            .as_ref()
            .ok_or_else(|| SqliteError::misuse("cannot open WAL-index on unnamed OS file"))?;

        let mut shm_os = db_path.as_os_str().to_os_string();
        shm_os.push("-shm");
        let shm_path = PathBuf::from(shm_os);
        let conn_id = NEXT_CONNECTION_ID.fetch_add(1, AtomicOrdering::Relaxed);

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::MetadataExt;

            // Verify `.db` FD still corresponds to `self.inode_key` and resides on a local FS.
            let db_meta = db_file.metadata().map_err(|e| {
                SqliteError::with_extended(extended::SQLITE_IOERR_FSTAT, e.to_string())
            })?;
            if (db_meta.dev(), db_meta.ino()) != self.inode_key {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_READONLY_DBMOVED,
                    "database file identity changed since open",
                ));
            }
            sys::unix::verify_local_filesystem_for_shm(db_file.as_raw_fd())?;

            // Serialize `UnixShmNode` attachment per `.db` inode WITHOUT holding `UnixInodeInfo`
            // across blocking OS syscalls (`open`, `ftruncate`, `fcntl`).
            let shm_init_mutex = {
                let inode = self
                    .inode_info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode mutex"))?;
                Arc::clone(&inode.shm_init_lock)
            };
            let _shm_init_guard = shm_init_mutex
                .lock()
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned shm init mutex"))?;

            // Fast path: check if an active `UnixShmNode` is already attached to this `.db` inode.
            let existing_node = {
                let inode = self
                    .inode_info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode mutex"))?;
                inode.shm_node.clone()
            };

            if let Some(node) = existing_node {
                // Prevent split-brain across hardlinks / moved `-shm` sidecars!
                if node.backing.shm_path() != shm_path {
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_SHMOPEN,
                        format!(
                            "conflicting -shm sidecar path for active database inode ({} vs {})",
                            node.backing.shm_path().display(),
                            shm_path.display()
                        ),
                    ));
                }
                // Verify on-disk `-shm` via `lstat` (NEVER `open`+`close`!) so we never drop
                // process-scoped POSIX `fcntl` locks on `-shm`!
                let lmeta = fs::symlink_metadata(&shm_path).map_err(|e| {
                    SqliteError::with_extended(extended::SQLITE_IOERR_SHMOPEN, e.to_string())
                })?;
                if lmeta.file_type().is_symlink()
                    || (lmeta.dev(), lmeta.ino()) != node.backing.inode_key()
                {
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_SHMOPEN,
                        "active -shm file was replaced, unlinked, or turned into a symlink",
                    ));
                }

                if let Err(err) = node.ensure_dms_locked()
                    && (err.extended_code != extended::SQLITE_READONLY_CANTINIT
                        || !options.allow_unreliable_readonly)
                {
                    return Err(err);
                }

                {
                    let mut st = node.state.lock().map_err(|_| {
                        SqliteError::new(ErrorCode::Internal, "poisoned UnixShmNode")
                    })?;
                    st.locks.register_connection(conn_id);
                }
                let session = Arc::new(UnixShmSession { node });
                return Ok(WalShm::new(conn_id, session));
            }

            // Reject pre-existing `-shm` symlink or `-shm` hardlinked to `.db` BEFORE opening!
            if let Ok(pre_meta) = fs::symlink_metadata(&shm_path) {
                if pre_meta.file_type().is_symlink() {
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_SHMOPEN,
                        "refusing to follow symlink for -shm sidecar",
                    ));
                }
                if (pre_meta.dev(), pre_meta.ino()) == self.inode_key {
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_SHMOPEN,
                        "-shm sidecar is hardlinked to the main database file",
                    ));
                }
            }

            // Open `-shm` OUTSIDE the `UnixInodeInfo` mutex (`os_unix.c:5032-5044`).
            let mut shm_readonly = options.readonly_shm;
            let shm_file = if !shm_readonly {
                match OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(&shm_path)
                {
                    Ok(f) => f,
                    Err(e)
                        if e.kind() == ErrorKind::PermissionDenied
                            || e.kind() == ErrorKind::ReadOnlyFilesystem =>
                    {
                        shm_readonly = true;
                        OpenOptions::new()
                            .read(true)
                            .open(&shm_path)
                            .map_err(|err| {
                                SqliteError::new(
                                    ErrorCode::CantOpen,
                                    format!("unable to open {}: {err}", shm_path.display()),
                                )
                            })?
                    }
                    Err(e) => {
                        return Err(SqliteError::new(
                            ErrorCode::CantOpen,
                            format!("unable to open {}: {e}", shm_path.display()),
                        ));
                    }
                }
            } else {
                OpenOptions::new().read(true).open(&shm_path).map_err(|e| {
                    SqliteError::new(
                        ErrorCode::CantOpen,
                        format!("unable to open {}: {e}", shm_path.display()),
                    )
                })?
            };

            let fmeta = match shm_file.metadata() {
                Ok(m) => m,
                Err(e) => {
                    let _ = sys::unix::close_file_fd(shm_file);
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_FSTAT,
                        e.to_string(),
                    ));
                }
            };
            let shm_inode_key = (fmeta.dev(), fmeta.ino());
            let post_lmeta = fs::symlink_metadata(&shm_path);
            let bad_identity = match post_lmeta {
                Ok(lm) => {
                    lm.file_type().is_symlink()
                        || !fmeta.is_file()
                        || (lm.dev(), lm.ino()) != shm_inode_key
                        || shm_inode_key == self.inode_key
                }
                Err(_) => true,
            };
            if bad_identity {
                let _ = safe_close_shm_fd(shm_inode_key, shm_file);
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMOPEN,
                    "invalid -shm file identity or symlink race detected",
                ));
            }
            if let Err(e) = sys::unix::verify_local_filesystem_for_shm(shm_file.as_raw_fd()) {
                let _ = safe_close_shm_fd(shm_inode_key, shm_file);
                return Err(e);
            }

            // Perform Unix DMS (`unixLockSharedMemory`, `os_unix.c:4872-4914`) OUTSIDE `UnixInodeInfo`!
            let fd = shm_file.as_raw_fd();
            let mut is_unlocked = false;
            let dms_res: Result<()> = (|| {
                let lock_type = sys::unix::get_posix_lock_type(
                    fd,
                    sys::unix::UNIX_SHM_DMS,
                    1,
                    extended::SQLITE_IOERR_LOCK,
                )?;
                if lock_type == sys::unix::F_WRLCK {
                    return Err(SqliteError::new(
                        ErrorCode::Busy,
                        "WAL-index DMS held exclusively by another process",
                    ));
                }
                if lock_type == sys::unix::F_UNLCK {
                    if shm_readonly {
                        is_unlocked = true;
                        return Err(SqliteError::with_extended(
                            extended::SQLITE_READONLY_CANTINIT,
                            "cannot initialize read-only WAL-index (-shm)",
                        ));
                    }
                    sys::unix::set_posix_lock(
                        fd,
                        sys::unix::F_WRLCK,
                        sys::unix::UNIX_SHM_DMS,
                        1,
                        extended::SQLITE_IOERR_SHMLOCK,
                    )?;
                    if let Err(e) = sys::unix::ftruncate_fd(fd, 3, extended::SQLITE_IOERR_SHMOPEN) {
                        let _ = sys::unix::set_posix_lock(
                            fd,
                            sys::unix::F_UNLCK,
                            sys::unix::UNIX_SHM_DMS,
                            1,
                            extended::SQLITE_IOERR_SHMLOCK,
                        );
                        return Err(e);
                    }
                }
                sys::unix::set_posix_lock(
                    fd,
                    sys::unix::F_RDLCK,
                    sys::unix::UNIX_SHM_DMS,
                    1,
                    extended::SQLITE_IOERR_SHMLOCK,
                )
            })();

            if let Err(err) = dms_res {
                let keep_unreliable = err.extended_code == extended::SQLITE_READONLY_CANTINIT
                    && options.allow_unreliable_readonly;
                if !keep_unreliable {
                    let _ = safe_close_shm_fd(shm_inode_key, shm_file);
                    return Err(err);
                }
            }

            let shm_inode_info_for_hook = lookup_inode_info(shm_inode_key);
            {
                let mut sg = shm_inode_info_for_hook.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned shm inode mutex")
                })?;
                sg.has_active_shm_node = true;
            }
            let defer_hook: sys::unix::DeferFileCloseHook = Box::new(move |file_to_close| {
                if let Ok(mut guard) = shm_inode_info_for_hook.lock()
                    && (guard.n_lock > 0 || guard.has_active_shm_node)
                {
                    guard.pending_close_fds.push(file_to_close);
                    return None;
                }
                Some(file_to_close)
            });

            let backing = Arc::new(sys::unix::SysShmBacking::new(
                shm_file,
                shm_path,
                shm_inode_key,
                shm_readonly,
                Some(defer_hook),
            ));

            let eff_gran = self
                .min_shm_granularity
                .max(sys::unix::os_page_size())
                .max(SHM_CHUNK_SIZE);
            let regions_per_map = (eff_gran / SHM_CHUNK_SIZE).max(1);

            let mut locks = InProcessShmLocks::default();
            locks.register_connection(conn_id);

            let node = Arc::new(UnixShmNode {
                db_inode_key: self.inode_key,
                db_inode_info: Arc::clone(&self.inode_info),
                shm_init_lock: Arc::clone(&shm_init_mutex),
                backing,
                state: Mutex::new(UnixShmNodeState {
                    locks,
                    is_unlocked,
                    chunks: Vec::new(),
                    regions_per_map,
                }),
            });

            {
                let mut inode = self
                    .inode_info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode mutex"))?;
                inode.shm_node = Some(Arc::clone(&node));
            }

            let session = Arc::new(UnixShmSession { node });
            Ok(WalShm::new(conn_id, session))
        }

        #[cfg(windows)]
        {
            let cur_db_id = sys::windows::file_id_by_handle(db_file)?;
            if cur_db_id != self.file_id {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_READONLY_DBMOVED,
                    "database file identity changed since open",
                ));
            }
            sys::windows::verify_local_path_for_shm(&shm_path)?;

            // Serialize `WinShmNode` attachment & teardown per `.db` file ID without holding
            // `WinFileIdentityInfo` across blocking Win32 file/lock operations.
            let shm_init_mutex = {
                let info = self.file_info.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned WinFileIdentity")
                })?;
                Arc::clone(&info.shm_init_lock)
            };
            let _shm_init_guard = shm_init_mutex.lock().map_err(|_| {
                SqliteError::new(ErrorCode::Internal, "poisoned win shm init mutex")
            })?;

            let existing_node = {
                let info = self.file_info.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned WinFileIdentity")
                })?;
                info.shm_node.clone()
            };

            if let Some(node) = existing_node {
                if node.backing.shm_path() != shm_path {
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_SHMOPEN,
                        "conflicting -shm sidecar path for active Windows database file",
                    ));
                }
                let (conn_file, _) =
                    sys::windows::open_shm_handle(&shm_path, !node.backing.is_readonly())?;
                let conn_fid = sys::windows::file_id_by_handle(&conn_file)?;
                if conn_fid != node.backing.file_id() || conn_fid == self.file_id {
                    let _ = sys::windows::close_file_handle(conn_file);
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_SHMOPEN,
                        "-shm file identity mismatch on secondary Windows handle",
                    ));
                }
                if let Err(err) = node.ensure_dms_locked()
                    && (err.extended_code != extended::SQLITE_READONLY_CANTINIT
                        || !options.allow_unreliable_readonly)
                {
                    let _ = sys::windows::close_file_handle(conn_file);
                    return Err(err);
                }
                {
                    let mut st = node.state.lock().map_err(|_| {
                        SqliteError::new(ErrorCode::Internal, "poisoned WinShmNode")
                    })?;
                    st.n_ref += 1;
                }
                let session = Arc::new(WinShmSession {
                    node,
                    lock_file: Mutex::new(Some(conn_file)),
                    held_masks: Mutex::new((0, 0)),
                });
                return Ok(WalShm::new(conn_id, session));
            }

            let create_writable = !options.readonly_shm;
            let (shared_file, shm_readonly) =
                sys::windows::open_shm_handle(&shm_path, create_writable)?;
            let shm_fid = match sys::windows::file_id_by_handle(&shared_file) {
                Ok(id) => id,
                Err(e) => {
                    let _ = sys::windows::close_file_handle(shared_file);
                    return Err(e);
                }
            };
            if shm_fid == self.file_id {
                let _ = sys::windows::close_file_handle(shared_file);
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHMOPEN,
                    "-shm sidecar has same file ID as main database file",
                ));
            }
            let (conn_file, _) = match sys::windows::open_shm_handle(&shm_path, !shm_readonly) {
                Ok(pair) => pair,
                Err(e) => {
                    let _ = sys::windows::close_file_handle(shared_file);
                    return Err(e);
                }
            };

            let backing = Arc::new(sys::windows::SysShmBacking::new(
                shared_file,
                shm_path,
                shm_fid,
                shm_readonly,
            ));
            let node = Arc::new(WinShmNode {
                db_file_id: self.file_id,
                db_info: Arc::clone(&self.file_info),
                shm_init_lock: Arc::clone(&shm_init_mutex),
                backing,
                state: Mutex::new(WinShmNodeState {
                    n_ref: 1,
                    is_unlocked: true,
                    chunks: Vec::new(),
                }),
            });

            if let Err(err) = node.ensure_dms_locked()
                && (err.extended_code != extended::SQLITE_READONLY_CANTINIT
                    || !options.allow_unreliable_readonly)
            {
                let _ = sys::windows::close_file_handle(conn_file);
                return Err(err);
            }

            {
                let mut info = self.file_info.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned WinFileIdentity")
                })?;
                info.shm_node = Some(Arc::clone(&node));
            }

            let session = Arc::new(WinShmSession {
                node,
                lock_file: Mutex::new(Some(conn_file)),
                held_masks: Mutex::new((0, 0)),
            });
            Ok(WalShm::new(conn_id, session))
        }
    }

    fn is_readonly(&self) -> bool {
        self.readonly
    }

    fn supports_wal_shm(&self) -> bool {
        self.path.is_some()
    }

    fn device_characteristics(&self) -> u32 {
        // Matches `SQLITE_POWERSAFE_OVERWRITE = 1` (`sqliteInt.h:402`, `os_unix.c:4369`, `os_win.c:4523`).
        IOCAP_POWERSAFE_OVERWRITE
    }
}

/// Default OS filesystem VFS (`"unix"` on Unix platforms, `"win32"` on Windows).
#[derive(Debug, Clone, Copy)]
pub struct OsVfs {
    lock_geometry: LockGeometry,
    min_shm_granularity: usize,
}

impl Default for OsVfs {
    fn default() -> Self {
        Self::new()
    }
}

impl OsVfs {
    pub const fn new() -> Self {
        Self::with_lock_geometry(LockGeometry::PRODUCTION)
    }

    pub const fn with_lock_geometry(lock_geometry: LockGeometry) -> Self {
        Self {
            lock_geometry,
            min_shm_granularity: SHM_CHUNK_SIZE,
        }
    }

    /// Overrides the minimum `MapView` mapping granularity in bytes (must be a power of two
    /// `>= 32768`; e.g. `65_536` to test 64 KiB OS page chunk grouping on 4 KiB page hosts).
    pub const fn with_min_shm_granularity(mut self, min_shm_granularity: usize) -> Self {
        assert!(
            min_shm_granularity >= SHM_CHUNK_SIZE && min_shm_granularity.is_power_of_two(),
            "min_shm_granularity must be a power of two >= 32768"
        );
        self.min_shm_granularity = min_shm_granularity;
        self
    }
}

impl Vfs for OsVfs {
    fn name(&self) -> &str {
        if cfg!(unix) {
            "unix"
        } else if cfg!(windows) {
            "win32"
        } else {
            "os"
        }
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.lock_geometry
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        if flags.contains(OpenFlags::AUTOPROXY) {
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                "SQLITE_OPEN_AUTOPROXY locking style is not supported",
            ));
        }

        let target_path = match path {
            Some(p) => {
                #[cfg(unix)]
                {
                    if flags.contains(OpenFlags::NOFOLLOW)
                        && let Ok(lm) = fs::symlink_metadata(p)
                        && lm.file_type().is_symlink()
                    {
                        return Err(SqliteError::with_extended(
                            extended::SQLITE_CANTOPEN_SYMLINK,
                            format!("unable to open database file (symlink): {}", p.display()),
                        ));
                    }
                    let (resolved, traversed_symlink) = sys::unix::unix_full_pathname(p)?;
                    if flags.contains(OpenFlags::NOFOLLOW) && traversed_symlink {
                        return Err(SqliteError::with_extended(
                            extended::SQLITE_CANTOPEN_SYMLINK,
                            format!("unable to open database file (symlink): {}", p.display()),
                        ));
                    }
                    resolved
                }
                #[cfg(windows)]
                {
                    sys::windows::win_full_pathname(p)?
                }
            }
            None => make_temp_file_path(),
        };

        // Pre-open geometry conflict check on existing Unix inodes so we don't even open a
        // descriptor when an active inode uses a different `LockGeometry`!
        #[cfg(unix)]
        if let Ok(existing_meta) = fs::metadata(&target_path) {
            use std::os::unix::fs::MetadataExt;
            let pre_key = (existing_meta.dev(), existing_meta.ino());
            let pre_info = lookup_inode_info(pre_key);
            let conflict = {
                let guard = pre_info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode mutex"))?;
                guard.n_open_handles > 0
                    && guard.lock_geometry.is_some_and(|g| g != self.lock_geometry)
            };
            maybe_prune_unix_inode(pre_key, &pre_info);
            if conflict {
                return Err(SqliteError::misuse(format!(
                    "conflicting LockGeometry on active file {}",
                    target_path.display()
                )));
            }
        }

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
                    if (err.kind() == ErrorKind::PermissionDenied
                        || err.kind() == ErrorKind::ReadOnlyFilesystem)
                        && path.is_some()
                        && !flags.contains(OpenFlags::EXCLUSIVE)
                        && !flags.contains(OpenFlags::DELETEONCLOSE)
                        && !flags.requires_initial_dir_sync() =>
                {
                    // Fallback to read-only for existing non-exclusive database opens (`unixOpen` in `os_unix.c:6692`).
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
        let (file, inode_key, inode_info) = {
            use std::os::unix::fs::MetadataExt;
            let meta = match file.metadata() {
                Ok(m) => m,
                Err(e) => {
                    let _ = sys::unix::close_file_fd(file);
                    return Err(SqliteError::with_extended(
                        extended::SQLITE_IOERR_FSTAT,
                        e.to_string(),
                    ));
                }
            };
            if meta.is_dir() {
                let _ = sys::unix::close_file_fd(file);
                return Err(SqliteError::with_extended(
                    extended::SQLITE_CANTOPEN_ISDIR,
                    format!(
                        "unable to open directory as database: {}",
                        target_path.display()
                    ),
                ));
            }
            let key = (meta.dev(), meta.ino());
            let info = lookup_inode_info(key);

            let mut maybe_rejected_file: Option<File> = None;
            let mut geom_conflict_err: Option<SqliteError> = None;
            {
                let mut guard = info
                    .lock()
                    .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned inode mutex"))?;
                if guard.n_open_handles > 0
                    && let Some(active_geom) = guard.lock_geometry
                    && active_geom != self.lock_geometry
                {
                    geom_conflict_err = Some(SqliteError::misuse(format!(
                        "conflicting LockGeometry on active inode {:?} ({:#x} vs {:#x})",
                        key,
                        active_geom.pending_byte_offset(),
                        self.lock_geometry.pending_byte_offset()
                    )));
                    if guard.n_lock > 0 {
                        // Defer closing the rejected FD so we NEVER drop active POSIX locks!
                        guard.pending_close_fds.push(file);
                    } else {
                        maybe_rejected_file = Some(file);
                    }
                    return finalize_unix_open_rejection(
                        key,
                        &info,
                        maybe_rejected_file,
                        geom_conflict_err.unwrap(),
                    );
                }
                guard.lock_geometry = Some(self.lock_geometry);
                guard.n_open_handles += 1;
            }
            let _ = (maybe_rejected_file, geom_conflict_err);
            (file, key, info)
        };

        #[cfg(windows)]
        let (file_id, file_info) = {
            let fid = match sys::windows::file_id_by_handle(&file) {
                Ok(id) => id,
                Err(e) => {
                    let _ = sys::windows::close_file_handle(file);
                    return Err(e);
                }
            };
            let info = lookup_win_file_info(fid);
            {
                let mut guard = info.lock().map_err(|_| {
                    SqliteError::new(ErrorCode::Internal, "poisoned WinFileIdentity")
                })?;
                if guard.n_open_handles > 0
                    && let Some(active_geom) = guard.lock_geometry
                    && active_geom != self.lock_geometry
                {
                    drop(guard);
                    let _ = sys::windows::close_file_handle(file);
                    return Err(SqliteError::misuse(
                        "conflicting LockGeometry on active Windows database file",
                    ));
                }
                guard.lock_geometry = Some(self.lock_geometry);
                guard.n_open_handles += 1;
            }
            (fid, info)
        };

        let pending_dir_sync = !delete_on_close && !readonly && flags.requires_initial_dir_sync();

        Ok(Box::new(OsFile {
            file: Some(file),
            path: Some(target_path),
            delete_on_close,
            readonly,
            pending_dir_sync: AtomicBool::new(pending_dir_sync),
            lock_geometry: self.lock_geometry,
            min_shm_granularity: self.min_shm_granularity,
            lock_level: Mutex::new(LockLevel::NoLock),
            #[cfg(unix)]
            inode_key,
            #[cfg(unix)]
            inode_info,
            #[cfg(windows)]
            file_id,
            #[cfg(windows)]
            file_info,
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => {
                if sync_dir && let Some(parent) = path.parent() {
                    let dir_target = if parent.as_os_str().is_empty() {
                        Path::new(".")
                    } else {
                        parent
                    };
                    #[cfg(unix)]
                    {
                        sys::unix::sync_directory(dir_target)?;
                    }
                    #[cfg(not(unix))]
                    {
                        if let Ok(dir) = File::open(dir_target) {
                            let _ = dir.sync_all();
                        }
                    }
                }
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_DELETE,
                e.to_string(),
            )),
        }
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        match fs::metadata(path) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_ACCESS,
                e.to_string(),
            )),
        }
    }

    fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
        #[cfg(unix)]
        {
            let (resolved, _) = sys::unix::unix_full_pathname(path)?;
            Ok(resolved)
        }
        #[cfg(windows)]
        {
            sys::windows::win_full_pathname(path)
        }
    }
}

#[cfg(unix)]
fn finalize_unix_open_rejection<T>(
    key: (u64, u64),
    info: &Arc<Mutex<UnixInodeInfo>>,
    file_to_close_now: Option<File>,
    err: SqliteError,
) -> Result<T> {
    if let Some(f) = file_to_close_now {
        let _ = sys::unix::close_file_fd(f);
    }
    maybe_prune_unix_inode(key, info);
    Err(err)
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
    fn test_open_flags_super_journal_and_bitmasks() {
        assert_eq!(OpenFlags::SUPER_JOURNAL.0, 0x0000_4000);
        assert_eq!(OpenFlags::MASTER_JOURNAL, OpenFlags::SUPER_JOURNAL);
        assert_eq!(OpenFlags::NOFOLLOW.0, 0x0100_0000);
        let flags = OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::SUPER_JOURNAL;
        assert!(flags.contains(OpenFlags::SUPER_JOURNAL));
        assert!(!flags.is_readonly());
    }

    #[test]
    fn test_wal_index_exact_byte_offsets_and_constants() {
        assert_eq!(WALINDEX_HDR_COPY1_OFFSET, 0);
        assert_eq!(WALINDEX_HDR_COPY_LEN, 48);
        assert_eq!(WALINDEX_HDR_COPY2_OFFSET, 48);
        assert_eq!(WAL_CKPT_INFO_OFFSET, 96);
        assert_eq!(WAL_CKPT_NBACKFILL_OFFSET, 96);
        assert_eq!(WAL_CKPT_READMARK_OFFSET, 100);
        assert_eq!(WAL_CKPT_LOCK_ARRAY_OFFSET, 120);
        assert_eq!(WAL_LOCK_OFFSET, 120);
        assert_eq!(WAL_LOCK_LEN, 8);
        assert_eq!(WAL_SHM_DMS_OFFSET, 128);
        assert_eq!(WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET, 128);
        assert_eq!(WAL_CKPT_RESERVED_OFFSET, 132);
        assert_eq!(WALINDEX_HDR_TOTAL_SIZE, 136);
        assert_eq!(READMARK_NOT_USED, 0xffff_ffff);

        assert_eq!(wal_lock_byte_offset(WAL_WRITE_LOCK), Some(120));
        assert_eq!(wal_lock_byte_offset(WAL_CKPT_LOCK), Some(121));
        assert_eq!(wal_lock_byte_offset(WAL_RECOVER_LOCK), Some(122));
        for slot in 0..=4u8 {
            let idx = wal_read_lock_slot(slot).unwrap();
            assert_eq!(wal_lock_byte_offset(idx), Some(123 + u64::from(slot)));
        }
        assert_eq!(wal_read_lock_slot(5), None);
        assert_eq!(wal_lock_byte_offset(8), None);

        // Hash block frame boundaries: 1..=4062 -> 0, 4063..=8158 -> 1, 8159..=12254 -> 2, 12255 -> 3
        assert_eq!(wal_frame_page(1), 0);
        assert_eq!(wal_frame_page(4062), 0);
        assert_eq!(wal_frame_page(4063), 1);
        assert_eq!(wal_frame_page(8158), 1);
        assert_eq!(wal_frame_page(8159), 2);
        assert_eq!(wal_frame_page(12_254), 2);
        assert_eq!(wal_frame_page(12_255), 3);
    }

    #[test]
    fn test_wal_index_header_64k_page_size_codec_and_checksum() {
        let hdr = WalIndexHeaderData::new(
            65_536,
            42,
            10,
            7,
            true,
            [0x1122_3344, 0x5566_7788],
            [0xdead_beef, 0xcafe_babe],
        )
        .unwrap();

        let (raw, normalized) = hdr.encode_for_publish().unwrap();
        assert_eq!(normalized.version, WALINDEX_MAX_VERSION);
        assert_eq!(normalized.is_init, 1);
        assert_eq!(normalized.page_size, 65_536);

        // `WalIndexHdr.szPage` at bytes 14..16 MUST encode 65536 as native-endian `1u16`!
        let encoded_sz_page = u16::from_ne_bytes([raw[14], raw[15]]);
        assert_eq!(encoded_sz_page, 1);
        assert_eq!(&raw[32..36], &0xdead_beefu32.to_be_bytes());
        assert_eq!(&raw[36..40], &0xcafe_babeu32.to_be_bytes());

        let decoded = WalIndexHeaderData::decode_verified_pair(&raw, &raw).unwrap();
        assert_eq!(decoded, normalized);
        assert_eq!(decoded.page_size, 65_536);

        // Corrupting 1 bit in copy 2 or checksum must fail closed
        let mut dirty_copy2 = raw;
        dirty_copy2[16] ^= 0x01;
        assert!(WalIndexHeaderData::decode_verified_pair(&raw, &dirty_copy2).is_err());

        let mut bad_cksum = raw;
        bad_cksum[40] ^= 0x80;
        assert!(WalIndexHeaderData::decode_verified_pair(&bad_cksum, &bad_cksum).is_err());

        // Valid checksum with unsupported version -> CantOpen (`wal.c:2828`)
        let mut future_ver = raw;
        future_ver[0..4].copy_from_slice(&3_008_000u32.to_ne_bytes());
        let new_ck = wal_checksum_bytes(true, &future_ver[0..40], None).unwrap();
        future_ver[40..44].copy_from_slice(&new_ck[0].to_ne_bytes());
        future_ver[44..48].copy_from_slice(&new_ck[1].to_ne_bytes());
        assert_eq!(
            WalIndexHeaderData::decode_verified_pair(&future_ver, &future_ver)
                .unwrap_err()
                .code,
            ErrorCode::CantOpen
        );
    }

    #[test]
    fn test_region0_lock_byte_firewall_and_nbackfill_attempted_128() {
        let vfs = MemoryVfs::new();
        let db = vfs
            .open(
                Some(Path::new("firewall.db")),
                OpenFlags::READWRITE | OpenFlags::CREATE,
            )
            .unwrap();
        assert_eq!(db.lock_state_for_diagnostics().unwrap(), None);

        let shm = db.open_wal_shm().unwrap();
        let mut writer = shm.acquire_writer().unwrap();
        let chunk0 = writer.ensure_region(0).unwrap();

        // Accessing bytes 120..128 (`aLock[0..8]`) MUST fail closed!
        for forbidden_off in [120usize, 122, 124, 126] {
            assert!(
                chunk0
                    .load_u16(forbidden_off, AtomicOrdering::Relaxed)
                    .is_err()
            );
            assert!(
                chunk0
                    .store_u16(forbidden_off, 1, AtomicOrdering::Relaxed)
                    .is_err()
            );
        }
        assert!(chunk0.load_u32(120, AtomicOrdering::Relaxed).is_err());
        assert!(chunk0.load_u32(124, AtomicOrdering::Relaxed).is_err());
        assert!(chunk0.store_u32(120, 1, AtomicOrdering::Relaxed).is_err());
        assert!(chunk0.store_u32(124, 1, AtomicOrdering::Relaxed).is_err());
        assert!(chunk0.zero_aligned_u32_range(116, 132).is_err());

        // Meanwhile `nBackfillAttempted` at `128..132` MUST succeed as normal mapped data!
        let hdr = WalIndexHeaderData::new(4096, 12, 5, 1, false, [1, 2], [3, 4]).unwrap();
        writer.publish_header(&hdr).unwrap();
        writer.release().unwrap();

        let mut ckpt = shm.acquire_checkpoint(CheckpointMode::Full).unwrap();
        ckpt.set_backfill_attempted(9).unwrap();
        ckpt.set_backfill_completed(7).unwrap();
        let info = ckpt.read_ckpt_info().unwrap();
        assert_eq!(info.n_backfill, 7);
        assert_eq!(info.n_backfill_attempted, 9);
        ckpt.release().unwrap();
    }

    #[test]
    fn test_64k_granularity_chunk_grouping_and_non_relocation_across_12255_frames() {
        // Test BOTH MemoryVfs and OsVfs with 64 KiB `min_shm_granularity` so two 32 KiB
        // `ShmChunk`s share one `Arc<MapView>` without relocating chunk 0 when growing to chunk 2/3!
        let mem_vfs = MemoryVfs::new().with_min_shm_granularity(65_536);
        let mem_db = mem_vfs
            .open(
                Some(Path::new("grow64k.db")),
                OpenFlags::READWRITE | OpenFlags::CREATE,
            )
            .unwrap();
        verify_chunk_grouping_and_growth(&*mem_db);

        let os_vfs = OsVfs::new().with_min_shm_granularity(65_536);
        let tmp_db = make_temp_file_path();
        let mut os_db = os_vfs
            .open(
                Some(&tmp_db),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
            )
            .unwrap();
        assert_eq!(
            os_db.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::NoLock)
        );
        verify_chunk_grouping_and_growth(&*os_db);
        os_db.close().unwrap();
        let mut shm_p = tmp_db.as_os_str().to_os_string();
        shm_p.push("-shm");
        let _ = os_vfs.delete(&PathBuf::from(shm_p), false);
        os_vfs.delete(&tmp_db, false).unwrap();
    }

    fn verify_chunk_grouping_and_growth(db: &dyn VfsFile) {
        let mut shm = db.open_wal_shm().unwrap();
        let mut writer = shm.acquire_writer().unwrap();

        let chunk0_before = writer.ensure_region(0).unwrap();
        let chunk1_before = shm.inspect_mapped_chunk(1).unwrap().unwrap();
        assert!(chunk0_before.shares_view_with(&chunk1_before));
        assert_eq!(chunk0_before.subrange_offset(), 0);
        assert_eq!(chunk1_before.subrange_offset(), 32_768);
        assert_eq!(chunk0_before.view().len(), 65_536);
        let orig_view0_id = chunk0_before.view().debug_view_id();

        // Populate frames up to 12,255 (spanning chunk 0: 1..=4062, chunk 1: 4063..=8158,
        // chunk 2: 8159..=12254, chunk 3: 12255).
        for frame in 1..=12_255u32 {
            let pgno = (frame % 500) + 1;
            writer.record_frame(frame, pgno, 0).unwrap();
        }
        let hdr =
            WalIndexHeaderData::new(65_536, 12_255, 500, 1, false, [10, 20], [30, 40]).unwrap();
        writer.publish_header(&hdr).unwrap();

        let chunk0_after = shm.inspect_mapped_chunk(0).unwrap().unwrap();
        let chunk1_after = shm.inspect_mapped_chunk(1).unwrap().unwrap();
        let chunk2_after = shm.inspect_mapped_chunk(2).unwrap().unwrap();
        let chunk3_after = shm.inspect_mapped_chunk(3).unwrap().unwrap();

        // Chunk 0 and 1 MUST NOT have relocated when chunks 2 & 3 were mapped!
        assert_eq!(chunk0_after.view().debug_view_id(), orig_view0_id);
        assert!(chunk0_after.shares_view_with(&chunk0_before));
        assert!(chunk0_after.shares_view_with(&chunk1_after));
        assert!(chunk2_after.shares_view_with(&chunk3_after));
        assert!(!chunk0_after.shares_view_with(&chunk2_after));

        // Verify boundary frame lookups (4063, 8159, 12255)
        let pg_4063 = (4063 % 500) + 1;
        let pg_8159 = (8159 % 500) + 1;
        let pg_12255 = (12_255 % 500) + 1;
        assert_eq!(
            writer.lookup_page_in_range(pg_4063, 1, 4063).unwrap(),
            Some(4063)
        );
        assert_eq!(
            writer.lookup_page_in_range(pg_8159, 1, 8159).unwrap(),
            Some(8159)
        );
        assert_eq!(
            writer.lookup_page_in_range(pg_12255, 1, 12_255).unwrap(),
            Some(12_255)
        );

        // Truncating above 8158 removes frames 8159..=12255 while keeping <= 8158 intact!
        writer.truncate_frames_above(8158).unwrap();
        assert_eq!(
            writer.lookup_page_in_range(pg_8159, 1, 12_255).unwrap(),
            Some(8159 - 500)
        );

        drop(chunk0_before);
        drop(chunk1_before);
        drop(chunk0_after);
        drop(chunk1_after);
        drop(chunk2_after);
        drop(chunk3_after);
        writer.release().unwrap();
        shm.close(true).unwrap();
    }

    #[test]
    fn test_memory_vfs_wal_shm_multi_connection_conflicts_and_recovery_marks() {
        let vfs = MemoryVfs::new();
        let path = Path::new("multi_wal.db");
        let db1 = vfs
            .open(Some(path), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();
        let db2 = vfs.open(Some(path), OpenFlags::READWRITE).unwrap();
        let db3 = vfs.open(Some(path), OpenFlags::READWRITE).unwrap();

        let shm1 = db1.open_wal_shm().unwrap();
        let shm2 = db2.open_wal_shm().unwrap();
        let shm3 = db3.open_wal_shm().unwrap();

        // Initialize header with mx_frame = 25
        {
            let mut w1 = shm1.acquire_writer().unwrap();
            assert_eq!(shm2.acquire_writer().unwrap_err().code, ErrorCode::Busy);
            for f in 1..=25u32 {
                w1.record_frame(f, f, 0).unwrap();
            }
            let hdr = WalIndexHeaderData::new(4096, 25, 25, 1, false, [1, 1], [10, 20]).unwrap();
            w1.publish_header(&hdr).unwrap();
            w1.release().unwrap();
        }

        // Reader 2 holds slot 2 with mark 15; Reader 3 holds slot 0 (DB-only)
        let r2 = shm2.acquire_read_slot_with_mark(2, Some(15)).unwrap();
        let r3 = shm3.acquire_read_slot(0).unwrap();

        // Recovery on shm1 (`WRITE -> CKPT + RECOVER` WITHOUT locking `READ(0)`!):
        // r3 holds `READ(0)` and MUST NOT block recovery!
        // r2 holds `READ(2)`, so recovery MUST skip slot 2 (`BUSY`) leaving mark=15 untouched,
        // while setting slot 1 = mx_frame (25) and slots 3,4 = READMARK_NOT_USED!
        let w1 = shm1.acquire_writer().unwrap();
        let mut rec = w1.enter_recovery().unwrap_or_else(|(_, e)| panic!("{e}"));
        let hdr = WalIndexHeaderData::new(4096, 25, 25, 2, false, [1, 1], [10, 20]).unwrap();
        let (_, updated) = rec.finish_recovery_header_and_marks(&hdr).unwrap();
        assert_eq!(updated, [true, true, false, true, true]);

        let ckpt_info = r2.read_ckpt_info().unwrap();
        assert_eq!(ckpt_info.n_backfill, 0);
        assert_eq!(ckpt_info.n_backfill_attempted, 25);
        assert_eq!(ckpt_info.read_marks[0], 0);
        assert_eq!(ckpt_info.read_marks[1], 25);
        assert_eq!(ckpt_info.read_marks[2], 15);
        assert_eq!(ckpt_info.read_marks[3], READMARK_NOT_USED);
        assert_eq!(ckpt_info.read_marks[4], READMARK_NOT_USED);

        rec.release().unwrap();
        r2.release().unwrap();
        r3.release().unwrap();
    }

    #[test]
    fn test_lock_geometry_mismatch_rejected_on_memory_and_os_vfs() {
        let mem_vfs = MemoryVfs::with_lock_geometry(LockGeometry::PRODUCTION);
        let h1 = mem_vfs
            .open(
                Some(Path::new("geom.db")),
                OpenFlags::READWRITE | OpenFlags::CREATE,
            )
            .unwrap();
        let err = mem_vfs
            .open_with_lock_geometry(
                Some(Path::new("geom.db")),
                OpenFlags::READWRITE,
                LockGeometry::TEST_64K,
            )
            .err()
            .expect("conflicting LockGeometry must be rejected");
        assert_eq!(err.code, ErrorCode::Misuse);
        drop(h1);
        // Once all handles close, opening with TEST_64K succeeds!
        let _h2 = mem_vfs
            .open_with_lock_geometry(
                Some(Path::new("geom.db")),
                OpenFlags::READWRITE,
                LockGeometry::TEST_64K,
            )
            .unwrap();

        // Same enforcement on physical OsVfs inode!
        let vfs_prod = OsVfs::with_lock_geometry(LockGeometry::PRODUCTION);
        let vfs_test = OsVfs::with_lock_geometry(LockGeometry::TEST_64K);
        let tmp = make_temp_file_path();
        let mut os_h1 = vfs_prod
            .open(Some(&tmp), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();
        os_h1.lock(LockLevel::Shared).unwrap();
        os_h1.lock(LockLevel::Reserved).unwrap();
        assert_eq!(
            vfs_test
                .open(Some(&tmp), OpenFlags::READWRITE)
                .err()
                .expect("conflicting LockGeometry on physical inode must be rejected")
                .code,
            ErrorCode::Misuse
        );
        // Existing RESERVED lock must remain intact after rejected open!
        assert!(os_h1.check_reserved_lock().unwrap());
        os_h1.close().unwrap();

        let mut os_h2 = vfs_test.open(Some(&tmp), OpenFlags::READWRITE).unwrap();
        os_h2.close().unwrap();
        vfs_prod.delete(&tmp, false).unwrap();
    }

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

        h1.lock(LockLevel::Shared).unwrap();
        h2.lock(LockLevel::Shared).unwrap();
        assert!(!h2.check_reserved_lock().unwrap());

        h1.lock(LockLevel::Reserved).unwrap();
        assert!(h2.check_reserved_lock().unwrap());
        assert_eq!(
            h2.lock(LockLevel::Reserved).unwrap_err().code,
            ErrorCode::Busy
        );

        assert_eq!(
            h1.lock(LockLevel::Exclusive).unwrap_err().code,
            ErrorCode::Busy
        );
        assert_eq!(h1.lock_level(), LockLevel::Pending);

        h2.unlock(LockLevel::NoLock).unwrap();
        h1.lock(LockLevel::Exclusive).unwrap();
        assert_eq!(
            h2.lock(LockLevel::Shared).unwrap_err().code,
            ErrorCode::Busy
        );

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
        assert_eq!(
            h2.lock(LockLevel::Reserved).unwrap_err().code,
            ErrorCode::Busy
        );

        h2.unlock(LockLevel::NoLock).unwrap();
        drop(h2);
        assert!(h1.check_reserved_lock().unwrap());

        h1.unlock(LockLevel::NoLock).unwrap();
        drop(h1);
        vfs.delete(&tmp, false).unwrap();
    }

    #[test]
    fn test_super_journal_open_flags_and_one_shot_dir_sync() {
        assert_eq!(OpenFlags::SUPER_JOURNAL, OpenFlags::MASTER_JOURNAL);
        assert_eq!(OpenFlags::SUPER_JOURNAL.0, 0x0000_4000);
        assert!(
            (OpenFlags::READWRITE
                | OpenFlags::CREATE
                | OpenFlags::EXCLUSIVE
                | OpenFlags::SUPER_JOURNAL)
                .requires_initial_dir_sync()
        );
        assert!(
            (OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_JOURNAL)
                .requires_initial_dir_sync()
        );
        assert!(
            (OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::WAL).requires_initial_dir_sync()
        );
        assert!(!(OpenFlags::READONLY | OpenFlags::SUPER_JOURNAL).requires_initial_dir_sync());
        assert!(
            !(OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB)
                .requires_initial_dir_sync()
        );
        assert!(
            !(OpenFlags::READWRITE
                | OpenFlags::CREATE
                | OpenFlags::DELETEONCLOSE
                | OpenFlags::SUPER_JOURNAL)
                .requires_initial_dir_sync()
        );

        // Verify MemoryVfs exclusive creation + readonly open for SUPER_JOURNAL
        let mem_vfs = MemoryVfs::new();
        let mem_sj = Path::new("main.db-mj1234569AB");
        let mut w_mem = mem_vfs
            .open(
                Some(mem_sj),
                OpenFlags::READWRITE
                    | OpenFlags::CREATE
                    | OpenFlags::EXCLUSIVE
                    | OpenFlags::SUPER_JOURNAL,
            )
            .unwrap();
        assert_eq!(
            mem_vfs
                .open(
                    Some(mem_sj),
                    OpenFlags::READWRITE
                        | OpenFlags::CREATE
                        | OpenFlags::EXCLUSIVE
                        | OpenFlags::SUPER_JOURNAL,
                )
                .err()
                .unwrap()
                .code,
            ErrorCode::CantOpen
        );
        w_mem
            .write_at(b"main.db-journal\0aux.db-journal\0", 0)
            .unwrap();
        w_mem.sync(SyncFlags::Normal).unwrap();
        w_mem.close().unwrap();

        let mut r_mem = mem_vfs
            .open(Some(mem_sj), OpenFlags::READONLY | OpenFlags::SUPER_JOURNAL)
            .unwrap();
        assert!(r_mem.is_readonly());
        let mut rbuf = vec![0u8; r_mem.file_size().unwrap() as usize];
        assert_eq!(r_mem.read_at(&mut rbuf, 0).unwrap(), rbuf.len());
        assert_eq!(&rbuf, b"main.db-journal\0aux.db-journal\0");
        assert_eq!(
            r_mem.write_at(b"x", 0).unwrap_err().code,
            ErrorCode::ReadOnly
        );
        r_mem.close().unwrap();
        mem_vfs.delete(mem_sj, true).unwrap();
        assert!(!mem_vfs.exists(mem_sj).unwrap());

        // Verify OsVfs exclusive creation, readonly open, and one-shot parent-directory sync
        let os_vfs = OsVfs::new();
        for (suffix, kind_flag, expected_dir_sync) in [
            ("-mjABCDEF901", OpenFlags::SUPER_JOURNAL, 1usize),
            ("-journal", OpenFlags::MAIN_JOURNAL, 1usize),
            ("-wal", OpenFlags::WAL, 1usize),
            ("", OpenFlags::MAIN_DB, 0usize),
        ] {
            let base = make_temp_file_path();
            let target = PathBuf::from(format!("{}{suffix}", base.display()));
            let _ = os_vfs.delete(&target, false);
            TEST_OS_DIR_SYNC_COUNT.with(|c| c.set(0));

            let mut wf = os_vfs
                .open(
                    Some(&target),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::EXCLUSIVE | kind_flag,
                )
                .unwrap();
            if kind_flag == OpenFlags::SUPER_JOURNAL {
                assert_eq!(
                    os_vfs
                        .open(
                            Some(&target),
                            OpenFlags::READWRITE
                                | OpenFlags::CREATE
                                | OpenFlags::EXCLUSIVE
                                | OpenFlags::SUPER_JOURNAL,
                        )
                        .err()
                        .unwrap()
                        .code,
                    ErrorCode::CantOpen
                );
            }
            wf.write_at(b"payload\0", 0).unwrap();
            // First sync fires parent-directory sync iff `requires_initial_dir_sync()`.
            wf.sync(SyncFlags::Normal).unwrap();
            assert_eq!(
                TEST_OS_DIR_SYNC_COUNT.with(std::cell::Cell::get),
                expected_dir_sync
            );
            // Second sync on the same handle must NOT re-sync the parent directory.
            wf.write_at(b"more\0", 8).unwrap();
            wf.sync(SyncFlags::Normal).unwrap();
            assert_eq!(
                TEST_OS_DIR_SYNC_COUNT.with(std::cell::Cell::get),
                expected_dir_sync
            );
            wf.close().unwrap();

            if kind_flag == OpenFlags::SUPER_JOURNAL {
                let mut rf = os_vfs
                    .open(
                        Some(&target),
                        OpenFlags::READONLY | OpenFlags::SUPER_JOURNAL,
                    )
                    .unwrap();
                assert!(rf.is_readonly());
                let mut out = [0u8; 8];
                assert_eq!(rf.read_at(&mut out, 0).unwrap(), 8);
                assert_eq!(&out, b"payload\0");
                rf.close().unwrap();
            }

            os_vfs.delete(&target, true).unwrap();
            assert!(!os_vfs.exists(&target).unwrap());
        }
    }
}
