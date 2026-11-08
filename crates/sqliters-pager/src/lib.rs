//! Rollback journal (`pager.c`), page cache (`pcache1.c`), transactions, journal modes,
//! and indexed anonymous physical sub-savepoints for SQLite 3.54.1.
#![forbid(unsafe_code)]

pub mod header;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqliters_types::{
    Bitvec, DatabaseKind, ErrorCode, LockGeometry, Pgno, Result, SQLITE_DEFAULT_PAGE_SIZE,
    SQLITE_VERSION_NUMBER, SqliteError, TextEncoding, decode_db_header_page_size, extended,
};
use sqliters_vfs::{LockLevel, MemoryVfs, OpenFlags, OsVfs, SyncFlags, Vfs, VfsFile};

pub use header::{DB_HEADER_SIZE, DatabaseHeader};

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

/// Rollback journal operating mode (`PAGER_JOURNALMODE_*` in `sqlite/src/pager.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JournalMode {
    Delete,
    Truncate,
    Persist,
    Memory,
    Off,
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
}

impl Default for PagerOpenOptions {
    fn default() -> Self {
        Self {
            default_page_size: SQLITE_DEFAULT_PAGE_SIZE,
            lock_geometry: None,
            db_kind: None,
            temp_in_memory: false,
        }
    }
}

#[derive(Debug, Clone)]
struct SubsavepointSlot {
    orig_page_count: Pgno,
    orig_header: DatabaseHeader,
    /// Pre-images of pages (`<= orig_page_count`) modified since this subsavepoint opened.
    saved_pages: HashMap<Pgno, Vec<u8>>,
}

struct CachedPage {
    data: Vec<u8>,
    dirty: bool,
}

#[inline]
fn derive_sidecar_path(canonical_db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = canonical_db_path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// Manages page-level reads, writes, caching, rollback journaling, crash recovery, and sub-savepoints.
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

    readonly: bool,
    closed: bool,
    synchronous: u8,
    journal_mode: JournalMode,
    journal_size_limit: i64,

    header: DatabaseHeader,
    header_loaded: bool,
    page_size_fixed: bool,
    page_count: Pgno,

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
            readonly,
            closed: false,
            synchronous: 2,
            journal_mode,
            journal_size_limit: -1,
            header: candidate_header,
            header_loaded: false,
            page_size_fixed: false,
            page_count: 0,
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
        if let Some(ref f) = self.file {
            f.lock(level)?;
        }
        Ok(())
    }

    #[inline]
    fn unlock_db_file(&self, level: LockLevel) -> Result<()> {
        if let Some(ref f) = self.file {
            f.unlock(level)?;
        }
        Ok(())
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
    pub fn set_synchronous(&mut self, level: u8) {
        self.synchronous = level.min(3);
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

    /// Gets/sets the size limit for persistent journals (`sqlite3PagerJournalSizeLimit`, `pager.c:7549`).
    /// Setting `limit >= -1` updates the limit (`-1` = unlimited); `limit < -1` is a query-only no-op.
    #[inline]
    pub fn set_journal_size_limit(&mut self, limit: i64) -> i64 {
        if limit >= -1 {
            self.journal_size_limit = limit;
        }
        self.journal_size_limit
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

    /// Updates the rollback journal mode if allowed (`sqlite3PagerSetJournalMode`, `pager.c:7437`),
    /// returning the resulting active [`JournalMode`].
    pub fn set_journal_mode(&mut self, mode: JournalMode) -> Result<JournalMode> {
        self.ensure_not_closed()?;
        if !self.ok_to_change_journal_mode() {
            return Ok(self.journal_mode);
        }

        let target_mode = if self.is_memdb_backing {
            match mode {
                JournalMode::Memory | JournalMode::Off => mode,
                JournalMode::Delete | JournalMode::Truncate | JournalMode::Persist => {
                    self.journal_mode
                }
            }
        } else {
            mode
        };

        let old_mode = self.journal_mode;
        if target_mode == old_mode {
            return Ok(self.journal_mode);
        }

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

    /// Sets the page size before the database is initialized (`PRAGMA page_size = N`).
    pub fn set_page_size(&mut self, new_size: u32) -> Result<()> {
        self.ensure_not_closed()?;
        if !DatabaseHeader::is_valid_page_size(new_size) {
            return Err(SqliteError::misuse(format!(
                "invalid page size {new_size}: must be power of two in 512..=65536"
            )));
        }
        if self.state == TransactionState::Open && !self.header_loaded {
            let _ = self.ensure_header_loaded()?;
        }
        if self.page_size_fixed && new_size != self.header.page_size {
            // In SQLite, PRAGMA page_size on an existing DB without VACUUM is a no-op.
            return Ok(());
        }
        if !self.page_size_fixed && self.page_count == 0 {
            self.header.page_size = new_size;
        }
        Ok(())
    }

    fn reject_if_wal_present(&self) -> Result<()> {
        if let Some(ref wpath) = self.wal_path
            && self.vfs.exists(wpath)?
            && let Ok(mut wfile) = self.vfs.open(Some(wpath), OpenFlags::READONLY)
        {
            let sz_res = wfile.file_size();
            let _ = wfile.close();
            if sz_res? > 0 {
                return Err(SqliteError::new(
                    ErrorCode::CantOpen,
                    "WAL mode databases are not supported; checkpoint in SQLite with PRAGMA journal_mode=DELETE first",
                ));
            }
        }
        Ok(())
    }

    /// Starts a read transaction (`SHARED` lock + WAL check + hot-journal recovery + authoritative
    /// 100-byte header validation). On any error, releases `.db` back to `NoLock` without publishing
    /// an unvalidated header.
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

        self.lock_db_file(LockLevel::Shared)?;
        if let Err(e) = self.begin_read_under_shared_lock() {
            let _ = self.unlock_db_file(LockLevel::NoLock);
            return Err(e);
        }
        self.state = TransactionState::Read;
        Ok(())
    }

    fn begin_read_under_shared_lock(&mut self) -> Result<()> {
        self.reject_if_wal_present()?;
        if self.is_memdb_backing
            || (self.db_kind == DatabaseKind::PrivateTemporary && self.file.is_none())
        {
            self.header_loaded = true;
            return Ok(());
        }

        let recovered = self.recover_hot_journal_under_shared_lock()?;
        let file_len = self.db_file()?.file_size()?;
        if file_len == 0 {
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
        Ok(())
    }

    /// Starts or upgrades to a write transaction (`RESERVED` lock).
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
        if acquired_read_here {
            let _ = self.unlock_db_file(LockLevel::NoLock);
            self.state = TransactionState::Open;
        } else {
            let _ = self.unlock_db_file(LockLevel::Shared);
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
            if target_lock >= LockLevel::Exclusive && !self.is_memdb_backing {
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

        // Reject writing to auto-vacuum (`FULL` / `INCREMENTAL`) pointer-map databases until Faz 1.3.
        if self.page_count > 0 && self.header.largest_root_btree_page != 0 {
            self.abort_write_upgrade(acquired_read_here);
            return Err(SqliteError::new(
                ErrorCode::Error,
                "writing to auto_vacuum (pointer-map) databases is not supported",
            ));
        }

        let lock_res = if target_lock >= LockLevel::Exclusive {
            self.lock_db_file(LockLevel::Reserved)
                .and_then(|()| self.lock_db_file(LockLevel::Exclusive))
        } else {
            self.lock_db_file(LockLevel::Reserved)
        };
        if let Err(e) = lock_res {
            self.abort_write_upgrade(acquired_read_here);
            return Err(e);
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
            let _ = self.finalize_journal_after_tx(false);
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

        match self.journal_mode {
            JournalMode::Off | JournalMode::Memory => {
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
            let offset = u64::from(pgno - 1) * (ps as u64);
            self.db_file()?.read_at(&mut buf, offset)?;
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
                if self.journal_file.is_some() {
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
            }
        }
        Ok(())
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

    /// Commits/finalizes any active write transaction while retaining `LockLevel::Shared`
    /// (`TransactionState::Read`) for remaining active readers (`Write -> Read`).
    pub fn downgrade_to_read(&mut self) -> Result<()> {
        self.ensure_not_closed()?;
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => {
                self.unlock_db_file(LockLevel::Shared)?;
                Ok(())
            }
            TransactionState::Write => self.commit_retaining_read(true),
        }
    }

    /// Releases `LockLevel::Shared` (`Read -> Open/NoLock`) when no active reader remains.
    /// Does not abort an active `Write` transaction.
    pub fn end_read_if_unused(&mut self) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state == TransactionState::Read {
            self.unlock_db_file(LockLevel::NoLock)?;
            self.state = TransactionState::Open;
        }
        Ok(())
    }

    /// Commits the active transaction. When `retain_read` is `true`, downgrades `.db` lock to
    /// `LockLevel::Shared` (`TransactionState::Read`); when `false`, unlocks to `LockLevel::NoLock`
    /// (`TransactionState::Open`).
    pub fn commit_retaining_read(&mut self, retain_read: bool) -> Result<()> {
        self.ensure_not_closed()?;
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => {
                if retain_read {
                    self.unlock_db_file(LockLevel::Shared)?;
                } else {
                    self.unlock_db_file(LockLevel::NoLock)?;
                    self.state = TransactionState::Open;
                }
                Ok(())
            }
            TransactionState::Write => {
                // Phase 1 (`sqlite3PagerCommitPhaseOne` / `PAGER_WRITER_FINISHED`, `pager.c:6740`):
                //Skipped on retry if Phase 1 already completed and only Phase 2 (`finalize_journal_after_tx`)
                // failed transiently, preventing re-writing `JOURNAL_MAGIC` over a truncated/zeroed journal!
                if !self.commit_phase_one_done {
                    let lock_byte_pgno = self.lock_byte_page();
                    let has_dirty_pages = self.cache.iter().any(|(&pgno, p)| {
                        p.dirty && pgno <= self.page_count && pgno != lock_byte_pgno
                    });
                    let needs_db_flush = self.cache_modified
                        || has_dirty_pages
                        || self.page_count != self.tx_orig_page_count;

                    if needs_db_flush {
                        // Bump file change counter & version_valid_for on Page 1 (at most once per tx
                        // so retrying `commit` after `SQLITE_BUSY` does not double-increment).
                        if self.page_count > 0 {
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

                        let effective_disk_pages = if self.page_count == lock_byte_pgno {
                            self.page_count - 1
                        } else {
                            self.page_count
                        };
                        let target_size = u64::from(effective_disk_pages) * ps;
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

                if retain_read {
                    self.unlock_db_file(LockLevel::Shared)?;
                    self.state = TransactionState::Read;
                } else {
                    self.unlock_db_file(LockLevel::NoLock)?;
                    self.state = TransactionState::Open;
                }
                Ok(())
            }
        }
    }

    /// Rolls back the active transaction, releasing locks back to `TransactionState::Open` (`NoLock`).
    pub fn rollback(&mut self) -> Result<()> {
        self.rollback_retaining_read(false)
    }

    /// Rolls back the active write transaction, restoring pre-images and original page count
    /// (or discarding dirty disk cache when `JournalMode::Off`), then downgrading `.db` lock to
    /// `LockLevel::Shared` (`retain_read == true`) or `LockLevel::NoLock` (`retain_read == false`).
    pub fn rollback_retaining_read(&mut self, retain_read: bool) -> Result<()> {
        self.ensure_not_closed()?;
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => {
                if retain_read {
                    self.unlock_db_file(LockLevel::Shared)?;
                } else {
                    self.unlock_db_file(LockLevel::NoLock)?;
                    self.state = TransactionState::Open;
                }
                Ok(())
            }
            TransactionState::Write => {
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
                self.subsavepoints.clear();

                // Finalize journal file (`Delete` unlinks even if `journal_file` was already closed
                // by a failed `commit()`, `Truncate` truncates to 0, `Persist` zeroes 28B) BEFORE
                // clearing `self.journal_started_in_tx`!
                self.finalize_journal_after_tx(false)?;
                self.journal_started_in_tx = false;

                if retain_read {
                    self.unlock_db_file(LockLevel::Shared)?;
                    self.state = TransactionState::Read;
                } else {
                    self.unlock_db_file(LockLevel::NoLock)?;
                    self.state = TransactionState::Open;
                }
                Ok(())
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
        self.tx_preimages.clear();
        self.tx_journaled.clear_all();
        self.subsavepoints.clear();
        self.finalize_journal_after_tx(false)?;
        self.journal_started_in_tx = false;

        if retain_read {
            self.unlock_db_file(LockLevel::Shared)?;
            if !self.header_loaded
                && let Err(e) = self.begin_read_under_shared_lock()
            {
                let _ = self.unlock_db_file(LockLevel::NoLock);
                self.state = TransactionState::Open;
                return Err(e);
            }
            self.state = TransactionState::Read;
        } else {
            self.unlock_db_file(LockLevel::NoLock)?;
            self.state = TransactionState::Open;
        }
        Ok(())
    }

    /// Finalizes `self.journal_file` at the end of commit or rollback (`pager_end_transaction`,
    /// `sqlite/src/pager.c:2117-2161` and `zeroJournalHdr`, `pager.c:1436-1468`).
    fn finalize_journal_after_tx(&mut self, is_commit: bool) -> Result<()> {
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
            JournalMode::Delete | JournalMode::Memory | JournalMode::Off => {
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
        let target = self.subsavepoints[index].clone();
        self.apply_subsavepoint_undo(&target);
        self.subsavepoints[index].saved_pages.clear();
        Ok(())
    }

    fn apply_subsavepoint_undo(&mut self, sp: &SubsavepointSlot) {
        self.commit_phase_one_done = false;
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

    /// Explicitly rolls back any active transaction, closes open journal and database file handles,
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
        hdr[24..28].copy_from_slice(&self.header.page_size.to_be_bytes());
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
