//! Rollback journal (`pager.c`), write-ahead log (`wal.c`), page cache (`pcache1.c`),
//! transactions, journal modes, and indexed anonymous physical sub-savepoints for SQLite 3.54.1.
#![forbid(unsafe_code)]

pub mod header;
pub mod wal;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sqliters_types::{
    Bitvec, DatabaseKind, ErrorCode, LockGeometry, Pgno, Result, SQLITE_DEFAULT_PAGE_SIZE,
    SQLITE_VERSION_NUMBER, SqliteError, TextEncoding, decode_db_header_page_size, extended,
};
use sqliters_vfs::{
    LockLevel, MemoryVfs, OpenFlags, OsVfs, ShmOpenOptions, SyncFlags, Vfs, VfsFile,
};

pub use header::{AutoVacuumMode, DB_HEADER_SIZE, DatabaseHeader};
pub use wal::{
    WAL_FRAME_HDRSIZE, WAL_HDRSIZE, WAL_MAGIC_BE, WAL_MAGIC_LE, WAL_MAX_VERSION,
    WAL_RETRY_PROTOCOL_LIMIT, WAL_SAVEPOINT_NDATA, Wal, WalCheckpointMode, WalCheckpointResult,
    WalExclusiveMode, WalFileHeader, WalFrameInput, wal_decode_frame, wal_encode_frame_header,
    wal_frame_offset,
};

/// Magic 8-byte string at the start of an SQLite rollback journal (`aJournalMagic` in `pager.c`).
pub const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

/// Core header field length in bytes at the start of a journal sector (`zeroJournalHdr` zeroes 28 bytes).
pub const JOURNAL_HEADER_CORE_BYTES: usize = 28;

/// Minimum size of the SQLite rollback journal header (`MIN_SECTOR_SIZE` in `pager.c`).
const MIN_JOURNAL_HEADER_BYTES: usize = 512;

/// Maximum sector size accepted in rollback journal headers (`MAX_SECTOR_SIZE` in `pager.c`).
const MAX_JOURNAL_SECTOR_BYTES: u32 = 65_536;

/// Transaction state of the [`Pager`] (`PAGER_OPEN`, `PAGER_READER`, `PAGER_WRITER_*` in `pager.c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Open,
    Read,
    Write,
}

/// Journal operating mode (`PAGER_JOURNALMODE_*` in `sqlite/src/pager.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JournalMode {
    Delete,
    Truncate,
    Persist,
    Memory,
    Off,
    Wal,
}

impl JournalMode {
    /// Lowercase canonical SQLite journal mode name (`sqlite3JournalModename` in `pager.c`).
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Truncate => "truncate",
            Self::Persist => "persist",
            Self::Memory => "memory",
            Self::Off => "off",
            Self::Wal => "wal",
        }
    }

    /// Parses a `PRAGMA journal_mode = <mode>` value (case-insensitive ASCII).
    pub fn from_pragma_str(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("delete") {
            Some(Self::Delete)
        } else if s.eq_ignore_ascii_case("truncate") {
            Some(Self::Truncate)
        } else if s.eq_ignore_ascii_case("persist") {
            Some(Self::Persist)
        } else if s.eq_ignore_ascii_case("memory") {
            Some(Self::Memory)
        } else if s.eq_ignore_ascii_case("off") {
            Some(Self::Off)
        } else if s.eq_ignore_ascii_case("wal") {
            Some(Self::Wal)
        } else {
            None
        }
    }
}

/// Database locking mode (`PAGER_LOCKINGMODE_*` in `sqlite/src/pager.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockingMode {
    Normal,
    Exclusive,
}

impl LockingMode {
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Exclusive => "exclusive",
        }
    }

    pub fn from_pragma_str(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("normal") {
            Some(Self::Normal)
        } else if s.eq_ignore_ascii_case("exclusive") {
            Some(Self::Exclusive)
        } else {
            None
        }
    }
}

/// Configuration options passed when opening a [`Pager`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagerOpenOptions {
    /// Candidate page size for brand-new/empty databases (`SQLITE_DEFAULT_PAGE_SIZE = 4096` in
    /// production; `1024` in the SQLite `testfixture` profile). Overwritten by the on-disk header
    /// on the first locked read if the database file is already populated.
    pub default_page_size: u32,
    /// Optional explicit byte-range lock geometry. When `None`, `vfs.lock_geometry()` is used;
    /// when `Some(g)` and `g != vfs.lock_geometry()`, opening fails closed with `ErrorCode::Misuse`.
    pub lock_geometry: Option<LockGeometry>,
    /// Optional logical database kind override (`NamedDisk`, `PrivateTemporary`, or `Memory`).
    pub db_kind: Option<DatabaseKind>,
    /// For [`DatabaseKind::PrivateTemporary`], selects in-memory backing (`temp_store = MEMORY`)
    /// while preserving `db_kind() == DatabaseKind::PrivateTemporary`.
    pub temp_in_memory: bool,
    /// Opens a read-only database in `immutable` mode (skips OS locking and shared memory creation).
    pub immutable: bool,
    /// Forces `-shm` to open read-only (`WAL_SHM_RDONLY`, testing read-only WAL-index / `bShmUnreliable`).
    pub readonly_shm: bool,
    /// Build-default synchronous level applied when entering WAL mode if the user has not explicitly
    /// overridden `PRAGMA synchronous` (`SQLITE_DEFAULT_WAL_SYNCHRONOUS = 2` in `sqliteInt.h:1501`).
    pub default_wal_synchronous: u8,
    /// Retains `<db>-wal` and `<db>-shm` on last-connection close (`SQLITE_FCNTL_PERSIST_WAL`).
    pub persist_wal: bool,
}

impl Default for PagerOpenOptions {
    fn default() -> Self {
        Self {
            default_page_size: SQLITE_DEFAULT_PAGE_SIZE,
            lock_geometry: None,
            db_kind: None,
            temp_in_memory: false,
            immutable: false,
            readonly_shm: false,
            default_wal_synchronous: 2,
            persist_wal: false,
        }
    }
}

#[derive(Debug, Clone)]
struct SubsavepointSlot {
    orig_page_count: Pgno,
    orig_header: DatabaseHeader,
    /// Pre-images of pages (`<= orig_page_count`) modified or spilled since this subsavepoint opened.
    saved_pages: HashMap<Pgno, Vec<u8>>,
    /// 4-word WAL savepoint token (`[mx_frame, cksum0, cksum1, n_ckpt]`) when `use_wal` is active.
    wal_savepoint: Option<[u32; WAL_SAVEPOINT_NDATA]>,
}

struct CachedPage {
    data: Vec<u8>,
    dirty: bool,
}

#[derive(Debug, Clone)]
struct PendingByteCopy {
    raw_byte_size: u64,
    post_pending_tail: Option<(u64, Vec<u8>)>,
    new_header: DatabaseHeader,
    new_page_count: Pgno,
}

#[inline]
fn derive_sidecar_path(canonical_db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = canonical_db_path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// Manages page-level reads, writes, caching, rollback journaling, WAL protocol, crash recovery,
/// and sub-savepoints.
pub struct Pager {
    vfs: Arc<dyn Vfs>,
    lock_geometry: LockGeometry,
    db_kind: DatabaseKind,
    is_memdb_backing: bool,

    db_path: Option<PathBuf>,
    journal_path: Option<PathBuf>,
    wal_path: Option<PathBuf>,
    shm_path: Option<PathBuf>,

    /// Backing `.db` handle (`None` for lazy file-backed `PrivateTemporary` prior to first write,
    /// or after [`Pager::close`]).
    file: Option<Box<dyn VfsFile>>,
    journal_file: Option<Box<dyn VfsFile>>,
    wal: Option<Wal>,

    readonly: bool,
    immutable: bool,
    readonly_shm: bool,
    persist_wal: bool,
    suppress_auto_wal: bool,
    closed: bool,
    synchronous: u8,
    synchronous_user_set: bool,
    default_wal_synchronous: u8,
    locking_mode: LockingMode,
    journal_mode: JournalMode,
    journal_size_limit: i64,
    cache_size: i32,

    header: DatabaseHeader,
    header_loaded: bool,
    page_size_fixed: bool,
    page_count: Pgno,
    configured_auto_vacuum: AutoVacuumMode,
    autovacuum_ptrmap_ready: bool,
    next_page_size: Option<u32>,
    pending_byte_copy: Option<PendingByteCopy>,
    staged_raw_copy: bool,

    cache: HashMap<Pgno, CachedPage>,
    state: TransactionState,

    /// Transaction-level undo state for atomic rollback
    tx_orig_page_count: Pgno,
    tx_orig_header: DatabaseHeader,
    tx_journaled: Bitvec,
    tx_preimages: HashMap<Pgno, Vec<u8>>,
    journal_records_written: u32,
    journal_off: u64,
    journal_nonce: u32,
    journal_sector_size: u32,
    journal_started_in_tx: bool,
    cache_modified: bool,
    change_counter_bumped: bool,
    /// True once `commit` has started writing/truncating the main database file (`PAGER_WRITER_DBMOD`).
    db_file_modified: bool,
    /// True once Phase 1 of `commit` (`.db` page flush + truncate + sync) has finished (`PAGER_WRITER_FINISHED`).
    commit_phase_one_done: bool,

    subsavepoints: Vec<SubsavepointSlot>,
}

impl Pager {
    /// Opens an OS-backed database file using [`OsVfs`] and default [`PagerOpenOptions`].
    pub fn open_file(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Self> {
        let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        Self::open_with_options(vfs, Some(path.as_ref()), flags, PagerOpenOptions::default())
    }

    /// Creates an isolated in-memory database using [`MemoryVfs`] and default [`PagerOpenOptions`].
    pub fn open_in_memory() -> Result<Self> {
        let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
        Self::open_with_options(
            vfs,
            None,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
            PagerOpenOptions::default(),
        )
    }

    /// Opens a database on a custom [`Vfs`] implementation with default [`PagerOpenOptions`].
    pub fn open_with_vfs(vfs: Arc<dyn Vfs>, path: Option<&Path>, flags: OpenFlags) -> Result<Self> {
        Self::open_with_options(vfs, path, flags, PagerOpenOptions::default())
    }

    /// Opens an isolated private temporary database (`""` / `TEMP`) on `vfs`.
    pub fn open_private_temporary(
        vfs: Arc<dyn Vfs>,
        mut options: PagerOpenOptions,
    ) -> Result<Self> {
        options.db_kind = Some(DatabaseKind::PrivateTemporary);
        Self::open_with_options(
            vfs,
            None,
            OpenFlags::READWRITE
                | OpenFlags::CREATE
                | OpenFlags::EXCLUSIVE
                | OpenFlags::TEMP_DB
                | OpenFlags::DELETEONCLOSE,
            options,
        )
    }

    /// Opens a database on `vfs` using explicit [`PagerOpenOptions`].
    ///
    /// Does **not** read or validate the 100-byte database header, inspect `-wal`, or replay any
    /// `<db>-journal` during open (`TransactionState::Open`, `LockLevel::NoLock`,
    /// `header_loaded = false`). Authoritative header loading and hot-journal recovery occur under
    /// `LockLevel::Shared` on the first [`Self::begin_read`] / [`Self::begin_write`].
    pub fn open_with_options(
        vfs: Arc<dyn Vfs>,
        path: Option<&Path>,
        flags: OpenFlags,
        options: PagerOpenOptions,
    ) -> Result<Self> {
        if !DatabaseHeader::is_valid_page_size(options.default_page_size) {
            return Err(SqliteError::misuse(format!(
                "invalid default_page_size {}: must be power of two in 512..=65536",
                options.default_page_size
            )));
        }

        let vfs_geom = vfs.lock_geometry();
        let lock_geometry = match options.lock_geometry {
            Some(g) if g != vfs_geom => {
                return Err(SqliteError::misuse(format!(
                    "PagerOpenOptions lock_geometry ({:#x}) conflicts with VFS lock_geometry ({:#x})",
                    g.pending_byte_offset(),
                    vfs_geom.pending_byte_offset()
                )));
            }
            Some(g) => g,
            None => vfs_geom,
        };

        let inferred_kind =
            if path == Some(Path::new(":memory:")) || flags.contains(OpenFlags::MEMORY) {
                DatabaseKind::Memory
            } else if path.is_none() || path.is_some_and(|p| p.as_os_str().is_empty()) {
                DatabaseKind::PrivateTemporary
            } else {
                DatabaseKind::NamedDisk
            };
        let db_kind = options.db_kind.unwrap_or(inferred_kind);

        let is_memdb_backing = match db_kind {
            DatabaseKind::Memory => true,
            DatabaseKind::PrivateTemporary => options.temp_in_memory,
            DatabaseKind::NamedDisk => false,
        };

        let (db_path, journal_path, wal_path, shm_path, file, readonly) = match db_kind {
            DatabaseKind::NamedDisk => {
                let raw_path = path.ok_or_else(|| {
                    SqliteError::misuse("DatabaseKind::NamedDisk requires a non-empty path")
                })?;
                if raw_path.as_os_str().is_empty()
                    || raw_path == Path::new(":memory:")
                    || flags.contains(OpenFlags::MEMORY)
                {
                    return Err(SqliteError::misuse(
                        "DatabaseKind::NamedDisk cannot be used with empty or :memory: path",
                    ));
                }
                let canonical = vfs.full_pathname(raw_path)?;
                let jpath = derive_sidecar_path(&canonical, "-journal");
                let wpath = derive_sidecar_path(&canonical, "-wal");
                let spath = derive_sidecar_path(&canonical, "-shm");

                let open_target = if flags.contains(OpenFlags::NOFOLLOW) {
                    raw_path
                } else {
                    canonical.as_path()
                };
                let opened = vfs.open(Some(open_target), flags)?;
                let ro = opened.is_readonly() || flags.is_readonly();
                (
                    Some(canonical),
                    Some(jpath),
                    Some(wpath),
                    Some(spath),
                    Some(opened),
                    ro,
                )
            }
            DatabaseKind::Memory => {
                let mem_vfs = MemoryVfs::with_lock_geometry(lock_geometry);
                let mem_flags = if flags.is_readonly() {
                    OpenFlags::READONLY | OpenFlags::MEMORY
                } else {
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY
                };
                let opened = mem_vfs.open(None, mem_flags)?;
                let ro = opened.is_readonly() || flags.is_readonly();
                (None, None, None, None, Some(opened), ro)
            }
            DatabaseKind::PrivateTemporary => {
                if is_memdb_backing {
                    let mem_vfs = MemoryVfs::with_lock_geometry(lock_geometry);
                    let mem_flags = if flags.is_readonly() {
                        OpenFlags::READONLY | OpenFlags::MEMORY
                    } else {
                        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY
                    };
                    let opened = mem_vfs.open(None, mem_flags)?;
                    let ro = opened.is_readonly() || flags.is_readonly();
                    (None, None, None, None, Some(opened), ro)
                } else {
                    // File-backed temporary DB (`temp_in_memory == false`):
                    // defer opening the anonymous `TEMP_DB | DELETEONCLOSE | EXCLUSIVE` OS file
                    // until the first write transaction!
                    let ro = flags.is_readonly();
                    (None, None, None, None, None, ro)
                }
            }
        };

        let readonly = readonly || options.immutable;

        let journal_mode = if is_memdb_backing {
            JournalMode::Memory
        } else {
            JournalMode::Delete
        };

        let mut candidate_header = DatabaseHeader::new_with_page_size(options.default_page_size);
        candidate_header.database_size_pages = 0;

        Ok(Self {
            vfs,
            lock_geometry,
            db_kind,
            is_memdb_backing,
            db_path,
            journal_path,
            wal_path,
            shm_path,
            file,
            journal_file: None,
            wal: None,
            readonly,
            immutable: options.immutable,
            readonly_shm: options.readonly_shm,
            persist_wal: options.persist_wal,
            suppress_auto_wal: false,
            closed: false,
            synchronous: 2,
            synchronous_user_set: false,
            default_wal_synchronous: options.default_wal_synchronous.min(3),
            locking_mode: LockingMode::Normal,
            journal_mode,
            journal_size_limit: -1,
            cache_size: -2000,
            header: candidate_header,
            header_loaded: false,
            page_size_fixed: false,
            page_count: 0,
            configured_auto_vacuum: AutoVacuumMode::None,
            autovacuum_ptrmap_ready: false,
            next_page_size: None,
            pending_byte_copy: None,
            staged_raw_copy: false,
            cache: HashMap::new(),
            state: TransactionState::Open,
            tx_orig_page_count: 0,
            tx_orig_header: DatabaseHeader::default(),
            tx_journaled: Bitvec::new(64),
            tx_preimages: HashMap::new(),
            journal_records_written: 0,
            journal_off: 0,
            journal_nonce: 0,
            journal_sector_size: MIN_JOURNAL_HEADER_BYTES as u32,
            journal_started_in_tx: false,
            cache_modified: false,
            change_counter_bumped: false,
            db_file_modified: false,
            commit_phase_one_done: false,
            subsavepoints: Vec::new(),
        })
    }

    #[inline]
    fn ensure_not_closed(&self) -> Result<()> {
        if self.closed {
            return Err(SqliteError::misuse("pager is closed"));
        }
        Ok(())
    }

    #[inline]
    fn db_file(&self) -> Result<&dyn VfsFile> {
        self.file
            .as_deref()
            .ok_or_else(|| SqliteError::misuse("database backing file is not open"))
    }

    #[inline]
    fn lock_db_file(&self, level: LockLevel) -> Result<()> {
        if self.immutable {
            return Ok(());
        }
        if let Some(ref f) = self.file {
            f.lock(level)?;
        }
        Ok(())
    }

    #[inline]
    fn unlock_db_file(&self, level: LockLevel) -> Result<()> {
        if self.immutable {
            return Ok(());
        }
        if let Some(ref f) = self.file {
            f.unlock(level)?;
        }
        Ok(())
    }

    /// Acquires `LockLevel::Exclusive` on `.db`, restoring the prior lock level (and releasing any
    /// intermediate `Pending` lock) on failure (`pagerExclusiveLock` in `sqlite/src/pager.c:7635`).
    fn acquire_exclusive_lock_or_restore(&self) -> Result<()> {
        if self.immutable {
            return Ok(());
        }
        if let Some(ref f) = self.file {
            let orig = f.lock_level();
            if orig == LockLevel::NoLock {
                f.lock(LockLevel::Shared)?;
            }
            let shared_lvl = f.lock_level();
            if let Err(e) = f.lock(LockLevel::Exclusive) {
                let _ = f.unlock(shared_lvl);
                if orig == LockLevel::NoLock {
                    let _ = f.unlock(LockLevel::NoLock);
                }
                return Err(e);
            }
        }
        Ok(())
    }

    #[inline]
    pub const fn use_wal(&self) -> bool {
        self.wal.is_some()
    }

    /// Returns a shared reference to the open [`Wal`] session, if active.
    #[inline]
    pub const fn wal(&self) -> Option<&Wal> {
        self.wal.as_ref()
    }

    #[inline]
    pub fn wal_mut(&mut self) -> Option<&mut Wal> {
        self.wal.as_mut()
    }

    #[inline]
    fn wal_sync_flags(&self) -> Option<SyncFlags> {
        if self.is_memdb_backing || self.db_kind == DatabaseKind::PrivateTemporary {
            None
        } else if self.synchronous >= 3 {
            Some(SyncFlags::Full)
        } else if self.synchronous == 2 {
            Some(SyncFlags::Normal)
        } else {
            None
        }
    }

    #[inline]
    fn ckpt_sync_flags(&self) -> Option<SyncFlags> {
        if self.is_memdb_backing || self.db_kind == DatabaseKind::PrivateTemporary {
            None
        } else if self.synchronous >= 3 {
            Some(SyncFlags::Full)
        } else if self.synchronous >= 1 {
            Some(SyncFlags::Normal)
        } else {
            None
        }
    }

    #[inline]
    fn apply_default_wal_synchronous_if_unset(&mut self) {
        if !self.synchronous_user_set {
            self.synchronous = self.default_wal_synchronous;
        }
    }

    #[inline]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    #[inline]
    pub const fn is_readonly(&self) -> bool {
        self.readonly
    }

    #[inline]
    pub const fn is_immutable(&self) -> bool {
        self.immutable
    }

    #[inline]
    pub const fn db_kind(&self) -> DatabaseKind {
        self.db_kind
    }

    #[inline]
    pub const fn is_memdb_backing(&self) -> bool {
        self.is_memdb_backing
    }

    #[inline]
    pub const fn is_in_memory(&self) -> bool {
        self.is_memdb_backing
    }

    /// Returns `true` when the pager is backed by heap memory or a temporary file (`sqlite3PagerIsMemdb`).
    #[inline]
    pub const fn is_pager_memdb(&self) -> bool {
        self.is_memdb_backing || !matches!(self.db_kind, DatabaseKind::NamedDisk) || self.immutable
    }

    /// Returns `true` when WAL mode is active or configured (`use_wal() || journal_mode == Wal`).
    #[inline]
    pub const fn is_wal_mode(&self) -> bool {
        self.use_wal() || matches!(self.journal_mode, JournalMode::Wal)
    }

    /// Returns `true` once Phase 1 of a write commit has flushed `.db`.
    #[inline]
    pub const fn is_commit_phase_one_done(&self) -> bool {
        self.commit_phase_one_done
    }

    /// Returns `true` when a raw [`Self::copy_pages_from`] image is staged in the active write tx.
    #[inline]
    pub const fn has_staged_raw_copy(&self) -> bool {
        self.staged_raw_copy
    }

    /// Returns the active [`AutoVacuumMode`] (from Page 1 header once initialized, otherwise from
    /// [`Self::set_configured_auto_vacuum`]).
    #[inline]
    pub fn auto_vacuum_mode(&self) -> AutoVacuumMode {
        if self.page_count > 0 || self.page_size_fixed {
            AutoVacuumMode::from_header(&self.header)
        } else {
            self.configured_auto_vacuum
        }
    }

    /// Sets the configured [`AutoVacuumMode`] for an uninitialized database (`btree.c:3232-3250`).
    pub fn set_configured_auto_vacuum(&mut self, mode: AutoVacuumMode) {
        self.configured_auto_vacuum = mode;
        if self.page_count == 0 && !self.page_size_fixed {
            self.header.largest_root_btree_page = if mode == AutoVacuumMode::None { 0 } else { 1 };
            self.header.incremental_vacuum_mode = u32::from(mode == AutoVacuumMode::Incremental);
        }
    }

    /// Enables or disables pointer-map auto-vacuum write readiness on this pager.
    #[inline]
    pub fn set_autovacuum_ptrmap_ready(&mut self, ready: bool) {
        self.autovacuum_ptrmap_ready = ready;
    }

    /// Returns `true` when pointer-map auto-vacuum storage writes are enabled on this pager.
    #[inline]
    pub const fn is_autovacuum_ptrmap_ready(&self) -> bool {
        self.autovacuum_ptrmap_ready
    }

    /// Returns any deferred page size requested via [`Self::set_page_size`].
    #[inline]
    pub const fn next_page_size(&self) -> Option<u32> {
        self.next_page_size
    }

    /// Clears any deferred page size stored by [`Self::set_page_size`].
    #[inline]
    pub fn clear_next_page_size(&mut self) {
        self.next_page_size = None;
    }

    #[inline]
    pub const fn lock_geometry(&self) -> LockGeometry {
        self.lock_geometry
    }

    /// Returns the 1-based lock-byte page number (`PENDING_BYTE_PAGE`) for the active
    /// [`LockGeometry`] and current [`Self::page_size`].
    #[inline]
    pub const fn lock_byte_page(&self) -> Pgno {
        self.lock_geometry.lock_byte_page(self.header.page_size)
    }

    /// Queries diagnostic lock state (`SQLITE_FCNTL_LOCKSTATE` / `PRAGMA lock_status`):
    /// returns `Ok(None)` (`"unknown"`) for `memdb` or an unopened lazy `PrivateTemporary` file,
    /// `Ok(Some(level))` on open OS files, or `Err` if closed/faulted.
    pub fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
        self.ensure_not_closed()?;
        match self.file {
            Some(ref f) => f.lock_state_for_diagnostics(),
            None => Ok(None),
        }
    }

    #[inline]
    pub const fn synchronous(&self) -> u8 {
        self.synchronous
    }

    #[inline]
    pub const fn is_synchronous_user_set(&self) -> bool {
        self.synchronous_user_set
    }

    #[inline]
    pub fn set_synchronous(&mut self, level: u8) {
        self.synchronous = level.min(3);
        self.synchronous_user_set = true;
    }

    #[inline]
    pub const fn locking_mode(&self) -> LockingMode {
        self.locking_mode
    }

    /// Updates the pager locking mode (`sqlite3PagerLockingMode`, `pager.c:7404-7415`), returning
    /// the resulting active [`LockingMode`].
    pub fn set_locking_mode(&mut self, mode: LockingMode) -> LockingMode {
        let is_temp = self.db_kind == DatabaseKind::PrivateTemporary && !self.is_memdb_backing;
        let wal_heap = self.wal.as_ref().is_some_and(Wal::is_heap_memory);
        if !is_temp && !wal_heap {
            self.locking_mode = mode;
        }
        self.locking_mode
    }

    #[inline]
    pub const fn journal_mode(&self) -> JournalMode {
        self.journal_mode
    }

    #[inline]
    pub const fn journal_offset(&self) -> u64 {
        self.journal_off
    }

    #[inline]
    pub const fn journal_size_limit(&self) -> i64 {
        self.journal_size_limit
    }

    /// Gets/sets the size limit for persistent journals and WAL resets (`sqlite3PagerJournalSizeLimit`,
    /// `pager.c:7549`). Setting `limit >= -1` updates the limit (`-1` = unlimited); `limit < -1` is query-only.
    #[inline]
    pub fn set_journal_size_limit(&mut self, limit: i64) -> i64 {
        if limit >= -1 {
            self.journal_size_limit = limit;
            if let Some(ref mut wal) = self.wal {
                wal.set_journal_size_limit(limit);
            }
        }
        self.journal_size_limit
    }

    /// Consumes and returns the post-commit WAL frame count (`sqlite3PagerWalCallback`).
    pub fn take_wal_callback_frame_count(&mut self) -> u32 {
        self.wal.as_mut().map_or(0, Wal::take_callback_frame_count)
    }

    /// Queries `SQLITE_FCNTL_PERSIST_WAL`.
    #[inline]
    pub const fn persist_wal(&self) -> bool {
        self.persist_wal
    }

    /// Updates `SQLITE_FCNTL_PERSIST_WAL` (`true` keeps `-wal`/`-shm` on last-connection close).
    #[inline]
    pub fn set_persist_wal(&mut self, persist: bool) {
        self.persist_wal = persist;
    }

    /// Returns the configured page cache size (`PRAGMA cache_size`, default `-2000` KiB).
    #[inline]
    pub const fn cache_size(&self) -> i32 {
        self.cache_size
    }

    /// Updates the configured page cache size (`PRAGMA cache_size = N`).
    #[inline]
    pub fn set_cache_size(&mut self, size: i32) {
        self.cache_size = size;
    }

    /// Computes the effective dirty-page spill threshold in pages from [`Self::cache_size`] and
    /// [`Self::page_size`] (`pragma.c:884-891`).
    #[inline]
    pub fn effective_max_cache_pages(&self) -> usize {
        if self.cache_size >= 0 {
            (self.cache_size as usize).max(1)
        } else {
            let kib = (-i64::from(self.cache_size)) as u64;
            let bytes = kib.saturating_mul(1024);
            let ps = u64::from(self.header.page_size.max(512));
            (bytes / ps).max(1) as usize
        }
    }

    /// Spills dirty pages to `<db>-wal` when in an active WAL write transaction and the number of
    /// dirty cache pages exceeds [`Self::effective_max_cache_pages`].
    pub fn spill_if_over_cache_size(&mut self) -> Result<usize> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write || !self.use_wal() {
            return Ok(0);
        }
        let lock_byte_pg = self.lock_byte_page();
        let dirty_count = self
            .cache
            .iter()
            .filter(|&(&pgno, p)| p.dirty && pgno <= self.page_count && pgno != lock_byte_pg)
            .count();
        if dirty_count > self.effective_max_cache_pages() {
            self.spill_dirty_pages_to_wal()
        } else {
            Ok(0)
        }
    }

    /// Returns `true` when it is safe to change `journal_mode` (`sqlite3PagerOkToChangeJournalMode`,
    /// `pager.c:7536`): false if in a write transaction with dirty pages or `journal_off > 0`.
    pub fn ok_to_change_journal_mode(&self) -> bool {
        if self.state == TransactionState::Write
            && (self.cache_modified || self.journal_off > 0 || self.cache.values().any(|p| p.dirty))
        {
            return false;
        }
        if self.journal_file.is_some() && self.journal_off > 0 {
            return false;
        }
        true
    }

    fn wal_supported(&self) -> bool {
        if self.db_kind != DatabaseKind::NamedDisk || self.is_memdb_backing || self.immutable {
            return false;
        }
        if self.locking_mode == LockingMode::Exclusive {
            return true;
        }
        self.file.as_ref().is_some_and(|f| f.supports_wal_shm())
    }

    fn open_wal_under_shared_lock(&mut self) -> Result<()> {
        if self.wal.is_some() {
            return Ok(());
        }
        if !self.immutable && !self.wal_supported() {
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                "WAL mode is not supported on this database or VFS",
            ));
        }
        if let Some(mut jfd) = self.journal_file.take() {
            jfd.close()?;
        }
        self.journal_off = 0;

        if self.locking_mode == LockingMode::Exclusive && !self.immutable {
            self.acquire_exclusive_lock_or_restore()?;
        }

        let wpath = self
            .wal_path
            .as_deref()
            .ok_or_else(|| SqliteError::misuse("NamedDisk database missing wal_path"))?;
        let db_file = self
            .file
            .as_deref()
            .ok_or_else(|| SqliteError::misuse("NamedDisk database missing file"))?;
        let no_shm = self.locking_mode == LockingMode::Exclusive || self.immutable;
        let wal = Wal::open(
            Arc::clone(&self.vfs),
            db_file,
            wpath,
            no_shm,
            self.journal_size_limit,
            ShmOpenOptions {
                readonly_shm: self.readonly_shm,
                allow_unreliable_readonly: true,
            },
            self.immutable,
        )?;
        if wal.is_readonly() {
            self.readonly = true;
        }
        self.wal = Some(wal);
        self.journal_mode = JournalMode::Wal;
        self.apply_default_wal_synchronous_if_unset();
        Ok(())
    }

    /// Checkpoints and closes the open WAL connection under `LockLevel::Exclusive` prior to
    /// switching out of WAL mode (`sqlite3PagerCloseWal`, `pager.c:7746-7782`).
    ///
    /// If `LockLevel::Exclusive` cannot be obtained due to active readers/writers, returns
    /// `ErrorCode::Busy` WITHOUT closing `self.wal`!
    fn close_wal_for_mode_switch(&mut self) -> Result<()> {
        if self.wal.is_none() {
            self.lock_db_file(LockLevel::Shared)?;
            let exists_res = match self.wal_path.as_deref() {
                Some(wp) => self.vfs.exists(wp),
                None => Ok(false),
            };
            let wal_exists = match exists_res {
                Ok(v) => v,
                Err(e) => {
                    if self.locking_mode == LockingMode::Normal {
                        let _ = self.unlock_db_file(LockLevel::NoLock);
                    }
                    return Err(e);
                }
            };
            if wal_exists && let Err(e) = self.open_wal_under_shared_lock() {
                if self.locking_mode == LockingMode::Normal {
                    let _ = self.unlock_db_file(LockLevel::NoLock);
                }
                return Err(e);
            }
        }

        if self.wal.is_some() {
            self.acquire_exclusive_lock_or_restore()?;
            let ckpt_sync = self.ckpt_sync_flags();
            let wal = self.wal.take().expect("checked Some");
            let close_res = {
                let db_f = self.db_file()?;
                wal.close(db_f, ckpt_sync, false)
            };
            if let Err(e) = close_res {
                if self.locking_mode == LockingMode::Normal {
                    let _ = self.unlock_db_file(LockLevel::Shared);
                }
                return Err(e);
            }
        } else {
            self.acquire_exclusive_lock_or_restore()?;
        }
        Ok(())
    }

    /// Resolves `mode` against backing-store capabilities (`vdbe.c:8261-8272`, `pager.c:7440`),
    /// returning the current `journal_mode` when `mode` is unsupported on this database (e.g.
    /// `Wal` on `:memory:` / `PrivateTemporary` / `immutable`, or disk modes on `:memory:`).
    pub fn effective_journal_mode_target(&self, mode: JournalMode) -> JournalMode {
        if self.is_memdb_backing {
            match mode {
                JournalMode::Memory | JournalMode::Off => mode,
                JournalMode::Delete
                | JournalMode::Truncate
                | JournalMode::Persist
                | JournalMode::Wal => self.journal_mode,
            }
        } else if mode == JournalMode::Wal && !self.wal_supported() {
            self.journal_mode
        } else {
            mode
        }
    }

    /// Loads the authoritative on-disk header (discovering any existing `(2, 2)` WAL state) when
    /// idle on a `NamedDisk` database, then applies [`Self::set_journal_mode`] (`OP_JournalMode` semantics).
    pub fn transition_journal_mode(&mut self, mode: JournalMode) -> Result<JournalMode> {
        self.ensure_not_closed()?;
        if self.state == TransactionState::Open
            && !self.header_loaded
            && self.db_kind == DatabaseKind::NamedDisk
        {
            let _ = self.ensure_header_loaded()?;
        }
        self.set_journal_mode(mode)
    }

    /// Updates the journal mode if allowed (`OP_JournalMode` in `vdbe.c:8241` + `sqlite3PagerSetJournalMode`
    /// in `pager.c:7437`), performing rollback-journaled Page 1 `(1, 1) <-> (2, 2)` transitions
    /// when switching into or out of [`JournalMode::Wal`].
    pub fn set_journal_mode(&mut self, mode: JournalMode) -> Result<JournalMode> {
        self.ensure_not_closed()?;

        if !self.ok_to_change_journal_mode() {
            return Ok(self.journal_mode);
        }

        let old_mode = self.journal_mode;
        let target_mode = self.effective_journal_mode_target(mode);

        if target_mode == old_mode {
            return Ok(self.journal_mode);
        }

        if (old_mode == JournalMode::Wal || target_mode == JournalMode::Wal)
            && self.state != TransactionState::Open
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                if target_mode == JournalMode::Wal {
                    "cannot change into wal mode from within a transaction"
                } else {
                    "cannot change out of wal mode from within a transaction"
                },
            ));
        }

        // Transition 1: `Rollback -> WAL` (`vdbe.c:8296-8315`, `btree.c:11581-11600`)
        if target_mode == JournalMode::Wal {
            self.begin_read()?;
            if self.journal_mode == JournalMode::Wal {
                // `begin_read` already discovered WAL on disk (`-wal` or `(2, 2)` header)!
                self.end_read_if_unused()?;
                return Ok(JournalMode::Wal);
            }
            if self.readonly {
                let _ = self.end_read_if_unused();
                return Err(SqliteError::readonly(
                    "attempt to write a readonly database",
                ));
            }

            let saved_mode = self.journal_mode;
            if let Some(mut jfd) = self.journal_file.take() {
                let _ = jfd.close();
            }
            self.journal_off = 0;
            // Always protect Page 1 `(1, 1) -> (2, 2)` with an on-disk rollback journal (`Delete`),
            // which also unlinks `<db>-journal` upon `commit()`.
            self.journal_mode = JournalMode::Delete;

            let step_res: Result<()> = (|| {
                self.begin_exclusive()?;
                self.mark_page_writable(1)?;
                self.header.write_version = 2;
                self.header.read_version = 2;
                let hdr_copy = self.header.clone();
                if let Some(p1) = self.cache.get_mut(&1) {
                    hdr_copy.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
                    p1.dirty = true;
                }
                self.commit()
            })();

            if let Err(e) = step_res {
                let _ = self.rollback_retaining_read(false);
                self.journal_mode = saved_mode;
                return Err(e);
            }

            self.journal_mode = JournalMode::Wal;
            self.apply_default_wal_synchronous_if_unset();
            return Ok(JournalMode::Wal);
        }

        // Transition 2: `WAL -> Rollback` (`vdbe.c:8286-8309`, `pager.c:7746-7782`, `btree.c:11581-11600`)
        if old_mode == JournalMode::Wal {
            if self.readonly {
                return Err(SqliteError::readonly(
                    "attempt to write a readonly database",
                ));
            }

            self.close_wal_for_mode_switch()?;

            // `.db` currently holds `LockLevel::Exclusive`, and `self.wal` is `None`.
            // Suppress automatic WAL re-open while rewriting Page 1 `(2, 2) -> (1, 1)` under a
            // rollback journal (`Delete` if target is `Memory`/`Off` so bytes 18..19 are crash-safe!).
            let tx_rb_mode = match target_mode {
                JournalMode::Memory | JournalMode::Off => JournalMode::Delete,
                other => other,
            };
            self.journal_mode = tx_rb_mode;
            self.suppress_auto_wal = true;
            self.header_loaded = false;
            self.cache.clear();

            let step_res: Result<()> = (|| {
                self.begin_read()?;
                if self.page_count > 0
                    && (self.header.write_version != 1 || self.header.read_version != 1)
                {
                    self.begin_exclusive()?;
                    self.mark_page_writable(1)?;
                    self.header.write_version = 1;
                    self.header.read_version = 1;
                    let hdr_copy = self.header.clone();
                    if let Some(p1) = self.cache.get_mut(&1) {
                        hdr_copy.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
                        p1.dirty = true;
                    }
                    self.commit()?;
                } else {
                    self.end_read_if_unused()?;
                    if self.locking_mode == LockingMode::Normal {
                        self.unlock_db_file(LockLevel::NoLock)?;
                    }
                }
                Ok(())
            })();

            self.suppress_auto_wal = false;
            if let Err(e) = step_res {
                let _ = self.rollback_retaining_read(false);
                if self.locking_mode == LockingMode::Normal {
                    let _ = self.unlock_db_file(LockLevel::NoLock);
                }
                return Err(e);
            }

            if matches!(target_mode, JournalMode::Memory | JournalMode::Off) {
                if let Some(mut jfd) = self.journal_file.take() {
                    jfd.close()?;
                }
                self.journal_off = 0;
            }
            self.journal_mode = target_mode;
            return Ok(self.journal_mode);
        }

        // Standard rollback-to-rollback mode switch (`Delete` / `Truncate` / `Persist` / `Memory` / `Off`).
        self.journal_mode = target_mode;

        let leaving_persistent = matches!(old_mode, JournalMode::Truncate | JournalMode::Persist)
            && matches!(
                target_mode,
                JournalMode::Delete | JournalMode::Memory | JournalMode::Off
            );

        if leaving_persistent {
            if let Some(mut jfd) = self.journal_file.take() {
                jfd.close()?;
            }
            self.journal_off = 0;

            if self.db_kind == DatabaseKind::NamedDisk
                && let Some(jpath) = self.journal_path.clone()
            {
                // Match `pager.c:7482-7515`: acquire `RESERVED` lock before deleting `<db>-journal`
                // so we never unlink a journal currently owned by another writer, then restore
                // the prior transaction/lock state (`Open`, `Read`, or `Write`).
                match self.state {
                    TransactionState::Write => {
                        self.vfs.delete(&jpath, false)?;
                    }
                    TransactionState::Read => {
                        if !self.readonly && self.lock_db_file(LockLevel::Reserved).is_ok() {
                            let del_res = self.vfs.delete(&jpath, false);
                            let unlock_res = self.unlock_db_file(LockLevel::Shared);
                            del_res?;
                            unlock_res?;
                        }
                    }
                    TransactionState::Open => {
                        if !self.readonly && self.begin_read().is_ok() {
                            let mut del_res = Ok(());
                            if self.lock_db_file(LockLevel::Reserved).is_ok() {
                                del_res = self.vfs.delete(&jpath, false);
                            }
                            let unlock_res = self.unlock_db_file(LockLevel::NoLock);
                            self.state = TransactionState::Open;
                            del_res?;
                            unlock_res?;
                        }
                    }
                }
            }
        } else if matches!(target_mode, JournalMode::Memory | JournalMode::Off) {
            if let Some(mut jfd) = self.journal_file.take() {
                jfd.close()?;
            }
            self.journal_off = 0;
        }

        Ok(self.journal_mode)
    }

    /// Runs a WAL checkpoint (`sqlite3BtreeCheckpoint` + `sqlite3PagerCheckpoint`, `btree.c:11403`,
    /// `pager.c:7586`).
    ///
    /// Returns `ErrorCode::Locked` if this pager has an active `Read` or `Write` transaction,
    /// or `WalCheckpointResult { busy: false, n_log: -1, n_ckpt: -1 }` if the database is not in WAL mode.
    pub fn wal_checkpoint(
        &mut self,
        mode: WalCheckpointMode,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
        interrupt_flag: Option<&AtomicBool>,
    ) -> Result<WalCheckpointResult> {
        self.wal_checkpoint_with_hook(mode, busy_handler, interrupt_flag, None)
    }

    /// Runs a WAL checkpoint with an optional deterministic hook fired right after `walIndexReadHdr`
    /// and before `WAL_READ_LOCK(0)` acquisition.
    pub fn wal_checkpoint_with_hook(
        &mut self,
        mode: WalCheckpointMode,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
        interrupt_flag: Option<&AtomicBool>,
        after_read_hdr_hook: Option<&mut dyn FnMut()>,
    ) -> Result<WalCheckpointResult> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Open {
            return Err(SqliteError::new(
                ErrorCode::Locked,
                "database table is locked",
            ));
        }

        if self.wal.is_none() && self.db_kind == DatabaseKind::NamedDisk && !self.immutable {
            self.lock_db_file(LockLevel::Shared)?;
            let attach_res: Result<()> = (|| {
                let wpath = self.wal_path.clone().expect("NamedDisk has wal_path");
                if self.vfs.exists(&wpath)? {
                    if self.db_file()?.file_size()? > 0 {
                        self.open_wal_under_shared_lock()?;
                    } else if !self.readonly {
                        self.vfs.delete(&wpath, false)?;
                    }
                }
                if self.wal.is_none() {
                    let flen = self.db_file()?.file_size()?;
                    if flen >= DB_HEADER_SIZE as u64 {
                        let mut hdr_bytes = [0u8; DB_HEADER_SIZE];
                        if self.db_file()?.read_at(&mut hdr_bytes, 0)? == DB_HEADER_SIZE {
                            let disk_hdr = DatabaseHeader::parse(&hdr_bytes)?;
                            if disk_hdr.forces_readonly() {
                                self.readonly = true;
                            }
                            if disk_hdr.is_wal() || self.journal_mode == JournalMode::Wal {
                                self.open_wal_under_shared_lock()?;
                            }
                        }
                    } else if self.journal_mode == JournalMode::Wal {
                        self.open_wal_under_shared_lock()?;
                    }
                }
                Ok(())
            })();
            if let Err(e) = attach_res {
                if self.wal.is_none() && self.locking_mode == LockingMode::Normal {
                    let _ = self.unlock_db_file(LockLevel::NoLock);
                }
                return Err(e);
            }
            if self.wal.is_none() && self.locking_mode == LockingMode::Normal {
                let _ = self.unlock_db_file(LockLevel::NoLock);
            }
        }

        if self.wal.is_none() {
            return Ok(WalCheckpointResult {
                busy: false,
                n_log: -1,
                n_ckpt: -1,
            });
        }

        let ckpt_sync = self.ckpt_sync_flags();
        let db_f = self
            .file
            .as_deref()
            .ok_or_else(|| SqliteError::misuse("NamedDisk database missing file"))?;
        let (res, is_changed) = self.wal.as_mut().unwrap().checkpoint_with_hook(
            db_f,
            mode,
            busy_handler,
            ckpt_sync,
            interrupt_flag,
            after_read_hdr_hook,
        )?;
        if is_changed {
            self.cache.clear();
            self.header_loaded = false;
        }
        Ok(res)
    }

    /// Spills all currently dirty cache pages as uncommitted WAL frames (`is_commit = false`,
    /// `n_truncate = 0`, `pagerStress` in `pager.c:4700-4732`), recording savepoint pre-images
    /// (`subjournalPageIfRequired`) and evicting the spilled clean pages from `self.cache` so
    /// subsequent reads/writes exercise live `-wal` lookup and `iReCksum` frame overwrite.
    pub fn spill_dirty_pages_to_wal(&mut self) -> Result<usize> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "spill_dirty_pages_to_wal requires an active write transaction",
            ));
        }
        if !self.use_wal() {
            return Ok(0);
        }

        let lock_byte_pgno = self.lock_byte_page();
        if self.page_count > 0
            && let Some(p1) = self.cache.get_mut(&1)
            && p1.dirty
        {
            self.header.write_version = 2;
            self.header.read_version = 2;
            self.header.database_size_pages = self.page_count;
            self.header.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
        }

        let mut dirty_pgnos: Vec<Pgno> = self
            .cache
            .iter()
            .filter_map(|(&pgno, p)| {
                if p.dirty && pgno <= self.page_count && pgno != lock_byte_pgno {
                    Some(pgno)
                } else {
                    None
                }
            })
            .collect();
        dirty_pgnos.sort_unstable();
        if dirty_pgnos.is_empty() {
            return Ok(0);
        }

        // Save pre-spill image into any open sub-savepoint slots (`subjournalPageIfRequired`)!
        for &pgno in &dirty_pgnos {
            let cur_img = self.cache.get(&pgno).expect("dirty exists").data.clone();
            for sp in &mut self.subsavepoints {
                if pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno) {
                    sp.saved_pages.insert(pgno, cur_img.clone());
                }
            }
        }

        let ps = self.header.page_size;
        let w_sync = self.wal_sync_flags();
        let c_sync = self.ckpt_sync_flags();
        let mut spilled = 0usize;

        for pgno in dirty_pgnos {
            let page_data = self.cache.get(&pgno).expect("dirty exists").data.clone();
            let db_f = self
                .file
                .as_deref()
                .ok_or_else(|| SqliteError::misuse("missing db file during WAL spill"))?;
            self.wal.as_mut().unwrap().write_frames(
                db_f,
                ps,
                &[WalFrameInput {
                    pgno,
                    data: &page_data,
                }],
                0,
                false,
                w_sync,
                c_sync,
            )?;
            self.cache.remove(&pgno);
            spilled += 1;
        }

        self.cache_modified = true;
        Ok(spilled)
    }

    #[inline]
    pub fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
    }

    #[inline]
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    #[inline]
    pub fn journal_path(&self) -> Option<&Path> {
        self.journal_path.as_deref()
    }

    #[inline]
    pub fn wal_path(&self) -> Option<&Path> {
        self.wal_path.as_deref()
    }

    #[inline]
    pub fn shm_path(&self) -> Option<&Path> {
        self.shm_path.as_deref()
    }

    #[inline]
    pub const fn encoding(&self) -> TextEncoding {
        self.header.encoding()
    }

    /// Sets the database text encoding before the database is initialized (`PRAGMA encoding`).
    /// Per SQLite semantics, changing encoding after pages exist is a no-op.
    pub fn set_encoding(&mut self, enc: TextEncoding) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state == TransactionState::Open && !self.header_loaded {
            let _ = self.ensure_header_loaded()?;
        }
        if !self.page_size_fixed && self.page_count == 0 {
            self.header.text_encoding = enc as u32;
        }
        Ok(())
    }

    #[inline]
    pub const fn transaction_state(&self) -> TransactionState {
        self.state
    }

    #[inline]
    pub const fn is_header_loaded(&self) -> bool {
        self.header_loaded
    }

    #[inline]
    pub const fn header(&self) -> &DatabaseHeader {
        &self.header
    }

    /// Ensures the authoritative database header has been loaded under `SHARED` lock if currently
    /// in `TransactionState::Open` with `!header_loaded`, releasing the temporary read lock before
    /// returning.
    pub fn ensure_header_loaded(&mut self) -> Result<&DatabaseHeader> {
        self.ensure_not_closed()?;
        if self.state == TransactionState::Open && !self.header_loaded {
            self.begin_read()?;
            self.end_read_if_unused()?;
        }
        Ok(&self.header)
    }

    /// Returns the authoritative page size, performing a brief locked header read if needed.
    pub fn effective_page_size(&mut self) -> Result<u32> {
        Ok(self.ensure_header_loaded()?.page_size)
    }

    /// Returns the authoritative page count, performing a brief locked header read if needed.
    pub fn effective_page_count(&mut self) -> Result<Pgno> {
        let _ = self.ensure_header_loaded()?;
        Ok(self.page_count)
    }

    /// Returns the authoritative text encoding, performing a brief locked header read if needed.
    pub fn effective_encoding(&mut self) -> Result<TextEncoding> {
        Ok(self.ensure_header_loaded()?.encoding())
    }

    pub fn header_mut(&mut self) -> Result<&mut DatabaseHeader> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "cannot mutate database header outside write transaction",
            ));
        }
        self.ensure_journal_started()?;
        if self.page_count > 0 {
            self.mark_page_writable(1)?;
        }
        self.commit_phase_one_done = false;
        self.cache_modified = true;
        Ok(&mut self.header)
    }

    #[inline]
    pub const fn page_size(&self) -> u32 {
        self.header.page_size
    }

    #[inline]
    pub const fn usable_size(&self) -> u32 {
        self.header.usable_size()
    }

    #[inline]
    pub const fn page_count(&self) -> Pgno {
        self.page_count
    }

    /// Sets the page size before the database is initialized (`PRAGMA page_size = N`), or records
    /// `next_page_size` for a subsequent `VACUUM` when the database already has fixed pages or WAL.
    pub fn set_page_size(&mut self, new_size: u32) -> Result<()> {
        self.ensure_not_closed()?;
        DatabaseHeader::validate_page_size(new_size)?;
        if !self.is_wal_mode()
            && self.state == TransactionState::Open
            && !self.is_memdb_backing
            && (!self.header_loaded || (!self.page_size_fixed && self.page_count == 0))
        {
            self.begin_read()?;
            self.end_read_if_unused()?;
        }
        if !self.page_size_fixed && self.page_count == 0 && !self.is_wal_mode() {
            self.header.page_size = new_size;
            self.cache.clear();
            self.next_page_size = Some(new_size);
        } else {
            self.next_page_size = Some(new_size);
        }
        Ok(())
    }

    /// Starts a read transaction (`SHARED` lock + two-phase WAL discovery + hot-journal recovery
    /// + authoritative 100-byte header validation).
    pub fn begin_read(&mut self) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Open {
            return Ok(());
        }
        if self.is_memdb_backing {
            self.lock_db_file(LockLevel::Shared)?;
            self.header_loaded = true;
            self.state = TransactionState::Read;
            return Ok(());
        }
        if self.db_kind == DatabaseKind::PrivateTemporary && self.file.is_none() {
            self.header_loaded = true;
            self.state = TransactionState::Read;
            return Ok(());
        }

        let had_wal_before = self.wal.is_some();
        let prev_lock = self
            .file
            .as_ref()
            .map_or(LockLevel::NoLock, |f| f.lock_level());

        self.lock_db_file(LockLevel::Shared)?;
        if let Err(e) = self.begin_read_under_shared_lock() {
            if had_wal_before {
                if let Some(ref mut wal) = self.wal {
                    let _ = wal.end_read_transaction();
                }
            } else {
                if let Some(wal) = self.wal.take() {
                    wal.abort_open();
                    self.journal_mode = JournalMode::Delete;
                }
                if self.locking_mode == LockingMode::Normal || prev_lock == LockLevel::NoLock {
                    let _ = self.unlock_db_file(LockLevel::NoLock);
                }
            }
            return Err(e);
        }
        self.state = TransactionState::Read;
        Ok(())
    }

    fn begin_read_under_shared_lock(&mut self) -> Result<()> {
        if self.is_memdb_backing
            || (self.db_kind == DatabaseKind::PrivateTemporary && self.file.is_none())
        {
            self.header_loaded = true;
            return Ok(());
        }

        // Pass 1 (`pagerOpenWalIfPresent`, `pager.c:3393-3421`):
        // Check `<db>-wal` under `.db` `SHARED` lock BEFORE touching Page 1 or `<db>-journal`.
        if self.wal.is_none() && self.db_kind == DatabaseKind::NamedDisk && !self.suppress_auto_wal
        {
            let wpath = self.wal_path.clone().expect("NamedDisk has wal_path");
            if self.vfs.exists(&wpath)? {
                let db_bytes = self.db_file()?.file_size()?;
                if db_bytes == 0 {
                    if !self.immutable && !self.readonly {
                        self.vfs.delete(&wpath, false)?;
                    }
                } else {
                    self.open_wal_under_shared_lock()?;
                }
            }
        }

        if self.wal.is_none() {
            let recovered = if self.immutable {
                false
            } else {
                self.recover_hot_journal_under_shared_lock()?
            };
            let file_len = self.db_file()?.file_size()?;
            if file_len == 0 {
                if !self.suppress_auto_wal && self.journal_mode == JournalMode::Wal {
                    self.open_wal_under_shared_lock()?;
                } else {
                    if recovered || self.page_count != 0 {
                        self.cache.clear();
                        self.page_count = 0;
                        self.header.database_size_pages = 0;
                        self.header.change_counter = 0;
                        self.header.schema_cookie = 0;
                        self.page_size_fixed = false;
                    }
                    self.header_loaded = true;
                    return Ok(());
                }
            } else {
                if file_len < DB_HEADER_SIZE as u64 {
                    return Err(SqliteError::new(
                        ErrorCode::NotADb,
                        "file is not a database",
                    ));
                }

                let mut hdr_bytes = [0u8; DB_HEADER_SIZE];
                let n = self.db_file()?.read_at(&mut hdr_bytes, 0)?;
                if n < DB_HEADER_SIZE {
                    return Err(SqliteError::new(
                        ErrorCode::NotADb,
                        "short read on database header",
                    ));
                }

                let mut disk_hdr = DatabaseHeader::parse(&hdr_bytes)?;
                if disk_hdr.forces_readonly() {
                    self.readonly = true;
                }

                // Pass 2 (`lockBtree`, `btree.c:3371-3386`):
                // If `.db` Page 1 has `read_version == 2` (or `journal_mode == Wal`), attach WAL,
                // discard pre-WAL Page 1, and re-read Page 1 under the WAL snapshot below!
                if !self.suppress_auto_wal
                    && self.db_kind == DatabaseKind::NamedDisk
                    && (disk_hdr.is_wal() || self.journal_mode == JournalMode::Wal)
                {
                    self.open_wal_under_shared_lock()?;
                } else {
                    let ps = u64::from(disk_hdr.page_size);
                    let file_pages = (file_len / ps) as Pgno;

                    // Per btree.c:3330: if header page count is 0 or change_counter != version_valid_for
                    // (e.g. modified by pre-3.7.0 SQLite), fall back to physical file size in pages.
                    let effective_pages = if disk_hdr.database_size_pages == 0
                        || disk_hdr.change_counter != disk_hdr.version_valid_for
                    {
                        file_pages.max(1)
                    } else if disk_hdr.database_size_pages > file_pages && file_pages > 0 {
                        return Err(SqliteError::corrupt(
                            "database header page count exceeds file size",
                        ));
                    } else {
                        disk_hdr.database_size_pages
                    };
                    disk_hdr.database_size_pages = effective_pages;

                    let invalidate_cache = recovered
                        || !self.header_loaded
                        || !self.page_size_fixed
                        || disk_hdr.page_size != self.header.page_size
                        || disk_hdr.change_counter != self.header.change_counter
                        || disk_hdr.schema_cookie != self.header.schema_cookie
                        || effective_pages != self.page_count;
                    if invalidate_cache {
                        self.cache.clear();
                    }

                    self.header = disk_hdr;
                    self.page_count = effective_pages;
                    self.page_size_fixed = true;
                    self.header_loaded = true;
                    return Ok(());
                }
            }
        }

        // Active WAL path (`pagerBeginReadTransaction` + `pagerPagecount` + Page 1 read under WAL snapshot).
        self.wal.as_mut().unwrap().end_read_transaction()?;
        let changed = {
            let db_f = self
                .file
                .as_deref()
                .ok_or_else(|| SqliteError::misuse("NamedDisk database missing file"))?;
            self.wal.as_mut().unwrap().begin_read_transaction(db_f)?
        };
        if self.wal.as_ref().unwrap().is_readonly() {
            self.readonly = true;
        }
        if changed || !self.header_loaded {
            self.cache.clear();
        }

        let wal_pages = self.wal.as_ref().unwrap().db_size();
        let file_len = self.db_file()?.file_size()?;
        if wal_pages == 0 && file_len == 0 {
            if self.page_count != 0 {
                self.cache.clear();
                self.page_count = 0;
                self.header.database_size_pages = 0;
                self.header.change_counter = 0;
                self.header.schema_cookie = 0;
                self.page_size_fixed = false;
            }
            self.header_loaded = true;
            return Ok(());
        }

        let mut hdr_bytes = [0u8; DB_HEADER_SIZE];
        if let Some(p1_frame) = self.wal.as_ref().unwrap().find_frame(1)? {
            self.wal
                .as_ref()
                .unwrap()
                .read_frame(p1_frame, &mut hdr_bytes)?;
        } else {
            if file_len < DB_HEADER_SIZE as u64 {
                return Err(SqliteError::new(
                    ErrorCode::NotADb,
                    "file is not a database",
                ));
            }
            let n = self.db_file()?.read_at(&mut hdr_bytes, 0)?;
            if n < DB_HEADER_SIZE {
                return Err(SqliteError::new(
                    ErrorCode::NotADb,
                    "short read on database header",
                ));
            }
        }

        let mut snap_hdr = DatabaseHeader::parse(&hdr_bytes)?;
        if snap_hdr.forces_readonly() {
            self.readonly = true;
        }

        let effective_pages = if wal_pages > 0 {
            wal_pages
        } else {
            let ps = u64::from(snap_hdr.page_size);
            let file_pages = (file_len.div_ceil(ps)) as Pgno;
            if snap_hdr.database_size_pages == 0
                || snap_hdr.change_counter != snap_hdr.version_valid_for
            {
                file_pages.max(1)
            } else if snap_hdr.database_size_pages > file_pages && file_pages > 0 {
                return Err(SqliteError::corrupt(
                    "database header page count exceeds file size",
                ));
            } else {
                snap_hdr.database_size_pages
            }
        };
        snap_hdr.database_size_pages = effective_pages;

        let invalidate_cache = changed
            || !self.header_loaded
            || !self.page_size_fixed
            || snap_hdr.page_size != self.header.page_size
            || snap_hdr.change_counter != self.header.change_counter
            || snap_hdr.schema_cookie != self.header.schema_cookie
            || effective_pages != self.page_count;
        if invalidate_cache {
            self.cache.clear();
        }

        self.header = snap_hdr;
        self.page_count = effective_pages;
        self.page_size_fixed = true;
        self.header_loaded = true;
        Ok(())
    }

    /// Starts or upgrades to a write transaction (`RESERVED` lock in rollback mode; `WAL_WRITE_LOCK`
    /// in WAL mode).
    pub fn begin_write(&mut self) -> Result<()> {
        self.begin_write_internal(LockLevel::Reserved, 0)
    }

    /// Starts or upgrades to a write transaction, opening `savepoint_depth` anonymous sub-savepoint
    /// slots *before* initializing Page 1 of an empty database so `ROLLBACK TO` can revert to 0 pages.
    pub fn begin_write_with_savepoint_depth(&mut self, savepoint_depth: usize) -> Result<()> {
        self.begin_write_internal(LockLevel::Reserved, savepoint_depth)
    }

    /// Starts or upgrades to an exclusive write transaction (`BEGIN EXCLUSIVE`: acquires `EXCLUSIVE` lock immediately).
    pub fn begin_exclusive(&mut self) -> Result<()> {
        self.begin_write_internal(LockLevel::Exclusive, 0)
    }

    /// Starts or upgrades to an exclusive write transaction with `savepoint_depth` initial sub-savepoints.
    pub fn begin_exclusive_with_savepoint_depth(&mut self, savepoint_depth: usize) -> Result<()> {
        self.begin_write_internal(LockLevel::Exclusive, savepoint_depth)
    }

    fn abort_write_upgrade(&mut self, acquired_read_here: bool) {
        self.subsavepoints.clear();
        if self.use_wal() {
            if let Some(ref mut wal) = self.wal {
                let _ = wal.end_write_transaction();
                if acquired_read_here {
                    let _ = wal.end_read_transaction();
                }
            }
            if acquired_read_here {
                self.state = TransactionState::Open;
            } else {
                self.state = TransactionState::Read;
            }
            return;
        }

        if acquired_read_here {
            if self.locking_mode == LockingMode::Normal {
                let _ = self.unlock_db_file(LockLevel::NoLock);
            }
            self.state = TransactionState::Open;
        } else {
            if self.locking_mode == LockingMode::Normal {
                let _ = self.unlock_db_file(LockLevel::Shared);
            }
            self.state = TransactionState::Read;
        }
    }

    fn begin_write_internal(
        &mut self,
        target_lock: LockLevel,
        savepoint_depth: usize,
    ) -> Result<()> {
        self.ensure_not_closed()?;
        if self.readonly {
            return Err(SqliteError::readonly(
                "attempt to write a readonly database",
            ));
        }

        if self.state == TransactionState::Write {
            if !self.use_wal() && target_lock >= LockLevel::Exclusive && !self.is_memdb_backing {
                self.lock_db_file(LockLevel::Exclusive)?;
            }
            self.ensure_subsavepoints(savepoint_depth)?;
            if self.page_count == 0 {
                self.ensure_journal_started()?;
                self.initialize_empty_database()?;
            }
            return Ok(());
        }

        let acquired_read_here = self.state == TransactionState::Open;

        // Materialize lazy temporary `.db` file on first write transaction
        if self.db_kind == DatabaseKind::PrivateTemporary
            && !self.is_memdb_backing
            && self.file.is_none()
        {
            let opened = self.vfs.open(
                None,
                OpenFlags::READWRITE
                    | OpenFlags::CREATE
                    | OpenFlags::EXCLUSIVE
                    | OpenFlags::TEMP_DB
                    | OpenFlags::DELETEONCLOSE,
            )?;
            self.file = Some(opened);
            // If we were logically in `Read` on an unopened temp DB, acquire `Shared` on the new handle now.
            if !acquired_read_here && let Err(e) = self.lock_db_file(LockLevel::Shared) {
                return Err(e);
            }
        }

        if acquired_read_here {
            self.begin_read()?;
        }

        // Re-check `self.readonly` after `begin_read` (e.g. `write_version > 2` or read-only WAL/SHM)!
        if self.readonly {
            self.abort_write_upgrade(acquired_read_here);
            return Err(SqliteError::readonly(
                "attempt to write a readonly database",
            ));
        }

        // Reject writing to auto-vacuum (`FULL` / `INCREMENTAL`) pointer-map databases unless enabled.
        if !self.autovacuum_ptrmap_ready
            && self.page_count > 0
            && self.header.largest_root_btree_page != 0
        {
            self.abort_write_upgrade(acquired_read_here);
            return Err(SqliteError::new(
                ErrorCode::Error,
                "writing to auto_vacuum (pointer-map) databases is not supported",
            ));
        }

        if self.use_wal() {
            // In `locking_mode = EXCLUSIVE`, acquire `.db` `EXCLUSIVE` lock and transition WAL to
            // `WalExclusiveMode::Exclusive` if currently in `Normal` (`pager.c:5998-6004`).
            if self.locking_mode == LockingMode::Exclusive
                && self.wal.as_mut().unwrap().exclusive_mode_transition(-1)?
            {
                if let Err(e) = self.acquire_exclusive_lock_or_restore() {
                    self.abort_write_upgrade(acquired_read_here);
                    return Err(e);
                }
                if let Err(e) = self.wal.as_mut().unwrap().exclusive_mode_transition(1) {
                    self.abort_write_upgrade(acquired_read_here);
                    return Err(e);
                }
            }

            if let Err(e) = self.wal.as_mut().unwrap().begin_write_transaction() {
                self.abort_write_upgrade(acquired_read_here);
                return Err(e);
            }
        } else {
            let lock_res = if target_lock >= LockLevel::Exclusive
                || self.locking_mode == LockingMode::Exclusive
            {
                self.lock_db_file(LockLevel::Reserved)
                    .and_then(|()| self.lock_db_file(LockLevel::Exclusive))
            } else {
                self.lock_db_file(LockLevel::Reserved)
            };
            if let Err(e) = lock_res {
                self.abort_write_upgrade(acquired_read_here);
                return Err(e);
            }
        }

        self.tx_orig_page_count = self.page_count;
        self.tx_orig_header = self.header.clone();
        self.tx_journaled = Bitvec::new(self.page_count.max(64));
        self.tx_preimages.clear();
        self.journal_records_written = 0;
        self.journal_off = 0;
        self.journal_sector_size = self.effective_sector_size();
        self.journal_started_in_tx = false;
        self.cache_modified = false;
        self.change_counter_bumped = false;
        self.db_file_modified = false;
        self.commit_phase_one_done = false;
        self.pending_byte_copy = None;
        self.staged_raw_copy = false;

        self.state = TransactionState::Write;

        // Open requested anonymous subsavepoint slots BEFORE empty-DB Page 1 initialization
        // so `SAVEPOINT s1; CREATE TABLE ...; ROLLBACK TO s1;` can roll back to 0 pages!
        if let Err(e) = self.ensure_subsavepoints(savepoint_depth) {
            self.abort_write_upgrade(acquired_read_here);
            return Err(e);
        }

        // If the database is brand-new (0 pages), open journal and initialize Page 1 (`sqlite_schema`).
        if self.page_count == 0
            && let Err(e) = self
                .ensure_journal_started()
                .and_then(|()| self.initialize_empty_database())
        {
            if !self.use_wal() {
                let _ = self.finalize_journal_after_tx(false);
            }
            self.cache.remove(&1);
            self.page_count = 0;
            self.header = self.tx_orig_header.clone();
            self.page_size_fixed = false;
            self.journal_started_in_tx = false;
            self.cache_modified = false;
            self.commit_phase_one_done = false;
            self.abort_write_upgrade(acquired_read_here);
            return Err(e);
        }

        Ok(())
    }

    /// Lazily opens/initializes the rollback journal on the first page modification of a write
    /// transaction (`pager_open_journal` in `sqlite/src/pager.c:5893`).
    fn ensure_journal_started(&mut self) -> Result<()> {
        if self.journal_started_in_tx {
            return Ok(());
        }
        if self.use_wal() {
            self.journal_started_in_tx = true;
            self.cache_modified = true;
            return Ok(());
        }

        match self.journal_mode {
            JournalMode::Off | JournalMode::Memory | JournalMode::Wal => {
                self.journal_started_in_tx = true;
                self.cache_modified = true;
                Ok(())
            }
            JournalMode::Delete | JournalMode::Truncate | JournalMode::Persist => {
                let mut nonce_bytes = [0u8; 4];
                self.vfs.randomness(&mut nonce_bytes);
                self.journal_nonce = u32::from_be_bytes(nonce_bytes);
                self.journal_records_written = 0;
                self.journal_sector_size = self.effective_sector_size();

                if self.journal_file.is_none() {
                    let opened = match self.db_kind {
                        DatabaseKind::NamedDisk => {
                            let jpath = self
                                .journal_path
                                .as_deref()
                                .expect("NamedDisk has journal_path");
                            self.vfs.open(
                                Some(jpath),
                                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_JOURNAL,
                            )?
                        }
                        DatabaseKind::PrivateTemporary => self.vfs.open(
                            None,
                            OpenFlags::READWRITE
                                | OpenFlags::CREATE
                                | OpenFlags::EXCLUSIVE
                                | OpenFlags::TEMP_JOURNAL
                                | OpenFlags::DELETEONCLOSE,
                        )?,
                        DatabaseKind::Memory => unreachable!(
                            "Memory DB never uses Delete/Truncate/Persist journal mode"
                        ),
                    };
                    self.journal_file = Some(opened);
                }

                let hdr_sz = self
                    .journal_sector_size
                    .max(MIN_JOURNAL_HEADER_BYTES as u32);
                let initial_nrec = if self.synchronous == 0 {
                    0xffff_ffff
                } else {
                    0
                };
                if let Some(ref jfile) = self.journal_file
                    && let Err(e) = self.write_initial_journal_header(&**jfile, initial_nrec)
                {
                    if self.journal_mode == JournalMode::Delete
                        && let Some(mut jf) = self.journal_file.take()
                    {
                        let _ = jf.close();
                        if let Some(ref jpath) = self.journal_path {
                            let _ = self.vfs.delete(jpath, false);
                        }
                    }
                    self.journal_off = 0;
                    return Err(e);
                }

                self.journal_off = u64::from(hdr_sz);
                self.journal_started_in_tx = true;
                self.cache_modified = true;
                Ok(())
            }
        }
    }

    fn initialize_empty_database(&mut self) -> Result<()> {
        let ps = self.header.page_size as usize;
        let usable = self.header.usable_size();
        let mut page1 = vec![0u8; ps];

        let ver = if self.use_wal() || self.journal_mode == JournalMode::Wal {
            2
        } else {
            1
        };
        let effective_av = if self.configured_auto_vacuum != AutoVacuumMode::None {
            self.configured_auto_vacuum
        } else {
            AutoVacuumMode::from_header(&self.header)
        };
        match effective_av {
            AutoVacuumMode::None => {
                self.header.largest_root_btree_page = 0;
                self.header.incremental_vacuum_mode = 0;
            }
            AutoVacuumMode::Full => {
                self.header.largest_root_btree_page = 1;
                self.header.incremental_vacuum_mode = 0;
            }
            AutoVacuumMode::Incremental => {
                self.header.largest_root_btree_page = 1;
                self.header.incremental_vacuum_mode = 1;
            }
        }
        self.header.write_version = ver;
        self.header.read_version = ver;
        self.header.database_size_pages = 1;
        self.header.change_counter = 1;
        self.header.version_valid_for = 1;
        self.header.schema_format = 4;
        if self.header.text_encoding == 0 {
            self.header.text_encoding = 1;
        }
        self.header.sqlite_version_number = SQLITE_VERSION_NUMBER;
        self.header.write_into(&mut page1[0..DB_HEADER_SIZE]);

        // Page 1 B-tree header at offset 100: empty table leaf B-tree (flags = 0x0d)
        page1[100] = 0x0d; // PTF_INTKEY | PTF_LEAFDATA | PTF_LEAF
        page1[101..103].copy_from_slice(&0u16.to_be_bytes()); // first freeblock = 0
        page1[103..105].copy_from_slice(&0u16.to_be_bytes()); // cell count = 0
        let content_start: u16 = if usable == 65_536 { 0 } else { usable as u16 };
        page1[105..107].copy_from_slice(&content_start.to_be_bytes());
        page1[107] = 0; // fragmented free bytes = 0

        self.page_count = 1;
        self.page_size_fixed = true;
        self.header_loaded = true;
        self.cache_modified = true;
        self.cache.insert(
            1,
            CachedPage {
                data: page1,
                dirty: true,
            },
        );
        Ok(())
    }

    /// Reads page `pgno` (1-based) into the page cache if needed and returns an immutable slice.
    pub fn read_page(&mut self, pgno: Pgno) -> Result<&[u8]> {
        self.ensure_not_closed()?;
        if pgno == 0 || pgno > self.page_count {
            return Err(SqliteError::corrupt(format!(
                "requested invalid page {pgno} (page_count = {})",
                self.page_count
            )));
        }
        if pgno == self.lock_byte_page() {
            return Err(SqliteError::corrupt(format!(
                "cannot read lock-byte page {pgno}"
            )));
        }
        if !self.cache.contains_key(&pgno) {
            let ps = self.header.page_size as usize;
            let mut buf = vec![0u8; ps];
            let wal_frame = match self.wal.as_ref() {
                Some(wal) => wal.find_frame(pgno)?,
                None => None,
            };
            if let Some(fno) = wal_frame {
                self.wal.as_ref().unwrap().read_frame(fno, &mut buf)?;
            } else {
                let offset = u64::from(pgno - 1) * (ps as u64);
                self.db_file()?.read_at(&mut buf, offset)?;
            }
            self.cache.insert(
                pgno,
                CachedPage {
                    data: buf,
                    dirty: false,
                },
            );
        }
        Ok(self
            .cache
            .get(&pgno)
            .expect("just inserted")
            .data
            .as_slice())
    }

    /// Journals the pre-image of `pgno` (if not yet journaled in this tx/subsavepoints) and marks it dirty.
    fn mark_page_writable(&mut self, pgno: Pgno) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "cannot write page outside write transaction",
            ));
        }
        if pgno == 0 || pgno > self.page_count {
            return Err(SqliteError::corrupt(format!(
                "cannot write out-of-bounds page {pgno}"
            )));
        }
        if pgno == self.lock_byte_page() {
            return Err(SqliteError::corrupt(format!(
                "cannot write lock-byte page {pgno}"
            )));
        }

        // Ensure page is loaded into cache BEFORE opening journal so read errors leave state clean
        let _ = self.read_page(pgno)?;
        self.ensure_journal_started()?;

        let need_tx_preimage = self.journal_mode != JournalMode::Off
            && pgno <= self.tx_orig_page_count
            && !self.tx_journaled.test(pgno);
        let need_sp_preimage = self
            .subsavepoints
            .iter()
            .any(|sp| pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno));

        if need_tx_preimage || need_sp_preimage {
            let orig = self.cache.get(&pgno).expect("loaded").data.clone();

            // Always record pre-images for active sub-savepoint slots (even in `JournalMode::Off`
            // so `SAVEPOINT` / `ROLLBACK TO` works in cache per `savepoint-13.4`!).
            for sp in &mut self.subsavepoints {
                if pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno) {
                    sp.saved_pages.insert(pgno, orig.clone());
                }
            }

            if need_tx_preimage {
                if !self.use_wal() && self.journal_file.is_some() {
                    self.append_journal_page(pgno, &orig)?;
                }
                self.tx_journaled.grow_to(self.tx_orig_page_count.max(pgno));
                self.tx_journaled.set(pgno)?;
                self.tx_preimages.insert(pgno, orig);
            }
        }

        self.cache.get_mut(&pgno).expect("loaded").dirty = true;
        self.commit_phase_one_done = false;
        self.cache_modified = true;
        Ok(())
    }

    /// Returns a mutable byte slice for page `pgno` (`1..=page_count`), journaling its pre-image first.
    pub fn write_page(&mut self, pgno: Pgno) -> Result<&mut [u8]> {
        self.mark_page_writable(pgno)?;
        Ok(self
            .cache
            .get_mut(&pgno)
            .expect("loaded")
            .data
            .as_mut_slice())
    }

    /// Appends a brand-new zeroed page at the end of the database file, skipping the runtime
    /// `lock_byte_page` (`PENDING_BYTE_PAGE`), and returning its `Pgno`.
    pub fn allocate_raw_page(&mut self) -> Result<Pgno> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "cannot allocate page outside write transaction",
            ));
        }
        self.ensure_journal_started()?;
        if self.page_count == 0 {
            self.initialize_empty_database()?;
        }

        let next_pgno = self
            .page_count
            .checked_add(1)
            .ok_or_else(|| SqliteError::new(ErrorCode::Full, "database page count overflow"))?;

        // Skip the lock-byte page derived from `self.lock_geometry` (`PENDING_BYTE_PAGE`)!
        // Never mark `lock_byte_pgno` dirty or write it to `.db` / `-journal`.
        let ps = self.header.page_size as usize;
        let lock_byte_pgno = self.lock_byte_page();
        let allocated_pgno = if next_pgno == lock_byte_pgno {
            lock_byte_pgno
                .checked_add(1)
                .ok_or_else(|| SqliteError::new(ErrorCode::Full, "database page count overflow"))?
        } else {
            next_pgno
        };

        self.page_count = allocated_pgno;
        self.header.database_size_pages = allocated_pgno;
        self.tx_journaled.grow_to(allocated_pgno);
        self.commit_phase_one_done = false;
        self.cache_modified = true;
        self.cache.insert(
            allocated_pgno,
            CachedPage {
                data: vec![0u8; ps],
                dirty: true,
            },
        );
        // Page 1 header will need `database_size_pages` updated on commit
        self.mark_page_writable(1)?;
        Ok(allocated_pgno)
    }

    /// Shrinks the database page count to `new_count` within a write transaction (used by `VACUUM`),
    /// journaling all truncated data pages first so `rollback()` / hot-journal recovery can restore them.
    pub fn shrink_page_count(&mut self, mut new_count: Pgno) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "cannot shrink page count outside write transaction",
            ));
        }
        let lock_byte_pgno = self.lock_byte_page();
        if new_count == lock_byte_pgno {
            new_count -= 1;
        }
        if new_count < self.page_count {
            self.ensure_journal_started()?;
            for pgno in (new_count + 1)..=self.page_count {
                if pgno != lock_byte_pgno {
                    self.mark_page_writable(pgno)?;
                }
            }
            self.cache.retain(|&pgno, _| pgno <= new_count);
            self.page_count = new_count;
            self.header.database_size_pages = new_count;
            self.commit_phase_one_done = false;
            self.cache_modified = true;
            if new_count > 0 {
                self.mark_page_writable(1)?;
                let hdr_copy = self.header.clone();
                if let Some(p1) = self.cache.get_mut(&1) {
                    hdr_copy.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
                }
            }
        }
        Ok(())
    }

    /// Truncates the active write-transaction image to `new_count` pages (`sqlite3PagerTruncateImage`).
    #[inline]
    pub fn truncate_image(&mut self, new_count: Pgno) -> Result<()> {
        self.shrink_page_count(new_count)
    }

    /// Copies the complete byte image of `src` into `self` within an active write transaction
    /// (`sqlite3_backup_step` / `backupOnePage`, `sqlite/src/backup.c:237-567`), supporting both
    /// equal-page-size copies and page-size-changing raw byte copies while preserving rollback
    /// journaling in `self`'s pre-transaction page geometry.
    pub fn copy_pages_from(&mut self, src: &mut Pager) -> Result<()> {
        self.ensure_not_closed()?;
        src.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "copy_pages_from requires an active write transaction on destination",
            ));
        }
        if self.readonly {
            return Err(SqliteError::readonly(
                "attempt to write a readonly database",
            ));
        }

        let opened_src_read = if src.state == TransactionState::Open {
            src.begin_read()?;
            true
        } else {
            false
        };

        let copy_res: Result<()> = (|| {
            let pgsz_src = src.page_size();
            let pgsz_dest = self.tx_orig_header.page_size;
            if pgsz_src != pgsz_dest && (self.is_wal_mode() || self.is_pager_memdb()) {
                return Err(SqliteError::readonly(
                    "cannot change page size of WAL or memory database",
                ));
            }

            let raw_src_pages = src.page_count();
            let n_src_pages = raw_src_pages.max(1);
            let src_lock_pg = src.lock_byte_page();

            let mut patched_hdr = src.header().clone();
            if raw_src_pages == 0 {
                match src.auto_vacuum_mode() {
                    AutoVacuumMode::None => {
                        patched_hdr.largest_root_btree_page = 0;
                        patched_hdr.incremental_vacuum_mode = 0;
                    }
                    AutoVacuumMode::Full => {
                        patched_hdr.largest_root_btree_page = 1;
                        patched_hdr.incremental_vacuum_mode = 0;
                    }
                    AutoVacuumMode::Incremental => {
                        patched_hdr.largest_root_btree_page = 1;
                        patched_hdr.incremental_vacuum_mode = 1;
                    }
                }
                patched_hdr.schema_format = 4;
                if patched_hdr.text_encoding == 0 {
                    patched_hdr.text_encoding = 1;
                }
            }
            patched_hdr.page_size = pgsz_src;
            let ver = if self.is_wal_mode() { 2 } else { 1 };
            patched_hdr.write_version = ver;
            patched_hdr.read_version = ver;
            patched_hdr.database_size_pages = n_src_pages;

            let mut next_cc = self.tx_orig_header.change_counter.wrapping_add(1);
            if next_cc == 0 {
                next_cc = 1;
            }
            patched_hdr.change_counter = next_cc;
            patched_hdr.version_valid_for = next_cc;
            patched_hdr.sqlite_version_number = SQLITE_VERSION_NUMBER;
            if patched_hdr.schema_cookie <= self.tx_orig_header.schema_cookie {
                patched_hdr.schema_cookie = self.tx_orig_header.schema_cookie.wrapping_add(1);
            }

            let mut cached_src_page: Option<(Pgno, Vec<u8>)> = None;
            let mut get_src_page = |src_pager: &mut Pager, s_pgno: Pgno| -> Result<Vec<u8>> {
                if let Some((cached_no, ref cached_bytes)) = cached_src_page
                    && cached_no == s_pgno
                {
                    return Ok(cached_bytes.clone());
                }
                let page_bytes = if s_pgno == 1 {
                    let mut p1 = vec![0u8; pgsz_src as usize];
                    if raw_src_pages == 0 {
                        p1[100] = 0x0d;
                        let usable = patched_hdr.usable_size();
                        let content_start: u16 = if usable == 65_536 { 0 } else { usable as u16 };
                        p1[105..107].copy_from_slice(&content_start.to_be_bytes());
                    } else {
                        p1.copy_from_slice(src_pager.read_page(1)?);
                    }
                    patched_hdr.write_into(&mut p1[0..DB_HEADER_SIZE]);
                    p1
                } else if s_pgno == src_lock_pg || s_pgno > raw_src_pages {
                    vec![0u8; pgsz_src as usize]
                } else {
                    src_pager.read_page(s_pgno)?.to_vec()
                };
                cached_src_page = Some((s_pgno, page_bytes.clone()));
                Ok(page_bytes)
            };

            self.ensure_journal_started()?;

            if pgsz_src == pgsz_dest {
                let dest_lock_pg = self.lock_byte_page();
                for pgno in 1..=self.page_count {
                    if pgno != dest_lock_pg {
                        self.mark_page_writable(pgno)?;
                    }
                }
                for s_pgno in 1..=n_src_pages {
                    if s_pgno == dest_lock_pg {
                        continue;
                    }
                    let page_bytes = get_src_page(src, s_pgno)?;
                    self.cache.insert(
                        s_pgno,
                        CachedPage {
                            data: page_bytes,
                            dirty: true,
                        },
                    );
                }
                self.cache
                    .retain(|&pgno, _| pgno <= n_src_pages && pgno != dest_lock_pg);
                self.page_count = n_src_pages;
                self.tx_journaled
                    .grow_to(self.tx_orig_page_count.max(n_src_pages));
                self.header = patched_hdr;
                self.change_counter_bumped = true;
                self.cache_modified = true;
                self.commit_phase_one_done = false;
                self.pending_byte_copy = None;
                self.staged_raw_copy = true;
                self.next_page_size = None;
                return Ok(());
            }

            // `pgsz_src != pgsz_dest`: keep `self.header.page_size == pgsz_dest` and journal all
            // existing destination pages in `pgsz_dest` geometry before staging new byte slices.
            let dest_lock_pg = self.lock_geometry.lock_byte_page(pgsz_dest);
            for pgno in 1..=self.page_count {
                if pgno != dest_lock_pg {
                    self.mark_page_writable(pgno)?;
                }
            }

            let total_bytes = u64::from(n_src_pages) * u64::from(pgsz_src);
            let mut n_dest_pages = total_bytes.div_ceil(u64::from(pgsz_dest)) as Pgno;
            if n_dest_pages == dest_lock_pg {
                n_dest_pages = n_dest_pages.saturating_sub(1);
            }

            let mut new_cache = HashMap::with_capacity(n_dest_pages as usize);
            for d_pgno in 1..=n_dest_pages {
                if d_pgno == dest_lock_pg {
                    continue;
                }
                let d_off = u64::from(d_pgno - 1) * u64::from(pgsz_dest);
                let mut d_buf = vec![0u8; pgsz_dest as usize];
                if pgsz_src > pgsz_dest {
                    let s_pgno = (d_off / u64::from(pgsz_src)) as Pgno + 1;
                    let slice_off = (d_off % u64::from(pgsz_src)) as usize;
                    let s_buf = get_src_page(src, s_pgno)?;
                    d_buf.copy_from_slice(&s_buf[slice_off..slice_off + pgsz_dest as usize]);
                } else {
                    let ratio = pgsz_dest / pgsz_src;
                    for k in 0..ratio {
                        let sub_off = d_off + u64::from(k) * u64::from(pgsz_src);
                        if sub_off >= total_bytes {
                            break;
                        }
                        let s_pgno = (sub_off / u64::from(pgsz_src)) as Pgno + 1;
                        let s_buf = get_src_page(src, s_pgno)?;
                        let dst_start = (k * pgsz_src) as usize;
                        d_buf[dst_start..dst_start + pgsz_src as usize].copy_from_slice(&s_buf);
                    }
                }
                new_cache.insert(
                    d_pgno,
                    CachedPage {
                        data: d_buf,
                        dirty: true,
                    },
                );
            }

            let pb_off = self.lock_geometry.pending_byte_offset();
            let tail_start = pb_off + u64::from(pgsz_src);
            let tail_end = (pb_off + u64::from(pgsz_dest)).min(total_bytes);
            let post_pending_tail = if pgsz_src < pgsz_dest && tail_start < tail_end {
                let mut tail_bytes = Vec::with_capacity((tail_end - tail_start) as usize);
                let mut off = tail_start;
                while off < tail_end {
                    let s_pgno = (off / u64::from(pgsz_src)) as Pgno + 1;
                    let s_buf = get_src_page(src, s_pgno)?;
                    tail_bytes.extend_from_slice(&s_buf);
                    off += u64::from(pgsz_src);
                }
                Some((tail_start, tail_bytes))
            } else {
                None
            };

            self.cache = new_cache;
            self.page_count = n_dest_pages;
            self.tx_journaled
                .grow_to(self.tx_orig_page_count.max(n_dest_pages));
            self.pending_byte_copy = Some(PendingByteCopy {
                raw_byte_size: total_bytes,
                post_pending_tail,
                new_header: patched_hdr,
                new_page_count: n_src_pages,
            });
            self.staged_raw_copy = true;
            self.cache_modified = true;
            self.commit_phase_one_done = false;
            Ok(())
        })();

        if opened_src_read {
            let _ = src.end_read_if_unused();
        }
        copy_res
    }

    /// Commits the active write transaction (or releases read lock if in read transaction),
    /// returning to `TransactionState::Open` (`LockLevel::NoLock`).
    pub fn commit(&mut self) -> Result<()> {
        self.commit_retaining_read(false)
    }

    /// Alias for [`Self::commit_retaining_read`] matching `end_write(retain_read)` lifecycle separation.
    pub fn end_write(&mut self, retain_read: bool) -> Result<()> {
        self.commit_retaining_read(retain_read)
    }

    /// Normalizes `.db` and WAL lock state when ending a write transaction (`Write -> Read` or
    /// `Write -> Open`) or ending a read transaction (`Read -> Read` or `Read -> Open`), matching
    /// `pager_end_transaction` (`pager.c:2209-2214`) and `pager_unlock` (`pager.c:1901-1940`).
    fn finish_tx_to_read_or_open(&mut self, retain_read: bool) -> Result<()> {
        if self.use_wal() {
            if self.locking_mode == LockingMode::Normal
                && self.wal.as_mut().unwrap().exclusive_mode_transition(0)?
            {
                self.unlock_db_file(LockLevel::Shared)?;
            }
            if retain_read {
                self.state = TransactionState::Read;
            } else {
                self.wal.as_mut().unwrap().end_read_transaction()?;
                self.state = TransactionState::Open;
            }
            return Ok(());
        }

        if self.locking_mode == LockingMode::Normal {
            if retain_read {
                self.unlock_db_file(LockLevel::Shared)?;
                self.state = TransactionState::Read;
            } else {
                self.unlock_db_file(LockLevel::NoLock)?;
                self.state = TransactionState::Open;
            }
        } else if retain_read {
            self.state = TransactionState::Read;
        } else {
            self.state = TransactionState::Open;
        }
        Ok(())
    }

    /// Commits/finalizes any active write transaction while retaining `LockLevel::Shared`
    /// (`TransactionState::Read`) for remaining active readers (`Write -> Read`).
    pub fn downgrade_to_read(&mut self) -> Result<()> {
        self.ensure_not_closed()?;
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => self.finish_tx_to_read_or_open(true),
            TransactionState::Write => self.commit_retaining_read(true),
        }
    }

    /// Releases `LockLevel::Shared` (`Read -> Open/NoLock`, or `WAL_READ_LOCK` in WAL mode) when
    /// no active reader remains. Does not abort an active `Write` transaction.
    pub fn end_read_if_unused(&mut self) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state == TransactionState::Read {
            self.finish_tx_to_read_or_open(false)?;
        }
        Ok(())
    }

    /// Commits the active transaction. When `retain_read` is `true`, downgrades to
    /// `TransactionState::Read` (retaining `.db` `Shared` and positive `WAL_READ_LOCK(1..=4)` in WAL);
    /// when `false`, transitions to `TransactionState::Open`.
    pub fn commit_retaining_read(&mut self, retain_read: bool) -> Result<()> {
        self.ensure_not_closed()?;
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => self.finish_tx_to_read_or_open(retain_read),
            TransactionState::Write => {
                if self.use_wal() {
                    let lock_byte_pgno = self.lock_byte_page();
                    let has_dirty_pages = self.cache.iter().any(|(&pgno, p)| {
                        p.dirty && pgno <= self.page_count && pgno != lock_byte_pgno
                    });
                    let needs_db_flush = self.cache_modified
                        || has_dirty_pages
                        || self.page_count != self.tx_orig_page_count;

                    if needs_db_flush && self.page_count > 0 {
                        let p1_already_dirty = self.cache.get(&1).is_some_and(|p| p.dirty);
                        // Match `pager.c:6565-6571` (Ticket `2d1a5c67dfc2`) & `pager.c:3272`:
                        // Only dirty Page 1 on WAL commit when Page 1 was already modified, all
                        // dirty pages were spilled earlier (`!has_dirty_pages`), the database page
                        // count changed (`btree.c:6808`), or Page 1 header versions are not `(2, 2)`.
                        let must_write_p1 = p1_already_dirty
                            || !has_dirty_pages
                            || self.page_count != self.tx_orig_page_count
                            || self.header.write_version != 2
                            || self.header.read_version != 2;
                        if must_write_p1 {
                            self.mark_page_writable(1)?;
                            self.header.write_version = 2;
                            self.header.read_version = 2;
                            self.header.database_size_pages = self.page_count;
                            if !self.change_counter_bumped {
                                self.header.change_counter =
                                    self.header.change_counter.wrapping_add(1);
                                if self.header.change_counter == 0 {
                                    self.header.change_counter = 1;
                                }
                                self.header.version_valid_for = self.header.change_counter;
                                self.header.sqlite_version_number = SQLITE_VERSION_NUMBER;
                                self.change_counter_bumped = true;
                            }

                            let hdr_copy = self.header.clone();
                            let p1 = self.cache.get_mut(&1).expect("page 1 in cache");
                            hdr_copy.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
                            p1.dirty = true;
                        }

                        let mut dirty_pgnos: Vec<Pgno> = self
                            .cache
                            .iter()
                            .filter_map(|(&pgno, p)| {
                                if p.dirty && pgno <= self.page_count && pgno != lock_byte_pgno {
                                    Some(pgno)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        dirty_pgnos.sort_unstable();

                        let ps = self.header.page_size;
                        let w_sync = self.wal_sync_flags();
                        let c_sync = self.ckpt_sync_flags();
                        let effective_n_truncate = if self.page_count == lock_byte_pgno {
                            self.page_count - 1
                        } else {
                            self.page_count
                        };

                        {
                            let db_f = self
                                .file
                                .as_deref()
                                .ok_or_else(|| SqliteError::misuse("NamedDisk missing file"))?;
                            let wal = self.wal.as_mut().expect("use_wal checked");
                            let inputs: Vec<WalFrameInput<'_>> = dirty_pgnos
                                .iter()
                                .map(|&pgno| WalFrameInput {
                                    pgno,
                                    data: self
                                        .cache
                                        .get(&pgno)
                                        .expect("dirty page exists")
                                        .data
                                        .as_slice(),
                                })
                                .collect();
                            wal.write_frames(
                                db_f,
                                ps,
                                &inputs,
                                effective_n_truncate,
                                true,
                                w_sync,
                                c_sync,
                            )?;
                        }

                        self.cache.retain(|&pgno, _| pgno <= self.page_count);
                        for pgno in dirty_pgnos {
                            if let Some(page) = self.cache.get_mut(&pgno) {
                                page.dirty = false;
                            }
                        }
                    } else if self.page_count == 0 {
                        self.wal
                            .as_mut()
                            .expect("use_wal checked")
                            .undo(|_| Ok(()))?;
                    }

                    self.wal
                        .as_mut()
                        .expect("use_wal checked")
                        .end_write_transaction()?;

                    self.commit_phase_one_done = false;
                    self.db_file_modified = false;
                    self.cache_modified = false;
                    self.change_counter_bumped = false;
                    self.journal_started_in_tx = false;
                    self.pending_byte_copy = None;
                    self.staged_raw_copy = false;
                    self.tx_preimages.clear();
                    self.tx_journaled.clear_all();
                    self.subsavepoints.clear();
                    self.header_loaded = true;
                    self.page_size_fixed = self.page_count > 0;

                    return self.finish_tx_to_read_or_open(retain_read);
                }

                // Phase 1 (`sqlite3PagerCommitPhaseOne` / `PAGER_WRITER_FINISHED`, `pager.c:6740`):
                // Skipped on retry if Phase 1 already completed and only Phase 2 (`finalize_journal_after_tx`)
                // failed transiently, preventing re-writing `JOURNAL_MAGIC` over a truncated/zeroed journal!
                if !self.commit_phase_one_done {
                    let lock_byte_pgno = self.lock_byte_page();
                    let has_dirty_pages = self.cache.iter().any(|(&pgno, p)| {
                        p.dirty && pgno <= self.page_count && pgno != lock_byte_pgno
                    });
                    let needs_db_flush = self.cache_modified
                        || has_dirty_pages
                        || self.page_count != self.tx_orig_page_count
                        || self.pending_byte_copy.is_some();

                    if needs_db_flush {
                        // Bump file change counter & version_valid_for on Page 1 (at most once per tx
                        // so retrying `commit` after `SQLITE_BUSY` does not double-increment).
                        // Skip overwriting Page 1 header when `pending_byte_copy` is active so Page 1
                        // retains its serialized `pgsz_src` header while `self.header` remains `pgsz_dest`.
                        if self.page_count > 0 && self.pending_byte_copy.is_none() {
                            self.mark_page_writable(1)?;
                            self.header.database_size_pages = self.page_count;
                            if !self.change_counter_bumped {
                                self.header.change_counter =
                                    self.header.change_counter.wrapping_add(1);
                                if self.header.change_counter == 0 {
                                    self.header.change_counter = 1;
                                }
                                self.header.version_valid_for = self.header.change_counter;
                                self.header.sqlite_version_number = SQLITE_VERSION_NUMBER;
                                self.change_counter_bumped = true;
                            }

                            let hdr_copy = self.header.clone();
                            let p1 = self.cache.get_mut(&1).expect("page 1 in cache");
                            hdr_copy.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
                            p1.dirty = true;
                        }

                        // Step 1: Acquire EXCLUSIVE lock BEFORE modifying `.db` (`syncJournal` in `pager.c:4349`).
                        // If `lock(Exclusive)` returns `Busy`, `db_file_modified` remains `false` and the
                        // write transaction + dirty cache stay intact in `TransactionState::Write`!
                        self.lock_db_file(LockLevel::Exclusive)?;

                        // Step 2: Update `nRec` in rollback journal header and sync BEFORE touching `.db`!
                        if self.journal_started_in_tx
                            && let Some(ref jfile) = self.journal_file
                        {
                            self.sync_journal_before_db_write(&**jfile)?;
                        }

                        // Step 3: Write all dirty pages in ascending `Pgno` order to `.db`, truncate, and sync.
                        self.db_file_modified = true;

                        let ps = u64::from(self.header.page_size);
                        let mut dirty_pgnos: Vec<Pgno> = self
                            .cache
                            .iter()
                            .filter_map(|(&pgno, p)| {
                                if p.dirty && pgno <= self.page_count && pgno != lock_byte_pgno {
                                    Some(pgno)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        dirty_pgnos.sort_unstable();

                        for &pgno in &dirty_pgnos {
                            let page = self.cache.get(&pgno).expect("dirty page exists");
                            let offset = u64::from(pgno - 1) * ps;
                            self.db_file()?.write_at(&page.data, offset)?;
                        }

                        let target_size = if let Some(ref pending) = self.pending_byte_copy {
                            if let Some((tail_off, ref tail_bytes)) = pending.post_pending_tail {
                                self.db_file()?.write_at(tail_bytes, tail_off)?;
                            }
                            pending.raw_byte_size
                        } else {
                            let effective_disk_pages = if self.page_count == lock_byte_pgno {
                                self.page_count - 1
                            } else {
                                self.page_count
                            };
                            u64::from(effective_disk_pages) * ps
                        };
                        if self.db_file()?.file_size()? != target_size {
                            self.db_file()?.truncate(target_size)?;
                        }

                        if !self.is_memdb_backing
                            && self.db_kind != DatabaseKind::PrivateTemporary
                            && self.synchronous > 0
                        {
                            self.db_file()?.sync(SyncFlags::Normal)?;
                        }

                        self.cache.retain(|&pgno, _| pgno <= self.page_count);
                        for pgno in dirty_pgnos {
                            if let Some(page) = self.cache.get_mut(&pgno) {
                                page.dirty = false;
                            }
                        }
                        self.commit_phase_one_done = true;
                    }
                }

                // Phase 2 (`sqlite3PagerCommitPhaseTwo`): Finalize rollback journal according to `JournalMode`
                // (including zero-write `Write` transactions per `lock-7.3`!).
                // `journal_started_in_tx`, `db_file_modified`, and `tx_preimages` are cleared ONLY AFTER
                // `finalize_journal_after_tx(true)` succeeds so both `commit()` retry and `rollback()` are safe!
                self.finalize_journal_after_tx(true)?;

                if let Some(pending) = self.pending_byte_copy.take() {
                    self.cache.clear();
                    self.header = pending.new_header;
                    self.page_count = pending.new_page_count;
                    self.next_page_size = None;
                }
                self.staged_raw_copy = false;
                self.commit_phase_one_done = false;
                self.db_file_modified = false;
                self.cache_modified = false;
                self.change_counter_bumped = false;
                self.journal_started_in_tx = false;
                self.tx_preimages.clear();
                self.tx_journaled.clear_all();
                self.subsavepoints.clear();
                self.header_loaded = true;
                self.page_size_fixed = self.page_count > 0;

                self.finish_tx_to_read_or_open(retain_read)
            }
        }
    }

    /// Rolls back the active transaction, releasing locks back to `TransactionState::Open`.
    pub fn rollback(&mut self) -> Result<()> {
        self.rollback_retaining_read(false)
    }

    /// Rolls back the active write transaction, restoring pre-images and original page count
    /// (or discarding dirty disk cache when `JournalMode::Off`), then downgrading to
    /// `TransactionState::Read` (`retain_read == true`) or `TransactionState::Open` (`retain_read == false`).
    pub fn rollback_retaining_read(&mut self, retain_read: bool) -> Result<()> {
        self.ensure_not_closed()?;
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => self.finish_tx_to_read_or_open(retain_read),
            TransactionState::Write => {
                if self.use_wal() {
                    let orig_count = self.tx_orig_page_count;
                    let mut reverted_pgnos = Vec::new();
                    self.wal.as_mut().expect("use_wal checked").undo(|pgno| {
                        reverted_pgnos.push(pgno);
                        Ok(())
                    })?;
                    for pgno in reverted_pgnos {
                        self.cache.remove(&pgno);
                    }

                    self.cache.retain(|&pgno, _| pgno <= orig_count);
                    for (pgno, orig_data) in self.tx_preimages.drain() {
                        if pgno <= orig_count {
                            self.cache.insert(
                                pgno,
                                CachedPage {
                                    data: orig_data,
                                    dirty: false,
                                },
                            );
                        }
                    }

                    self.page_count = orig_count;
                    self.header = self.tx_orig_header.clone();
                    self.page_size_fixed = orig_count > 0;
                    self.tx_journaled.clear_all();
                    self.commit_phase_one_done = false;
                    self.db_file_modified = false;
                    self.cache_modified = false;
                    self.change_counter_bumped = false;
                    self.journal_started_in_tx = false;
                    self.pending_byte_copy = None;
                    self.staged_raw_copy = false;
                    self.subsavepoints.clear();

                    self.wal
                        .as_mut()
                        .expect("use_wal checked")
                        .end_write_transaction()?;
                    return self.finish_tx_to_read_or_open(retain_read);
                }

                if self.journal_mode == JournalMode::Off {
                    return self.rollback_journal_mode_off(retain_read);
                }

                let orig_count = self.tx_orig_page_count;

                // If `commit` already started modifying `.db` (`db_file_modified`), restore all
                // pre-images and original file size on disk and sync BEFORE finalizing/deleting
                // `<db>-journal`. If any disk operation fails here, keep `<db>-journal` intact on
                // disk so hot-journal recovery can still restore atomicity!
                if self.db_file_modified && !self.is_memdb_backing {
                    self.lock_db_file(LockLevel::Exclusive)?;
                    let ps = u64::from(self.tx_orig_header.page_size);
                    let lock_byte_pgno = self
                        .lock_geometry
                        .lock_byte_page(self.tx_orig_header.page_size);
                    let mut restore_pgnos: Vec<Pgno> = self
                        .tx_preimages
                        .keys()
                        .copied()
                        .filter(|&pgno| pgno > 0 && pgno <= orig_count && pgno != lock_byte_pgno)
                        .collect();
                    restore_pgnos.sort_unstable();

                    for pgno in restore_pgnos {
                        let orig_data = self.tx_preimages.get(&pgno).expect("key exists");
                        let offset = u64::from(pgno - 1) * ps;
                        self.db_file()?.write_at(orig_data, offset)?;
                    }

                    let orig_size = u64::from(orig_count) * ps;
                    if self.db_file()?.file_size()? != orig_size {
                        self.db_file()?.truncate(orig_size)?;
                    }
                    if self.db_kind != DatabaseKind::PrivateTemporary && self.synchronous > 0 {
                        self.db_file()?.sync(SyncFlags::Normal)?;
                    }
                    self.db_file_modified = false;
                }

                // Remove any pages allocated during the aborted transaction
                self.cache.retain(|&pgno, _| pgno <= orig_count);

                // Restore pre-images into cache
                for (pgno, orig_data) in self.tx_preimages.drain() {
                    if pgno <= orig_count {
                        self.cache.insert(
                            pgno,
                            CachedPage {
                                data: orig_data,
                                dirty: false,
                            },
                        );
                    }
                }

                self.page_count = orig_count;
                self.header = self.tx_orig_header.clone();
                self.page_size_fixed = orig_count > 0;
                self.tx_journaled.clear_all();
                self.commit_phase_one_done = false;
                self.db_file_modified = false;
                self.cache_modified = false;
                self.change_counter_bumped = false;
                self.pending_byte_copy = None;
                self.staged_raw_copy = false;
                self.subsavepoints.clear();

                // Finalize journal file (`Delete` unlinks even if `journal_file` was already closed
                // by a failed `commit()`, `Truncate` truncates to 0, `Persist` zeroes 28B) BEFORE
                // clearing `self.journal_started_in_tx`!
                self.finalize_journal_after_tx(false)?;
                self.journal_started_in_tx = false;

                self.finish_tx_to_read_or_open(retain_read)
            }
        }
    }

    fn rollback_journal_mode_off(&mut self, retain_read: bool) -> Result<()> {
        let was_modified =
            self.cache_modified || self.db_file_modified || self.cache.values().any(|p| p.dirty);

        if self.is_memdb_backing {
            // `pager.c:6850` / `2178`: in `MEMDB` with `journal_mode=OFF`, dirty flags are cleared
            // and the current in-memory pages remain intact!
            if was_modified && self.page_count > 0 {
                self.header.database_size_pages = self.page_count;
                let hdr_copy = self.header.clone();
                if let Some(p1) = self.cache.get_mut(&1) {
                    hdr_copy.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
                }
            }
            let ps = u64::from(self.header.page_size);
            self.cache.retain(|&pgno, _| pgno <= self.page_count);
            if let Some(ref db_file) = self.file {
                for (&pgno, page) in &mut self.cache {
                    if page.dirty {
                        let offset = u64::from(pgno - 1) * ps;
                        let _ = db_file.write_at(&page.data, offset);
                        page.dirty = false;
                    }
                }
                let target_size = u64::from(self.page_count) * ps;
                let _ = db_file.truncate(target_size);
            }
            self.page_size_fixed = self.page_count > 0;
            self.header_loaded = true;
        } else if was_modified {
            // Disk DB with `journal_mode=OFF` (`savepoint-13.4`): discard dirty in-memory cache
            // and mark `header_loaded = false` so subsequent reads reload untouched pages from disk!
            self.cache.clear();
            self.header = self.tx_orig_header.clone();
            self.page_count = 0;
            self.page_size_fixed = false;
            self.header_loaded = false;
        }

        self.commit_phase_one_done = false;
        self.db_file_modified = false;
        self.cache_modified = false;
        self.change_counter_bumped = false;
        self.pending_byte_copy = None;
        self.staged_raw_copy = false;
        self.tx_preimages.clear();
        self.tx_journaled.clear_all();
        self.subsavepoints.clear();
        self.finalize_journal_after_tx(false)?;
        self.journal_started_in_tx = false;

        if retain_read {
            if self.locking_mode == LockingMode::Normal {
                self.unlock_db_file(LockLevel::Shared)?;
            }
            if !self.header_loaded
                && let Err(e) = self.begin_read_under_shared_lock()
            {
                if self.locking_mode == LockingMode::Normal {
                    let _ = self.unlock_db_file(LockLevel::NoLock);
                }
                self.state = TransactionState::Open;
                return Err(e);
            }
            self.state = TransactionState::Read;
        } else {
            if self.locking_mode == LockingMode::Normal {
                self.unlock_db_file(LockLevel::NoLock)?;
            }
            self.state = TransactionState::Open;
        }
        Ok(())
    }

    /// Finalizes `self.journal_file` at the end of commit or rollback (`pager_end_transaction`,
    /// `sqlite/src/pager.c:2117-2161` and `zeroJournalHdr`, `pager.c:1436-1468`).
    fn finalize_journal_after_tx(&mut self, is_commit: bool) -> Result<()> {
        if self.journal_mode == JournalMode::Wal {
            return Ok(());
        }
        if matches!(
            self.journal_mode,
            JournalMode::Delete | JournalMode::Memory | JournalMode::Off
        ) {
            if let Some(mut jfd) = self.journal_file.take() {
                let close_res = jfd.close();
                self.journal_off = 0;
                close_res?;
            } else {
                self.journal_off = 0;
            }

            if self.journal_started_in_tx
                && self.journal_mode == JournalMode::Delete
                && self.db_kind == DatabaseKind::NamedDisk
                && let Some(ref jpath) = self.journal_path
            {
                let sync_dir = is_commit && self.synchronous > 0;
                self.vfs.delete(jpath, sync_dir)?;
            }
            return Ok(());
        }

        let Some(ref jfd_check) = self.journal_file else {
            self.journal_off = 0;
            return Ok(());
        };
        let _ = jfd_check;

        match self.journal_mode {
            JournalMode::Truncate => {
                if self.journal_off > 0 {
                    let jfd = self.journal_file.as_deref().expect("checked Some");
                    jfd.truncate(0)?;
                    if self.synchronous >= 2 {
                        let sflags = if self.synchronous >= 3 {
                            SyncFlags::Full
                        } else {
                            SyncFlags::Normal
                        };
                        jfd.sync(sflags)?;
                    }
                }
                self.journal_off = 0;
                Ok(())
            }
            JournalMode::Persist => {
                if self.journal_off > 0 {
                    let is_temp_file = self.db_kind == DatabaseKind::PrivateTemporary;
                    let has_super = false;
                    let limit = self.journal_size_limit;
                    let jfd = self.journal_file.as_deref().expect("checked Some");

                    if is_temp_file || has_super || limit == 0 {
                        jfd.truncate(0)?;
                    } else {
                        // Zero ONLY the first 28 bytes (`zeroJournalHdr` in `pager.c:1447`),
                        // leaving bytes 28..sector_size untouched!
                        let zero_hdr = [0u8; JOURNAL_HEADER_CORE_BYTES];
                        jfd.write_at(&zero_hdr, 0)?;
                    }

                    if self.synchronous > 0 {
                        jfd.sync(SyncFlags::DataOnly)?;
                    }

                    if limit > 0 {
                        let limit_u64 = limit as u64;
                        if jfd.file_size()? > limit_u64 {
                            jfd.truncate(limit_u64)?;
                        }
                    }
                }
                self.journal_off = 0;
                Ok(())
            }
            JournalMode::Delete | JournalMode::Memory | JournalMode::Off | JournalMode::Wal => {
                unreachable!("handled above")
            }
        }
    }

    // ========================================================================
    // Indexed Anonymous Physical Sub-Savepoint Slots (`implementation_plan.md:181-186`)
    // ========================================================================

    /// Returns the number of open physical sub-savepoint slots on this pager.
    #[inline]
    pub fn subsavepoint_depth(&self) -> usize {
        self.subsavepoints.len()
    }

    /// Ensures at least `target_depth` anonymous physical sub-savepoint slots are open **only**
    /// when `self.state == TransactionState::Write`. Never forces `begin_write()` from `Open`/`Read`.
    pub fn ensure_subsavepoints(&mut self, target_depth: usize) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Ok(());
        }
        while self.subsavepoints.len() < target_depth {
            self.subsavepoints.push(SubsavepointSlot {
                orig_page_count: self.page_count,
                orig_header: self.header.clone(),
                saved_pages: HashMap::new(),
                wal_savepoint: self.wal.as_ref().map(Wal::savepoint),
            });
        }
        Ok(())
    }

    /// Rolls back changes to sub-savepoint `index`, discarding slots above `index` and keeping
    /// slot `index` active (with cleared `saved_pages`) on the stack. No-op if `state != Write`
    /// or `index >= subsavepoints.len()`.
    pub fn rollback_to_subsavepoint(&mut self, index: usize) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write || index >= self.subsavepoints.len() {
            return Ok(());
        }
        while self.subsavepoints.len() > index + 1 {
            let sp = self.subsavepoints.pop().expect("len checked");
            self.apply_subsavepoint_undo(&sp);
        }
        if self.use_wal()
            && let Some(mut wsp) = self.subsavepoints[index].wal_savepoint
        {
            self.wal
                .as_mut()
                .expect("use_wal checked")
                .savepoint_undo(&mut wsp)?;
            self.subsavepoints[index].wal_savepoint = Some(wsp);
            // Evict any clean pages loaded from spilled WAL frames so restored savepoint pages
            // or committed WAL/DB pages are used instead.
            self.cache.retain(|_, p| p.dirty);
        }
        let target = self.subsavepoints[index].clone();
        self.apply_subsavepoint_undo(&target);
        self.subsavepoints[index].saved_pages.clear();
        Ok(())
    }

    fn apply_subsavepoint_undo(&mut self, sp: &SubsavepointSlot) {
        self.commit_phase_one_done = false;
        self.pending_byte_copy = None;
        self.staged_raw_copy = false;
        self.cache.retain(|&pgno, _| pgno <= sp.orig_page_count);
        for (&pgno, orig_bytes) in &sp.saved_pages {
            if pgno <= sp.orig_page_count {
                self.cache.insert(
                    pgno,
                    CachedPage {
                        data: orig_bytes.clone(),
                        dirty: true,
                    },
                );
            }
        }
        self.page_count = sp.orig_page_count;
        self.header = sp.orig_header.clone();
        if self.page_count == 0 {
            self.page_size_fixed = self.tx_orig_page_count > 0;
        } else if let Some(p1) = self.cache.get_mut(&1) {
            self.header.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
        }
    }

    /// Releases sub-savepoint `index` and any inner slots (`index..`), merging their pre-images
    /// into parent slot `index - 1` when `index > 0`.
    pub fn release_subsavepoint(&mut self, index: usize) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write || index >= self.subsavepoints.len() {
            return Ok(());
        }
        let drained: Vec<SubsavepointSlot> = self.subsavepoints.drain(index..).collect();
        if index > 0 {
            let parent = &mut self.subsavepoints[index - 1];
            for sp in drained {
                for (pgno, data) in sp.saved_pages {
                    if pgno <= parent.orig_page_count {
                        parent.saved_pages.entry(pgno).or_insert(data);
                    }
                }
            }
        }
        Ok(())
    }

    // ========================================================================
    // Explicit Close (`sqlite3PagerClose` with error surfacing)
    // ========================================================================

    /// Explicitly rolls back any active transaction, checkpoints & unlinks WAL/SHM if this is the
    /// last open connection (`sqlite3WalClose`), closes open journal and database file handles,
    /// releases OS locks (and deletes `PrivateTemporary` files via `DELETEONCLOSE`), surfacing the
    /// first error encountered.
    pub fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }

        let mut first_err: Option<SqliteError> = None;

        if self.state != TransactionState::Open
            && let Err(e) = self.rollback_retaining_read(false)
        {
            // Preserve `<db>-journal` on disk for future hot-journal recovery if rollback failed!
            first_err = Some(e);
            self.state = TransactionState::Open;
        }

        if let Some(wal) = self.wal.take() {
            let ckpt_sync = self.ckpt_sync_flags();
            let persist_wal = self.persist_wal;
            if let Some(ref db_fd) = self.file {
                if let Err(e) = wal.close(db_fd.as_ref(), ckpt_sync, persist_wal)
                    && first_err.is_none()
                {
                    first_err = Some(e);
                }
            } else {
                wal.abort_open();
            }
        }

        if let Some(mut jfd) = self.journal_file.take()
            && let Err(e) = jfd.close()
            && first_err.is_none()
        {
            first_err = Some(e);
        }
        self.journal_off = 0;

        if let Some(mut fd) = self.file.take()
            && let Err(e) = fd.close()
            && first_err.is_none()
        {
            first_err = Some(e);
        }

        self.cache.clear();
        self.subsavepoints.clear();
        self.tx_preimages.clear();
        self.tx_journaled.clear_all();
        self.pending_byte_copy = None;
        self.staged_raw_copy = false;
        self.commit_phase_one_done = false;
        self.journal_started_in_tx = false;
        self.state = TransactionState::Open;
        self.closed = true;

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    // ========================================================================
    // Rollback Journal Binary Format (`writeJournalHdr`, `syncJournal`, `pager_playback`)
    // ========================================================================

    fn effective_sector_size(&self) -> u32 {
        let raw = self
            .file
            .as_deref()
            .map_or(4096, VfsFile::sector_size)
            .clamp(MIN_JOURNAL_HEADER_BYTES as u32, MAX_JOURNAL_SECTOR_BYTES);
        if raw.is_power_of_two() {
            raw
        } else {
            raw.next_power_of_two()
                .clamp(MIN_JOURNAL_HEADER_BYTES as u32, MAX_JOURNAL_SECTOR_BYTES)
        }
    }

    fn write_initial_journal_header(&self, jfile: &dyn VfsFile, record_count: u32) -> Result<()> {
        let hdr_sz = (self.journal_sector_size as usize).max(MIN_JOURNAL_HEADER_BYTES);
        let mut hdr = vec![0u8; hdr_sz];
        hdr[0..8].copy_from_slice(&JOURNAL_MAGIC);
        hdr[8..12].copy_from_slice(&record_count.to_be_bytes());
        hdr[12..16].copy_from_slice(&self.journal_nonce.to_be_bytes());
        hdr[16..20].copy_from_slice(&self.tx_orig_page_count.to_be_bytes());
        hdr[20..24].copy_from_slice(&self.journal_sector_size.to_be_bytes());
        hdr[24..28].copy_from_slice(&self.tx_orig_header.page_size.to_be_bytes());
        jfile.write_at(&hdr, 0)
    }

    /// Matches `syncJournal` (`sqlite/src/pager.c:4352-4429`): neutralizes any stale trailing
    /// persistent-journal section magic at the next sector boundary, syncs journal data in full-sync
    /// mode, writes the 12-byte `[JOURNAL_MAGIC, nRec]` prefix at offset 0 (leaving bytes `12..512`
    /// untouched), and syncs the journal.
    fn sync_journal_before_db_write(&self, jfile: &dyn VfsFile) -> Result<()> {
        if self.synchronous == 0 || self.db_kind == DatabaseKind::PrivateTemporary {
            return Ok(());
        }

        let hdr_sz = u64::from(
            self.journal_sector_size
                .max(MIN_JOURNAL_HEADER_BYTES as u32),
        );
        if self.journal_off > 0 {
            let next_hdr_off = ((self.journal_off - 1) / hdr_sz + 1) * hdr_sz;
            if jfile.file_size()? >= next_hdr_off.saturating_add(8) {
                let mut magic = [0u8; 8];
                if jfile.read_at(&mut magic, next_hdr_off)? == 8 && magic == JOURNAL_MAGIC {
                    jfile.write_at(&[0u8], next_hdr_off)?;
                }
            }
        }

        let sflags = if self.synchronous >= 3 {
            SyncFlags::Full
        } else {
            SyncFlags::Normal
        };
        if self.synchronous >= 2 {
            jfile.sync(sflags)?;
        }

        let mut hdr12 = [0u8; 12];
        hdr12[0..8].copy_from_slice(&JOURNAL_MAGIC);
        hdr12[8..12].copy_from_slice(&self.journal_records_written.to_be_bytes());
        jfile.write_at(&hdr12, 0)?;

        jfile.sync(SyncFlags::Normal)
    }

    fn journal_checksum(nonce: u32, page_data: &[u8]) -> u32 {
        // Matches `pager_cksum` in `sqlite/src/pager.c`: stride backwards by 200 bytes from page_size - 200.
        let mut cksum = nonce;
        let mut i = page_data.len();
        while i >= 200 {
            i -= 200;
            cksum = cksum.wrapping_add(u32::from(page_data[i]));
        }
        cksum
    }

    fn append_journal_page(&mut self, pgno: Pgno, page_data: &[u8]) -> Result<()> {
        let Some(ref jfile) = self.journal_file else {
            return Ok(());
        };
        let ps = page_data.len();
        let rec_size = 4 + ps + 4;
        let offset = self.journal_off;

        let cksum = Self::journal_checksum(self.journal_nonce, page_data);
        let mut buf = Vec::with_capacity(rec_size);
        buf.extend_from_slice(&pgno.to_be_bytes());
        buf.extend_from_slice(page_data);
        buf.extend_from_slice(&cksum.to_be_bytes());

        jfile.write_at(&buf, offset)?;
        self.journal_off = offset.saturating_add(rec_size as u64);
        self.journal_records_written += 1;
        Ok(())
    }

    /// Safe hot-journal detection and multi-section recovery (`hasHotJournal` + `readJournalHdr` +
    /// `pager_playback` in `sqlite/src/pager.c`), called ONLY while holding at least
    /// `LockLevel::Shared` on the main database file!
    fn recover_hot_journal_under_shared_lock(&mut self) -> Result<bool> {
        let Some(jpath) = self.journal_path.clone() else {
            return Ok(false);
        };
        if !self.vfs.exists(&jpath)? {
            return Ok(false);
        }

        // CRITICAL (`pager.c:5254`): If ANY handle/process holds RESERVED (or higher),
        // an active writer owns `<db>-journal`. It is NOT a hot journal — do not touch it!
        if self.db_file()?.check_reserved_lock()? {
            return Ok(false);
        }

        let Ok(mut jfile) = self.vfs.open(Some(&jpath), OpenFlags::READONLY) else {
            return Ok(false);
        };
        let jsize = match jfile.file_size() {
            Ok(s) => s,
            Err(e) => {
                let _ = jfile.close();
                return Err(e);
            }
        };
        if jsize == 0 {
            let _ = jfile.close();
            return Ok(false);
        }

        let mut first_byte = [0u8; 1];
        let n_first = match jfile.read_at(&mut first_byte, 0) {
            Ok(n) => n,
            Err(e) => {
                let _ = jfile.close();
                return Err(e);
            }
        };
        if n_first == 0 || first_byte[0] == 0 {
            // `journal_mode=PERSIST` zeroes byte 0 to mark an inactive journal.
            let _ = jfile.close();
            return Ok(false);
        }

        if self.readonly {
            let _ = jfile.close();
            return Err(SqliteError::with_extended(
                extended::SQLITE_READONLY_ROLLBACK,
                "unable to open database: hot journal requires write access for recovery",
            ));
        }

        // Upgrade directly from SHARED -> EXCLUSIVE (`pager.c:5606`); fails with SQLITE_BUSY
        // if another reader also holds SHARED, leaving the hot journal intact for retry.
        if let Err(e) = self.lock_db_file(LockLevel::Exclusive) {
            let _ = jfile.close();
            return Err(e);
        }

        // Close any cached persistent handle before modifying/unlinking `jpath`.
        if let Some(mut cached_jf) = self.journal_file.take() {
            let _ = cached_jf.close();
            self.journal_off = 0;
        }

        if jsize < JOURNAL_HEADER_CORE_BYTES as u64 {
            let _ = jfile.close();
            self.vfs.delete(&jpath, true)?;
            self.unlock_db_file(LockLevel::Shared)?;
            return Ok(true);
        }

        let mut hdr0 = [0u8; JOURNAL_HEADER_CORE_BYTES];
        if let Err(e) = jfile.read_at(&mut hdr0, 0) {
            let _ = jfile.close();
            return Err(e);
        }
        if hdr0[0..8] != JOURNAL_MAGIC {
            let _ = jfile.close();
            self.vfs.delete(&jpath, true)?;
            self.unlock_db_file(LockLevel::Shared)?;
            return Ok(true);
        }

        let orig_pages = u32::from_be_bytes([hdr0[16], hdr0[17], hdr0[18], hdr0[19]]);
        let raw_sector_size = u32::from_be_bytes([hdr0[20], hdr0[21], hdr0[22], hdr0[23]]);
        let raw_page_size = u32::from_be_bytes([hdr0[24], hdr0[25], hdr0[26], hdr0[27]]);
        let page_size = if raw_page_size == 0 {
            // Legacy journal header without page size (`pager.c:1686`): inspect `.db` bytes 16..18
            // before falling back to `self.header.page_size`.
            let mut ps_bytes = [0u8; 2];
            if self.db_file()?.file_size()? >= 18
                && self.db_file()?.read_at(&mut ps_bytes, 16)? == 2
            {
                decode_db_header_page_size(ps_bytes).unwrap_or(self.header.page_size)
            } else {
                self.header.page_size
            }
        } else {
            raw_page_size
        };

        // Validate `page_size` and `sector_size` per `readJournalHdr` (`pager.c:1700-1710`).
        let valid_sector = (32..=MAX_JOURNAL_SECTOR_BYTES).contains(&raw_sector_size)
            && raw_sector_size.is_power_of_two();
        let hdr_sz = u64::from(raw_sector_size);
        if !DatabaseHeader::is_valid_page_size(page_size) || !valid_sector || jsize < hdr_sz {
            let _ = jfile.close();
            self.vfs.delete(&jpath, true)?;
            self.unlock_db_file(LockLevel::Shared)?;
            return Ok(true);
        }

        let playback_res: Result<()> = (|| {
            // Truncate main DB back to its pre-transaction size (`pager.c:2959`).
            self.db_file()?
                .truncate(u64::from(orig_pages) * u64::from(page_size))?;

            let ps = page_size as usize;
            let rec_size = (4 + ps + 4) as u64;
            let sj_pgno = self.lock_geometry.sj_pgno(page_size);
            let mut replayed_pages = Bitvec::new(orig_pages.max(1));
            let mut rec_buf = vec![0u8; rec_size as usize];
            let mut journal_off: u64 = 0;

            // Multi-section playback loop (`pager.c:2913-3003`):
            // C SQLite writes a new sector-aligned journal header each time its page cache spills.
            'sections: loop {
                if journal_off > 0 {
                    journal_off = ((journal_off - 1) / hdr_sz + 1) * hdr_sz;
                }
                if journal_off.saturating_add(hdr_sz) > jsize {
                    break;
                }

                let mut sec_hdr = [0u8; JOURNAL_HEADER_CORE_BYTES];
                jfile.read_at(&mut sec_hdr, journal_off)?;
                if sec_hdr[0..8] != JOURNAL_MAGIC {
                    break;
                }

                let raw_nrec =
                    u32::from_be_bytes([sec_hdr[8], sec_hdr[9], sec_hdr[10], sec_hdr[11]]);
                let nonce =
                    u32::from_be_bytes([sec_hdr[12], sec_hdr[13], sec_hdr[14], sec_hdr[15]]);
                journal_off += hdr_sz;

                let nrec: u64 = if raw_nrec == 0xffff_ffff || raw_nrec == 0 {
                    (jsize.saturating_sub(journal_off)) / rec_size
                } else {
                    u64::from(raw_nrec)
                };

                for _ in 0..nrec {
                    if journal_off.saturating_add(rec_size) > jsize {
                        // Truncated record at tail (`SQLITE_IOERR_SHORT_READ` in `pager.c:2985`): stop playback cleanly.
                        break 'sections;
                    }
                    jfile.read_at(&mut rec_buf, journal_off)?;
                    journal_off += rec_size;

                    let pgno = u32::from_be_bytes([rec_buf[0], rec_buf[1], rec_buf[2], rec_buf[3]]);
                    if pgno == 0 || pgno == sj_pgno {
                        break 'sections;
                    }
                    if pgno > orig_pages || replayed_pages.test(pgno) {
                        continue;
                    }
                    let data = &rec_buf[4..4 + ps];
                    let stored_cksum = u32::from_be_bytes([
                        rec_buf[4 + ps],
                        rec_buf[4 + ps + 1],
                        rec_buf[4 + ps + 2],
                        rec_buf[4 + ps + 3],
                    ]);
                    if Self::journal_checksum(nonce, data) != stored_cksum {
                        break 'sections;
                    }
                    let db_off = u64::from(pgno - 1) * u64::from(page_size);
                    self.db_file()?.write_at(data, db_off)?;
                    replayed_pages.set(pgno)?;
                }

                // Both `0xffffffff` (no-sync mode) and `0` (active un-spilled tail section) consume up to EOF.
                if raw_nrec == 0xffff_ffff || raw_nrec == 0 {
                    break;
                }
            }

            if !self.is_memdb_backing && self.db_kind != DatabaseKind::PrivateTemporary {
                self.db_file()?.sync(SyncFlags::Normal)?;
            }
            Ok(())
        })();

        let close_res = jfile.close();
        playback_res?;
        close_res?;

        // Finalize recovered hot journal: even if this connection was configured with `Memory`,
        // delete the recovered on-disk hot journal (`pager.c:2145-2159`), or zero/truncate if
        // this connection is in `Persist`/`Truncate` mode (`pager_end_transaction` -> delete on
        // hot-journal recovery is always safe).
        self.vfs.delete(&jpath, true)?;
        self.cache.clear();
        self.page_size_fixed = false;
        self.header_loaded = false;
        self.unlock_db_file(LockLevel::Shared)?;
        Ok(true)
    }
}

impl Drop for Pager {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pager_commit_rollback_and_savepoints() {
        let vfs = Arc::new(MemoryVfs::new());
        let mut pager = Pager::open_with_vfs(
            vfs.clone(),
            Some(Path::new("unit.db")),
            OpenFlags::READWRITE | OpenFlags::CREATE,
        )
        .unwrap();

        assert_eq!(pager.transaction_state(), TransactionState::Open);
        assert!(!pager.is_header_loaded());

        pager.begin_write().unwrap();
        assert_eq!(pager.page_count(), 1);
        let p2 = pager.allocate_raw_page().unwrap();
        assert_eq!(p2, 2);
        pager.write_page(2).unwrap()[0..4].copy_from_slice(b"ABCD");
        pager.commit().unwrap();

        // Reopen: lazy `Open/NoLock` starts with `page_count == 0`, then `begin_read()` loads header!
        let mut pager2 = Pager::open_with_vfs(
            vfs.clone(),
            Some(Path::new("unit.db")),
            OpenFlags::READWRITE,
        )
        .unwrap();
        assert_eq!(pager2.page_count(), 0);
        assert!(!pager2.is_header_loaded());

        pager2.begin_read().unwrap();
        assert_eq!(pager2.page_count(), 2);
        assert_eq!(&pager2.read_page(2).unwrap()[0..4], b"ABCD");
        pager2.end_read_if_unused().unwrap();
        assert_eq!(pager2.transaction_state(), TransactionState::Open);

        // Modify in write tx + indexed sub-savepoint, rollback sub-savepoint, then rollback tx
        pager2.begin_write().unwrap();
        pager2.write_page(2).unwrap()[0..4].copy_from_slice(b"XXXX");
        pager2.ensure_subsavepoints(1).unwrap();
        assert_eq!(pager2.subsavepoint_depth(), 1);
        let p3 = pager2.allocate_raw_page().unwrap();
        assert_eq!(p3, 3);
        pager2.write_page(2).unwrap()[0..4].copy_from_slice(b"YYYY");

        pager2.rollback_to_subsavepoint(0).unwrap();
        assert_eq!(pager2.subsavepoint_depth(), 1);
        assert_eq!(pager2.page_count(), 2);
        assert_eq!(&pager2.read_page(2).unwrap()[0..4], b"XXXX");
        pager2.release_subsavepoint(0).unwrap();
        assert_eq!(pager2.subsavepoint_depth(), 0);

        pager2.rollback().unwrap();
        pager2.begin_read().unwrap();
        assert_eq!(pager2.page_count(), 2);
        assert_eq!(&pager2.read_page(2).unwrap()[0..4], b"ABCD");
        pager2.close().unwrap();
    }
}
