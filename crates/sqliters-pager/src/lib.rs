//! Rollback journal (`pager.c`), write-ahead log (`wal.c`), page cache (`pcache1.c`),
//! transactions, journal modes, and indexed anonymous physical sub-savepoints for SQLite 3.54.1.
#![forbid(unsafe_code)]

pub mod header;
pub mod super_journal;
pub mod wal;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use sqliters_types::{
    Bitvec, DatabaseKind, ErrorCode, LockGeometry, Pgno, Result, SQLITE_DEFAULT_PAGE_SIZE,
    SQLITE_VERSION_NUMBER, SqliteError, TextEncoding, decode_db_header_page_size, extended,
};
use sqliters_vfs::{
    LockLevel, MemoryVfs, OpenFlags, OsVfs, ShmOpenOptions, SyncFlags, Vfs, VfsFile,
};

pub use header::{AutoVacuumMode, DB_HEADER_SIZE, DatabaseHeader};
pub use super_journal::{
    SUPER_JOURNAL_COLLISION_RETRY_LIMIT, SUPER_JOURNAL_MAX_PATH_BYTES, apply_8_3_suffix,
    commit_pagers_two_phase, create_super_journal_file, format_super_journal_name,
    is_super_journal_name, journal_hdr_offset, read_super_journal, super_journal_checksum,
};
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

const MAX_DENSE_PGNO: Pgno = 1_048_576;

#[derive(Default, Clone, Copy)]
struct MulXorHasher(u64);

impl std::hash::Hasher for MulXorHasher {
    #[inline(always)]
    fn finish(&self) -> u64 {
        let h = self.0;
        h ^ (h >> 32)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        self.0 = h;
    }

    #[inline(always)]
    fn write_u32(&mut self, i: u32) {
        self.0 = u64::from(i).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    #[inline(always)]
    fn write_u64(&mut self, i: u64) {
        self.0 = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type MulXorBuildHasher = std::hash::BuildHasherDefault<MulXorHasher>;
type FastPgnoMap<V> = HashMap<Pgno, V, MulXorBuildHasher>;

#[derive(Debug, Clone)]
struct SubsavepointSlot {
    orig_page_count: Pgno,
    orig_header: DatabaseHeader,
    /// Pre-images of pages (`<= orig_page_count`) modified or spilled since this subsavepoint opened.
    saved_pages: FastPgnoMap<Vec<u8>>,
    /// 4-word WAL savepoint token (`[mx_frame, cksum0, cksum1, n_ckpt]`) when `use_wal` is active.
    wal_savepoint: Option<[u32; WAL_SAVEPOINT_NDATA]>,
}

struct CachedPage {
    pgno: Pgno,
    data: Vec<u8>,
    dirty: bool,
    in_dirty_list: bool,
    occupied: bool,
}

const EMPTY_CACHE_SLOT: u32 = u32::MAX;

struct PageCache {
    dense_slots: Vec<u32>,
    sparse_slots: FastPgnoMap<u32>,
    entries: Vec<CachedPage>,
    free_slots: Vec<u32>,
    dirty_slots: Vec<u32>,
    max_cached_pgno: Pgno,
}

impl PageCache {
    fn new() -> Self {
        Self {
            dense_slots: Vec::new(),
            sparse_slots: FastPgnoMap::default(),
            entries: Vec::new(),
            free_slots: Vec::new(),
            dirty_slots: Vec::new(),
            max_cached_pgno: 0,
        }
    }

    fn with_capacity(cap: usize) -> Self {
        let dense_cap = cap.saturating_add(1).min(MAX_DENSE_PGNO as usize + 1);
        Self {
            dense_slots: Vec::with_capacity(dense_cap),
            sparse_slots: FastPgnoMap::default(),
            entries: Vec::with_capacity(cap),
            free_slots: Vec::new(),
            dirty_slots: Vec::with_capacity(cap.min(256)),
            max_cached_pgno: 0,
        }
    }

    #[inline(always)]
    fn slot_of(&self, pgno: Pgno) -> Option<u32> {
        if pgno <= MAX_DENSE_PGNO {
            let s = *self.dense_slots.get(pgno as usize)?;
            if s != EMPTY_CACHE_SLOT { Some(s) } else { None }
        } else {
            self.sparse_slots.get(&pgno).copied()
        }
    }

    #[inline(always)]
    fn entry_data(&self, slot: u32) -> &[u8] {
        self.entries[slot as usize].data.as_slice()
    }

    #[inline(always)]
    fn entry_data_mut(&mut self, slot: u32) -> &mut [u8] {
        self.entries[slot as usize].data.as_mut_slice()
    }

    #[inline(always)]
    fn is_dirty(&self, pgno: Pgno) -> bool {
        self.slot_of(pgno)
            .is_some_and(|slot| self.entries[slot as usize].dirty)
    }

    /// Drops the dirty mark of `pgno` (the page stays cached with its current content).
    /// Returns whether the page was dirty.
    #[inline]
    fn clear_dirty(&mut self, pgno: Pgno) -> bool {
        match self.slot_of(pgno) {
            Some(slot) => std::mem::replace(&mut self.entries[slot as usize].dirty, false),
            None => false,
        }
    }

    #[inline(always)]
    fn get_data(&self, pgno: Pgno) -> Option<&[u8]> {
        let slot = self.slot_of(pgno)?;
        Some(self.entries[slot as usize].data.as_slice())
    }

    #[inline(always)]
    fn get_data_mut(&mut self, pgno: Pgno) -> Option<&mut [u8]> {
        let slot = self.slot_of(pgno)?;
        Some(self.entries[slot as usize].data.as_mut_slice())
    }

    #[inline(always)]
    fn mark_slot_dirty(&mut self, slot: u32) {
        let entry = &mut self.entries[slot as usize];
        entry.dirty = true;
        if !entry.in_dirty_list {
            entry.in_dirty_list = true;
            self.dirty_slots.push(slot);
        }
    }

    #[inline]
    fn get_mut_and_mark_dirty(&mut self, pgno: Pgno) -> Option<&mut [u8]> {
        let slot = self.slot_of(pgno)?;
        self.mark_slot_dirty(slot);
        Some(self.entries[slot as usize].data.as_mut_slice())
    }

    fn insert(&mut self, pgno: Pgno, data: Vec<u8>, dirty: bool) -> u32 {
        if let Some(slot) = self.slot_of(pgno) {
            let entry = &mut self.entries[slot as usize];
            entry.data = data;
            entry.dirty = dirty;
            if dirty && !entry.in_dirty_list {
                entry.in_dirty_list = true;
                self.dirty_slots.push(slot);
            }
            return slot;
        }

        let slot = if let Some(slot) = self.free_slots.pop() {
            let entry = &mut self.entries[slot as usize];
            let already_listed = entry.in_dirty_list;
            entry.pgno = pgno;
            entry.data = data;
            entry.dirty = dirty;
            entry.occupied = true;
            if dirty && !already_listed {
                entry.in_dirty_list = true;
                self.dirty_slots.push(slot);
            }
            slot
        } else {
            let slot = self.entries.len() as u32;
            self.entries.push(CachedPage {
                pgno,
                data,
                dirty,
                in_dirty_list: dirty,
                occupied: true,
            });
            if dirty {
                self.dirty_slots.push(slot);
            }
            slot
        };

        if pgno <= MAX_DENSE_PGNO {
            let idx = pgno as usize;
            if idx >= self.dense_slots.len() {
                self.dense_slots.resize(idx + 1, EMPTY_CACHE_SLOT);
            }
            self.dense_slots[idx] = slot;
        } else {
            self.sparse_slots.insert(pgno, slot);
        }
        if pgno > self.max_cached_pgno {
            self.max_cached_pgno = pgno;
        }
        slot
    }

    fn remove(&mut self, pgno: Pgno) {
        let removed = if pgno <= MAX_DENSE_PGNO {
            self.dense_slots.get_mut(pgno as usize).and_then(|cell| {
                let prev = std::mem::replace(cell, EMPTY_CACHE_SLOT);
                (prev != EMPTY_CACHE_SLOT).then_some(prev)
            })
        } else {
            self.sparse_slots.remove(&pgno)
        };
        if let Some(slot) = removed {
            let entry = &mut self.entries[slot as usize];
            entry.occupied = false;
            entry.dirty = false;
            entry.data = Vec::new();
            self.free_slots.push(slot);
        }
    }

    fn clear(&mut self) {
        self.dense_slots.clear();
        self.sparse_slots.clear();
        self.entries.clear();
        self.free_slots.clear();
        self.dirty_slots.clear();
        self.max_cached_pgno = 0;
    }

    fn truncate_above(&mut self, max_pgno: Pgno) {
        if self.max_cached_pgno <= max_pgno {
            return;
        }
        let keep_len = (max_pgno.min(MAX_DENSE_PGNO) as usize) + 1;
        if keep_len < self.dense_slots.len() {
            for slot_cell in &mut self.dense_slots[keep_len..] {
                let slot = std::mem::replace(slot_cell, EMPTY_CACHE_SLOT);
                if slot != EMPTY_CACHE_SLOT {
                    let entry = &mut self.entries[slot as usize];
                    entry.occupied = false;
                    entry.dirty = false;
                    entry.data = Vec::new();
                    self.free_slots.push(slot);
                }
            }
            self.dense_slots.truncate(keep_len);
        }
        if !self.sparse_slots.is_empty() {
            let entries = &mut self.entries;
            let free_slots = &mut self.free_slots;
            self.sparse_slots.retain(|&pgno, &mut slot| {
                if pgno > max_pgno {
                    let entry = &mut entries[slot as usize];
                    entry.occupied = false;
                    entry.dirty = false;
                    entry.data = Vec::new();
                    free_slots.push(slot);
                    false
                } else {
                    true
                }
            });
        }
        self.max_cached_pgno = max_pgno;
    }

    fn retain_dirty_only(&mut self) {
        for (idx, entry) in self.entries.iter_mut().enumerate() {
            if entry.occupied && !entry.dirty {
                let pgno = entry.pgno;
                if pgno <= MAX_DENSE_PGNO {
                    if let Some(slot_cell) = self.dense_slots.get_mut(pgno as usize) {
                        *slot_cell = EMPTY_CACHE_SLOT;
                    }
                } else {
                    self.sparse_slots.remove(&pgno);
                }
                entry.occupied = false;
                entry.data = Vec::new();
                self.free_slots.push(idx as u32);
            }
        }
    }

    #[inline]
    fn any_dirty(&self) -> bool {
        self.dirty_slots.iter().any(|&slot| {
            let e = &self.entries[slot as usize];
            e.occupied && e.dirty
        })
    }

    #[inline]
    fn has_valid_dirty_pages(&self, page_count: Pgno, lock_byte_pgno: Pgno) -> bool {
        self.dirty_slots.iter().any(|&slot| {
            let e = &self.entries[slot as usize];
            e.occupied && e.dirty && e.pgno <= page_count && e.pgno != lock_byte_pgno
        })
    }

    #[inline]
    fn count_valid_dirty_pages(&self, page_count: Pgno, lock_byte_pgno: Pgno) -> usize {
        self.dirty_slots
            .iter()
            .filter(|&&slot| {
                let e = &self.entries[slot as usize];
                e.occupied && e.dirty && e.pgno <= page_count && e.pgno != lock_byte_pgno
            })
            .count()
    }

    fn collect_valid_dirty_pgnos_sorted(
        &self,
        page_count: Pgno,
        lock_byte_pgno: Pgno,
    ) -> Vec<Pgno> {
        let mut pgnos = Vec::with_capacity(self.dirty_slots.len());
        for &slot in &self.dirty_slots {
            let e = &self.entries[slot as usize];
            if e.occupied && e.dirty && e.pgno <= page_count && e.pgno != lock_byte_pgno {
                pgnos.push(e.pgno);
            }
        }
        pgnos.sort_unstable();
        pgnos
    }

    #[inline]
    fn clear_all_dirty(&mut self) {
        for slot in self.dirty_slots.drain(..) {
            let e = &mut self.entries[slot as usize];
            e.dirty = false;
            e.in_dirty_list = false;
        }
    }
}

/// Validation hook for `PRAGMA cell_size_check` (see [`Pager::set_page_load_check`]): returns
/// `false` when page `pgno` (`raw`, `usable_size` usable bytes) is a malformed b-tree page.
pub type PageLoadCheck = fn(pgno: Pgno, usable_size: u32, raw: &[u8]) -> bool;

#[derive(Debug, Clone)]
struct PendingByteCopy {
    raw_byte_size: u64,
    post_pending_tail: Option<(u64, Vec<u8>)>,
    new_header: DatabaseHeader,
    new_page_count: Pgno,
}

/// Leaf dirty-page and restart tracker attached to a source [`Pager`] while an online backup is
/// active (`sqlite3_backup` in `sqlite/src/backup.c`).
#[derive(Debug, Default)]
pub struct BackupTracker {
    /// Page numbers modified on the source connection since the last backup step.
    pub dirty_pages: BTreeSet<Pgno>,
    /// Set where C calls `sqlite3BackupRestart()`: the source pager was reset (an external
    /// modification was detected, the page size changed, the database was deserialized), a WAL
    /// transaction was rolled back, or an in-memory source committed. The backup starts again
    /// at page 1.
    pub needs_restart: bool,
    /// Set where C feeds every rewritten page to `sqlite3BackupUpdate()` (a VACUUM or a backup
    /// *into* the source, a truncation): every page the backup has already copied has to be
    /// copied again, but the backup keeps its position (`iNext`).
    pub all_dirty: bool,
    /// Monotonic counter incremented on every tracker event.
    pub generation: u64,
    /// Last `change_counter` committed by this connection itself.
    pub last_self_cc: Option<u32>,
    /// Last WAL `(salt0, salt1, mx_frame)` signature committed by this connection itself.
    pub last_self_wal: Option<(u32, u32, u32)>,
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
    /// `Pager.iDataVersion` (`pager.c:669`), the value behind `PRAGMA data_version`: starts at 1
    /// (C resets the cache once while opening) and is incremented wherever C calls
    /// `pager_reset()` — the database was changed by another connection, or the page size of
    /// the cache changed. Commits of this connection leave it alone (C adds one in
    /// `sqlite3PagerCommitPhaseTwo` and takes it back in `sqlite3BtreeCommitPhaseTwo`).
    data_version: u32,
    /// Encoding chosen with `PRAGMA encoding` (or inherited from the connection) for a database
    /// whose header does not record one yet. The header is re-read from disk at the start of
    /// every transaction, so the choice is kept here and re-applied until the first table
    /// writes it into the file.
    pending_encoding: Option<TextEncoding>,
    /// This connection committed page 1 into the WAL since it last loaded it. C stamps such a
    /// page with `dbFileVers + 1` without updating `Pager.dbFileVers`, so the first shared lock
    /// taken after it leaves WAL mode sees a "changed" file and resets the cache once.
    dv_wal_page1_written: bool,
    /// `PRAGMA cell_size_check`: validation run on a page when it is loaded from disk to be
    /// read as a b-tree page (`btreeInitPage` → `btreeCellSizeCheck`).
    page_load_check: Option<PageLoadCheck>,
    page_count: Pgno,
    configured_auto_vacuum: AutoVacuumMode,
    writable_schema: bool,
    next_page_size: Option<u32>,
    pending_byte_copy: Option<PendingByteCopy>,
    staged_raw_copy: bool,

    cache: PageCache,
    state: TransactionState,

    /// Transaction-level undo state for atomic rollback
    tx_orig_page_count: Pgno,
    tx_orig_header: DatabaseHeader,
    tx_orig_auto_vacuum: AutoVacuumMode,
    tx_journaled: Bitvec,
    tx_preimages: FastPgnoMap<Vec<u8>>,
    journal_records_written: u32,
    journal_off: u64,
    journal_nonce: u32,
    journal_sector_size: u32,
    journal_started_in_tx: bool,
    cache_modified: bool,
    change_counter_bumped: bool,
    /// `Pager.dbFileVers` (`pager.c`), change-counter part: the counter of the page 1 image this
    /// pager last loaded from storage (database file or WAL) or last wrote to the database file
    /// itself. A commit stamps `db_file_vers_cc + 1` (`pager_write_changecounter`); a WAL commit
    /// does not advance it, so the stamp stays constant while page 1 stays cached.
    db_file_vers_cc: u32,
    /// True once `commit` has started writing/truncating the main database file (`PAGER_WRITER_DBMOD`).
    db_file_modified: bool,
    /// True once `write_super_journal` has recorded a super-journal pointer in this tx (`pPager->setSuper`).
    set_super: bool,
    /// True once Phase 1 of `commit` (`.db` page flush + truncate + sync) has finished (`PAGER_WRITER_FINISHED`).
    commit_phase_one_done: bool,

    subsavepoints: Vec<SubsavepointSlot>,
    /// The first slot of `subsavepoints` stands for C's "transaction savepoint" (a `SAVEPOINT`
    /// run in autocommit mode: `db->isTransactionSavepoint`). C does not count it in
    /// `db->nSavepoint`, so its pager has no savepoint open for it (`Pager.nSavepoint`).
    transaction_savepoint: bool,
    sp_slot_pool: Vec<SubsavepointSlot>,
    sp_page_buf_pool: Vec<Vec<u8>>,
    backup_trackers: Vec<Arc<Mutex<BackupTracker>>>,
    backup_raw_staging: Option<BTreeMap<Pgno, Vec<u8>>>,
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
            data_version: 1,
            pending_encoding: None,
            dv_wal_page1_written: false,
            page_load_check: None,
            page_count: 0,
            configured_auto_vacuum: AutoVacuumMode::None,
            writable_schema: false,
            next_page_size: None,
            pending_byte_copy: None,
            staged_raw_copy: false,
            cache: PageCache::new(),
            state: TransactionState::Open,
            tx_orig_page_count: 0,
            tx_orig_header: DatabaseHeader::default(),
            tx_orig_auto_vacuum: AutoVacuumMode::None,
            tx_journaled: Bitvec::new(64),
            tx_preimages: FastPgnoMap::default(),
            journal_records_written: 0,
            journal_off: 0,
            journal_nonce: 0,
            journal_sector_size: MIN_JOURNAL_HEADER_BYTES as u32,
            journal_started_in_tx: false,
            cache_modified: false,
            change_counter_bumped: false,
            db_file_vers_cc: 0,
            db_file_modified: false,
            set_super: false,
            commit_phase_one_done: false,
            subsavepoints: Vec::new(),
            transaction_savepoint: false,
            sp_slot_pool: Vec::new(),
            sp_page_buf_pool: Vec::new(),
            backup_trackers: Vec::new(),
            backup_raw_staging: None,
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

    /// Acquires `LockLevel::Exclusive` on the database file when in an active rollback-mode write
    /// transaction (`sqlite3PagerExclusiveLock` in `sqlite/src/pager.c:6485-6498`).
    pub fn acquire_exclusive_lock(&mut self) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state == TransactionState::Write && !self.use_wal() && !self.is_memdb_backing {
            self.lock_db_file(LockLevel::Exclusive)?;
        }
        Ok(())
    }

    /// Returns `true` when this pager is an active durable rollback-journal write participant that
    /// increments `nTrans` in `vdbeCommit` (`sqlite/src/vdbeaux.c:2969-2987`).
    #[inline]
    pub const fn needs_super_journal_in_write_tx(&self) -> bool {
        matches!(self.state, TransactionState::Write)
            && self.synchronous > 0
            && !self.use_wal()
            && matches!(
                self.journal_mode,
                JournalMode::Delete | JournalMode::Persist | JournalMode::Truncate
            )
            && !self.is_pager_memdb()
    }

    /// Returns `true` when a raw [`Self::copy_pages_from`] image is staged in the active write tx.
    #[inline]
    pub const fn has_staged_raw_copy(&self) -> bool {
        self.staged_raw_copy
    }

    /// Returns the pre-transaction page size when in a write transaction, or the current page size.
    #[inline]
    pub const fn tx_orig_page_size(&self) -> u32 {
        if matches!(self.state, TransactionState::Write) && self.tx_orig_header.page_size > 0 {
            self.tx_orig_header.page_size
        } else {
            self.header.page_size
        }
    }

    /// Returns the current WAL snapshot signature `(salt0, salt1, mx_frame)` when in WAL mode.
    #[inline]
    pub fn wal_snapshot_signature(&self) -> Option<(u32, u32, u32)> {
        self.wal.as_ref().map(|wal| {
            let hdr = wal.header();
            (hdr.salt[0], hdr.salt[1], hdr.mx_frame)
        })
    }

    /// Attaches a leaf [`BackupTracker`] to receive dirty-page and restart notifications.
    pub fn attach_backup_tracker(&mut self, tracker: Arc<Mutex<BackupTracker>>) {
        if !self
            .backup_trackers
            .iter()
            .any(|existing| Arc::ptr_eq(existing, &tracker))
        {
            self.backup_trackers.push(tracker);
        }
    }

    /// Detaches a previously registered [`BackupTracker`].
    pub fn detach_backup_tracker(&mut self, tracker: &Arc<Mutex<BackupTracker>>) {
        self.backup_trackers
            .retain(|existing| !Arc::ptr_eq(existing, tracker));
    }

    #[inline]
    fn notify_backup_page_write(&self, pgno: Pgno) {
        if self.backup_trackers.is_empty() {
            return;
        }
        for tracker in &self.backup_trackers {
            let mut guard = tracker.lock().unwrap_or_else(|e| e.into_inner());
            guard.dirty_pages.insert(pgno);
            guard.generation = guard.generation.wrapping_add(1);
        }
    }

    #[inline]
    fn notify_backup_restart(&self) {
        if self.backup_trackers.is_empty() {
            return;
        }
        for tracker in &self.backup_trackers {
            let mut guard = tracker.lock().unwrap_or_else(|e| e.into_inner());
            guard.needs_restart = true;
            guard.dirty_pages.clear();
            guard.generation = guard.generation.wrapping_add(1);
        }
    }

    /// Every page may have changed without the page-level tracking seeing it (bulk rewrite):
    /// the backups re-copy what they have copied so far, at their current position.
    #[inline]
    fn notify_backup_all_dirty(&self) {
        if self.backup_trackers.is_empty() {
            return;
        }
        for tracker in &self.backup_trackers {
            let mut guard = tracker.lock().unwrap_or_else(|e| e.into_inner());
            guard.all_dirty = true;
            guard.generation = guard.generation.wrapping_add(1);
        }
    }

    #[inline]
    fn notify_backup_self_commit(&self) {
        if self.backup_trackers.is_empty() {
            return;
        }
        let cc = self.header.change_counter;
        let wal_sig = self.wal_snapshot_signature();
        for tracker in &self.backup_trackers {
            let mut guard = tracker.lock().unwrap_or_else(|e| e.into_inner());
            guard.last_self_cc = Some(cc);
            guard.last_self_wal = wal_sig;
            guard.generation = guard.generation.wrapping_add(1);
        }
    }

    /// Returns `true` once Page 1 has been initialized or loaded from disk (`BTS_PAGESIZE_FIXED`).
    #[inline]
    pub const fn is_page_size_fixed(&self) -> bool {
        self.page_size_fixed
    }

    /// Returns the active [`AutoVacuumMode`] (`sqlite3BtreeGetAutoVacuum`, `btree.c:3258-3270`).
    #[inline]
    pub const fn auto_vacuum_mode(&self) -> AutoVacuumMode {
        self.configured_auto_vacuum
    }

    /// Updates the configured [`AutoVacuumMode`] following `sqlite3BtreeSetAutoVacuum`
    /// (`sqlite/src/btree.c:3232-3256`).
    ///
    /// Returns `false` (`SQLITE_READONLY`) without modifying the current mode if Page 1 is already
    /// fixed and `mode != AutoVacuumMode::None` differs from `current != AutoVacuumMode::None`.
    /// Toggling `Full <-> Incremental` on an existing auto-vacuum database is allowed and returns
    /// `true`.
    pub fn set_configured_auto_vacuum(&mut self, mode: AutoVacuumMode) -> bool {
        let new_av = mode != AutoVacuumMode::None;
        let cur_av = self.configured_auto_vacuum != AutoVacuumMode::None;
        if self.page_size_fixed && new_av != cur_av {
            return false;
        }
        self.configured_auto_vacuum = mode;
        if self.page_count == 0 && !self.page_size_fixed {
            self.header.largest_root_btree_page = if mode == AutoVacuumMode::None { 0 } else { 1 };
            self.header.incremental_vacuum_mode = u32::from(mode == AutoVacuumMode::Incremental);
        }
        true
    }

    /// Enables or disables `PRAGMA writable_schema` truncated-image tolerance (`btree.c:3435-3442`).
    #[inline]
    pub fn set_writable_schema(&mut self, enabled: bool) {
        self.writable_schema = enabled;
    }

    /// Returns `true` when `PRAGMA writable_schema` is enabled on this pager.
    #[inline]
    pub const fn writable_schema(&self) -> bool {
        self.writable_schema
    }

    /// Compatibility no-op setter retained for pointer-map unit tests; auto-vacuum writes are
    /// unconditionally enabled in Phase 1.3.
    #[inline]
    pub fn set_autovacuum_ptrmap_ready(&mut self, _ready: bool) {}

    /// Returns `true` (auto-vacuum pointer-map writes are unconditionally enabled).
    #[inline]
    pub const fn is_autovacuum_ptrmap_ready(&self) -> bool {
        true
    }

    /// Replaces this pager's backing storage with an isolated in-memory database image initialized
    /// from `data` (`sqlite3_deserialize`, `sqlite/src/memdb.c`).
    ///
    /// Requires `TransactionState::Open`. Page 1 header parsing is deferred until the next read or
    /// write transaction so `PRAGMA writable_schema = ON` can be set before reading a fuzz image.
    pub fn load_deserialized_image(&mut self, data: &[u8], readonly: bool) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Open {
            return Err(SqliteError::new(
                ErrorCode::Locked,
                "cannot deserialize while a transaction is active",
            ));
        }

        if let Some(wal) = self.wal.take()
            && let Some(ref df) = self.file
        {
            // C closes the old b-tree the ordinary way (`attach.c` `REOPEN_AS_MEMDB` →
            // `sqlite3BtreeClose` → `sqlite3WalClose` with the pager's sync flags): the
            // close-time checkpoint syncs the log, copies the frames, syncs the database and
            // only then removes the log. Its result is not looked at there either.
            let ckpt_sync = self.ckpt_sync_flags();
            let _ = wal.close(&**df, ckpt_sync, self.persist_wal);
        }
        if let Some(mut jf) = self.journal_file.take() {
            let _ = jf.close();
        }
        if let Some(mut df) = self.file.take() {
            let _ = df.unlock(LockLevel::NoLock);
            let _ = df.close();
        }

        let mem_vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::with_lock_geometry(self.lock_geometry));
        let mem_file = mem_vfs.open(
            Some(Path::new("deserialized.db")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
        )?;
        if !data.is_empty() {
            mem_file.write_at(data, 0)?;
        }

        self.vfs = mem_vfs;
        self.db_path = None;
        self.journal_path = None;
        self.wal_path = None;
        self.db_kind = DatabaseKind::Memory;
        self.is_memdb_backing = true;
        self.readonly = readonly;
        self.journal_mode = JournalMode::Memory;
        self.file = Some(mem_file);
        self.cache.clear();
        self.header = DatabaseHeader::new_with_page_size(self.header.page_size);
        self.header_loaded = false;
        self.page_size_fixed = false;
        self.page_count = 0;
        self.configured_auto_vacuum = AutoVacuumMode::None;
        self.next_page_size = None;
        self.pending_byte_copy = None;
        self.staged_raw_copy = false;
        self.backup_raw_staging = None;
        self.set_super = false;
        self.notify_backup_restart();
        // A deserialized image is a new database (C opens a new b-tree for it).
        self.pending_encoding = None;
        self.data_version = 1;
        self.dv_wal_page1_written = false;
        Ok(())
    }

    /// `sqlite3PagerDataVersion` (`pager.c:1830`): the counter behind `PRAGMA data_version`.
    #[inline]
    pub const fn data_version(&self) -> u32 {
        self.data_version
    }

    /// The cache content was (or must be assumed) replaced by something this connection did not
    /// write itself — the `pPager->iDataVersion++` of `pager_reset()`.
    #[inline]
    fn bump_data_version(&mut self) {
        self.data_version = self.data_version.wrapping_add(1);
    }

    /// Installs (or removes) the validation that [`Self::read_page`] runs on a page it has to
    /// load from disk (`PRAGMA cell_size_check`). Pages that are already cached count as
    /// validated, exactly like a `MemPage` whose `isInit` is set in C.
    pub fn set_page_load_check(&mut self, check: Option<PageLoadCheck>) {
        self.page_load_check = check;
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
    #[inline]
    pub fn spill_if_over_cache_size(&mut self) -> Result<usize> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write || !self.use_wal() {
            return Ok(0);
        }
        let max_pages = self.effective_max_cache_pages();
        if self.cache.dirty_slots.len() <= max_pages {
            return Ok(0);
        }
        let lock_byte_pg = self.lock_byte_page();
        let dirty_count = self
            .cache
            .count_valid_dirty_pages(self.page_count, lock_byte_pg);
        if dirty_count > max_pages {
            self.spill_dirty_pages_to_wal()
        } else {
            Ok(0)
        }
    }

    /// Returns `true` when it is safe to change `journal_mode` (`sqlite3PagerOkToChangeJournalMode`,
    /// `pager.c:7536`): false if in a write transaction with dirty pages or `journal_off > 0`.
    pub fn ok_to_change_journal_mode(&self) -> bool {
        if self.state == TransactionState::Write
            && (self.cache_modified || self.journal_off > 0 || self.cache.any_dirty())
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
                if let Some(p1) = self.cache.get_mut_and_mark_dirty(1) {
                    hdr_copy.write_into(&mut p1[0..DB_HEADER_SIZE]);
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
            if std::mem::take(&mut self.dv_wal_page1_written) {
                // `sqlite3PagerSharedLock` of the header rewrite below: the database file now
                // carries the page 1 this connection committed through the WAL, which is not
                // what `Pager.dbFileVers` remembers → `pager_reset()`.
                self.bump_data_version();
            }

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
                    if let Some(p1) = self.cache.get_mut_and_mark_dirty(1) {
                        hdr_copy.write_into(&mut p1[0..DB_HEADER_SIZE]);
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
        if self.page_count > 0 && self.cache.is_dirty(1) {
            self.header.write_version = 2;
            self.header.read_version = 2;
            self.header.database_size_pages = self.page_count;
            let hdr_copy = self.header.clone();
            if let Some(p1) = self.cache.get_data_mut(1) {
                hdr_copy.write_into(&mut p1[0..DB_HEADER_SIZE]);
            }
        }

        let dirty_pgnos = self
            .cache
            .collect_valid_dirty_pgnos_sorted(self.page_count, lock_byte_pgno);
        if dirty_pgnos.is_empty() {
            return Ok(0);
        }

        // Save pre-spill image into any open sub-savepoint slots (`subjournalPageIfRequired`)!
        for &pgno in &dirty_pgnos {
            let cur_img = self.cache.get_data(pgno).expect("dirty exists").to_vec();
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
            let page_data = self.cache.get_data(pgno).expect("dirty exists").to_vec();
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
            self.cache.remove(pgno);
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

    /// Sets the database text encoding (`PRAGMA encoding`, or the connection encoding handed
    /// to a newly attached / temporary database). As in C this is possible until the first
    /// table is created: the header of a table-less database carries no encoding (bytes
    /// 56..59 are 0) even when page 1 already exists because of `PRAGMA journal_mode=WAL`,
    /// `auto_vacuum`, `user_version`, an empty transaction or `VACUUM`. Once the file records
    /// an encoding the call is a no-op.
    pub fn set_encoding(&mut self, enc: TextEncoding) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state == TransactionState::Open && !self.header_loaded {
            let _ = self.ensure_header_loaded()?;
        }
        if !self.header.file_format_is_set() && self.header.text_encoding_unset {
            self.pending_encoding = Some(enc);
            self.header.text_encoding = enc as u32;
        }
        Ok(())
    }

    /// Installs a header freshly read from the file, keeping the encoding chosen for a
    /// database that does not record one yet.
    #[inline]
    fn adopt_loaded_header(&mut self, hdr: DatabaseHeader) {
        self.header = hdr;
        if self.header.text_encoding_unset
            && let Some(enc) = self.pending_encoding
        {
            self.header.text_encoding = enc as u32;
        }
    }

    #[inline]
    pub const fn transaction_state(&self) -> TransactionState {
        self.state
    }

    #[inline]
    pub const fn is_header_loaded(&self) -> bool {
        self.header_loaded
    }

    /// Marks the cached database header as unloaded so the next read/write or pragma
    /// re-reads Page 1 from disk (and attaches WAL if Page 1 now has `write_version = 2`).
    #[inline]
    pub fn invalidate_header_loaded(&mut self) {
        self.header_loaded = false;
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
            if self.header.page_size != new_size {
                self.notify_backup_restart();
                // `sqlite3PagerSetPagesize` → `pager_reset()`.
                self.bump_data_version();
            }
            self.header.page_size = new_size;
            self.cache.clear();
            self.next_page_size = Some(new_size);
        } else {
            self.next_page_size = Some(new_size);
        }
        Ok(())
    }

    /// Sets the reserved bytes per page before Page 1 is initialized (`sqlite3BtreeSetPageSize`,
    /// `sqlite/src/btree.c:3103-3134`).
    pub fn set_reserved_bytes(&mut self, reserved: u8) -> Result<()> {
        self.ensure_not_closed()?;
        if self.header.page_size.saturating_sub(u32::from(reserved)) < 480 {
            return Err(SqliteError::misuse(
                "usable page size must be at least 480 bytes",
            ));
        }
        if !self.page_size_fixed && self.page_count == 0 {
            self.header.reserved_bytes = reserved;
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
        // Only the no-WAL-yet failure path below consults the prior `.db` lock level, so skip
        // the query (a mutex round trip in the VFS) once a WAL is attached.
        let prev_lock = if had_wal_before {
            LockLevel::NoLock
        } else {
            self.file
                .as_ref()
                .map_or(LockLevel::NoLock, |f| f.lock_level())
        };

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
        if (self.is_memdb_backing && self.header_loaded)
            || (self.db_kind == DatabaseKind::PrivateTemporary && self.file.is_none())
        {
            self.header_loaded = true;
            return Ok(());
        }
        if self.is_memdb_backing {
            let mem_len = self.file.as_ref().map_or(0, |f| f.file_size().unwrap_or(0));
            if mem_len == 0 {
                self.header_loaded = true;
                return Ok(());
            }
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
            let recovered = if self.immutable || self.is_memdb_backing {
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

                    // Per btree.c:3330 & 3435-3442: if header page count is 0 or change_counter != version_valid_for
                    // (e.g. modified by pre-3.7.0 SQLite), fall back to physical file size in pages;
                    // if header page count exceeds physical file pages and `writable_schema` is enabled,
                    // clamp to `file_pages`.
                    let effective_pages = if disk_hdr.database_size_pages == 0
                        || disk_hdr.change_counter != disk_hdr.version_valid_for
                    {
                        file_pages.max(1)
                    } else if disk_hdr.database_size_pages > file_pages && file_pages > 0 {
                        if self.writable_schema {
                            file_pages
                        } else {
                            return Err(SqliteError::corrupt(
                                "database header page count exceeds file size",
                            ));
                        }
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
                        if self.header_loaded {
                            self.notify_backup_restart();
                            // `sqlite3PagerSharedLock`: the 16 bytes at offset 24 differ from
                            // what this pager last saw → `pager_reset()`.
                            self.bump_data_version();
                        }
                        self.cache.clear();
                    }
                    if disk_hdr.page_size != self.header.page_size {
                        // `lockBtree` → `sqlite3PagerSetPagesize` → `pager_reset()`.
                        self.bump_data_version();
                    }

                    self.configured_auto_vacuum = AutoVacuumMode::from_header(&disk_hdr);
                    // `sqlite3PagerSharedLock` reads bytes 24..39 of the file into `dbFileVers`.
                    self.db_file_vers_cc = disk_hdr.change_counter;
                    self.adopt_loaded_header(disk_hdr);
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
            if self.header_loaded && changed {
                self.notify_backup_restart();
            }
            self.cache.clear();
        }
        if changed {
            // Page 1 is reloaded from the new snapshot (`readDbPage` refreshes `dbFileVers`).
            self.dv_wal_page1_written = false;
            // `pagerBeginReadTransaction`: `sqlite3WalBeginReadTransaction` reported a wal-index
            // header other than the one this connection last used → `pager_reset()`. As in C
            // this includes the first snapshot taken through a newly opened WAL.
            self.bump_data_version();
        }

        let wal_pages = self.wal.as_ref().unwrap().db_size();
        if !changed
            && self.header_loaded
            && self.page_size_fixed
            && wal_pages > 0
            && self.page_count == wal_pages
        {
            return Ok(());
        }
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
                if self.writable_schema {
                    file_pages
                } else {
                    return Err(SqliteError::corrupt(
                        "database header page count exceeds file size",
                    ));
                }
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
            if self.header_loaded {
                self.notify_backup_restart();
            }
            self.cache.clear();
        }
        if snap_hdr.page_size != self.header.page_size {
            // `lockBtree` → `sqlite3PagerSetPagesize` → `pager_reset()`.
            self.bump_data_version();
        }

        self.configured_auto_vacuum = AutoVacuumMode::from_header(&snap_hdr);
        if invalidate_cache {
            // `readDbPage(1)`: page 1 is (re-)read from the WAL snapshot or the database file
            // only when it is not cached; a cached page 1 keeps the older `dbFileVers`.
            self.db_file_vers_cc = snap_hdr.change_counter;
        }
        self.adopt_loaded_header(snap_hdr);
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
        }

        self.tx_orig_page_count = self.page_count;
        self.tx_orig_header = self.header.clone();
        self.tx_orig_auto_vacuum = self.configured_auto_vacuum;
        self.tx_journaled = Bitvec::new(self.page_count.max(64));
        self.tx_preimages.clear();
        self.journal_records_written = 0;
        self.journal_off = 0;
        self.journal_sector_size = self.effective_sector_size();
        self.journal_started_in_tx = false;
        self.cache_modified = false;
        self.change_counter_bumped = false;
        self.db_file_modified = false;
        self.set_super = false;
        self.commit_phase_one_done = false;
        self.pending_byte_copy = None;
        self.staged_raw_copy = false;
        self.backup_raw_staging = None;

        self.state = TransactionState::Write;
        if savepoint_depth == 0 {
            // No SQL savepoint is open when this write transaction starts.
            self.transaction_savepoint = false;
        }

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
            self.cache.remove(1);
            self.page_count = 0;
            self.header = self.tx_orig_header.clone();
            self.configured_auto_vacuum = self.tx_orig_auto_vacuum;
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
        // `newDatabase` (`btree.c`): bytes 24..99 of a new page 1 are zero; the first commit
        // stamps change counter 1 (`pager_write_changecounter`: `dbFileVers` (0) + 1).
        self.header.change_counter = 0;
        self.header.version_valid_for = 0;
        self.db_file_vers_cc = 0;
        // `newDatabase` (`btree.c`): schema format and text encoding stay 0 in the file until
        // the first table is created.
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
        self.configured_auto_vacuum = effective_av;
        self.header_loaded = true;
        self.cache_modified = true;
        self.cache.insert(1, page1, true);
        self.notify_backup_page_write(1);
        Ok(())
    }

    #[inline]
    fn ensure_page_cached(&mut self, pgno: Pgno) -> Result<u32> {
        if let Some(slot) = self.cache.slot_of(pgno) {
            return Ok(slot);
        }
        self.load_page_cold(pgno)
    }

    #[cold]
    fn load_page_cold(&mut self, pgno: Pgno) -> Result<u32> {
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
        Ok(self.cache.insert(pgno, buf, false))
    }

    /// Reads page `pgno` (1-based) into the page cache if needed and returns an immutable slice.
    ///
    /// This is the accessor for b-tree pages: with `PRAGMA cell_size_check` on, a page that has
    /// to be loaded from disk is validated before it is handed out, and the read fails with
    /// `SQLITE_CORRUPT` if a cell does not fit the page. The check lives on the load path only,
    /// so a cached page costs nothing extra. Pages that are not b-tree pages (pointer map,
    /// overflow, freelist) and raw page copies go through [`Self::read_page_unchecked`].
    #[inline]
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
        let slot = self.ensure_btree_page_cached(pgno)?;
        Ok(self.cache.entry_data(slot))
    }

    /// [`Self::read_page`] without the `PRAGMA cell_size_check` validation.
    #[inline]
    pub fn read_page_unchecked(&mut self, pgno: Pgno) -> Result<&[u8]> {
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
        let slot = self.ensure_page_cached(pgno)?;
        Ok(self.cache.entry_data(slot))
    }

    #[inline]
    fn ensure_btree_page_cached(&mut self, pgno: Pgno) -> Result<u32> {
        if let Some(slot) = self.cache.slot_of(pgno) {
            return Ok(slot);
        }
        self.load_btree_page_cold(pgno)
    }

    /// Loads a page that is about to be used as a b-tree page: `btreeInitPage` with
    /// `SQLITE_CellSizeCk`. A page that fails is not kept, so every later read loads and
    /// rejects it again (C leaves `MemPage.isInit` clear).
    #[cold]
    fn load_btree_page_cold(&mut self, pgno: Pgno) -> Result<u32> {
        let slot = self.load_page_cold(pgno)?;
        if let Some(check) = self.page_load_check
            && !check(pgno, self.header.usable_size(), self.cache.entry_data(slot))
        {
            self.cache.remove(pgno);
            // `SQLITE_CORRUPT_PAGE()` out of `btreeCellSizeCheck`.
            return Err(SqliteError::corrupt("database disk image is malformed"));
        }
        Ok(slot)
    }

    /// Journals the pre-image of `pgno` (if not yet journaled in this tx/subsavepoints) and marks it dirty.
    fn mark_page_writable(&mut self, pgno: Pgno) -> Result<u32> {
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
        let slot = self.ensure_page_cached(pgno)?;
        self.ensure_journal_started()?;

        let need_tx_preimage = self.journal_mode != JournalMode::Off
            && pgno <= self.tx_orig_page_count
            && !self.tx_journaled.test(pgno);
        let need_sp_preimage = self
            .subsavepoints
            .iter()
            .any(|sp| pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno));

        if need_tx_preimage || need_sp_preimage {
            let src = self.cache.entry_data(slot);
            if need_tx_preimage && !need_sp_preimage {
                let mut orig = self.sp_page_buf_pool.pop().unwrap_or_default();
                orig.clear();
                orig.extend_from_slice(src);
                if !self.use_wal() && self.journal_file.is_some() {
                    self.append_journal_page(pgno, &orig)?;
                }
                self.tx_journaled.grow_to(self.tx_orig_page_count.max(pgno));
                self.tx_journaled.set(pgno)?;
                self.tx_preimages.insert(pgno, orig);
            } else if !need_tx_preimage && self.subsavepoints.len() == 1 {
                let sp = &mut self.subsavepoints[0];
                if pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno) {
                    let mut orig = self.sp_page_buf_pool.pop().unwrap_or_default();
                    orig.clear();
                    orig.extend_from_slice(src);
                    sp.saved_pages.insert(pgno, orig);
                }
            } else {
                let mut orig = self.sp_page_buf_pool.pop().unwrap_or_default();
                orig.clear();
                orig.extend_from_slice(src);

                // Always record pre-images for active sub-savepoint slots (even in `JournalMode::Off`
                // so `SAVEPOINT` / `ROLLBACK TO` works in cache per `savepoint-13.4`!).
                for sp in &mut self.subsavepoints {
                    if pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno) {
                        let mut copy = self.sp_page_buf_pool.pop().unwrap_or_default();
                        copy.clear();
                        copy.extend_from_slice(&orig);
                        sp.saved_pages.insert(pgno, copy);
                    }
                }

                if need_tx_preimage {
                    if !self.use_wal() && self.journal_file.is_some() {
                        self.append_journal_page(pgno, &orig)?;
                    }
                    self.tx_journaled.grow_to(self.tx_orig_page_count.max(pgno));
                    self.tx_journaled.set(pgno)?;
                    self.tx_preimages.insert(pgno, orig);
                } else if self.sp_page_buf_pool.len() < 64 {
                    self.sp_page_buf_pool.push(orig);
                }
            }
        }

        self.cache.mark_slot_dirty(slot);
        self.commit_phase_one_done = false;
        self.cache_modified = true;
        self.notify_backup_page_write(pgno);
        Ok(slot)
    }

    /// Returns a mutable byte slice for page `pgno` (`1..=page_count`), journaling its pre-image first.
    #[inline]
    pub fn write_page(&mut self, pgno: Pgno) -> Result<&mut [u8]> {
        let slot = self.mark_page_writable(pgno)?;
        Ok(self.cache.entry_data_mut(slot))
    }

    /// What C's cache holds for page `pgno` right after its content was moved to another page
    /// number (`sqlite3PagerMovepage`) and the page is fetched again: the image the database
    /// file (or the WAL) has — the content from before this transaction, or zeroes for a page
    /// the transaction added. The page stays writable. (A page the transaction has not
    /// journalled still equals its stored image; with the journal switched off there is no
    /// pre-image and the content is left as it is.)
    pub fn restore_stored_image(&mut self, pgno: Pgno) -> Result<()> {
        let slot = self.mark_page_writable(pgno)?;
        if pgno > self.tx_orig_page_count {
            self.cache.entry_data_mut(slot).fill(0);
        } else if let Some(orig) = self.tx_preimages.get(&pgno) {
            let n = orig.len();
            let d = self.cache.entry_data_mut(slot);
            if d.len() == n {
                d.copy_from_slice(orig);
            }
        }
        Ok(())
    }

    /// `sqlite3PagerGet(.., PAGER_GET_NOCONTENT)` (`getPageNormal`): the b-tree layer takes
    /// `pgno` off the free list and does not care what it held. C then calls
    /// `addToSavepointBitvecs`: the page counts as already saved in every savepoint that is
    /// open, so no image of it ever goes to the sub-journal for them and a rollback to one of
    /// them (ROLLBACK TO, or a statement that fails) leaves the page with whatever was last
    /// written to it — it is a free-list leaf again by then, so nothing reads it. Call before
    /// the page is made writable.
    pub fn mark_no_content(&mut self, pgno: Pgno) {
        for sp in &mut self.subsavepoints {
            if pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno) {
                sp.saved_pages.insert(pgno, Vec::new());
            }
        }
    }

    /// Tells the pager whether the first SQL savepoint of the connection is the "transaction
    /// savepoint" (`SAVEPOINT` in autocommit mode). C keeps that one out of `db->nSavepoint`:
    /// `sqlite3BtreeBeginTrans` opens `db->nSavepoint` pager savepoints, so while only the
    /// transaction savepoint is open `Pager.nSavepoint` is 0 and `sqlite3PagerDontWrite` works
    /// as in a `BEGIN` transaction. The slot this pager keeps for it is not counted there.
    pub fn set_transaction_savepoint(&mut self, on: bool) {
        self.transaction_savepoint = on;
    }

    /// `sqlite3PagerDontWrite` (`pager.c`): the b-tree layer says the content of `pgno` no
    /// longer matters (it has just become a free-list leaf). A page this transaction dirtied
    /// is then not written to the database file at commit, exactly as in C: only for a
    /// rollback-journal database file (not a temporary or in-memory one; in WAL mode C sets the
    /// flag but still writes the frame) and only while no savepoint is open (`nSavepoint==0`).
    /// The page keeps its cached content and its journalled pre-image, so rollback is
    /// unaffected. Returns whether a pending write was dropped.
    pub fn dont_write(&mut self, pgno: Pgno) -> bool {
        if self.state != TransactionState::Write
            || self.use_wal()
            || self.db_kind != DatabaseKind::NamedDisk
            || self.subsavepoints.len() > usize::from(self.transaction_savepoint)
        {
            return false;
        }
        self.cache.clear_dirty(pgno)
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
        // The first page after page 1 is the root of the first table (in every state C can
        // produce a table-less database is exactly one page long): the schema format and the
        // text encoding are recorded from here on, as `sqlite3StartTable` does.
        self.header.set_file_format_for_first_table();
        self.tx_journaled.grow_to(allocated_pgno);
        self.commit_phase_one_done = false;
        self.cache_modified = true;
        self.cache.insert(allocated_pgno, vec![0u8; ps], true);
        self.notify_backup_page_write(allocated_pgno);
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
            self.cache.truncate_above(new_count);
            self.page_count = new_count;
            self.header.database_size_pages = new_count;
            self.commit_phase_one_done = false;
            self.cache_modified = true;
            if new_count > 0 {
                let p1_slot = self.mark_page_writable(1)?;
                let hdr_copy = self.header.clone();
                hdr_copy.write_into(&mut self.cache.entry_data_mut(p1_slot)[0..DB_HEADER_SIZE]);
            }
            self.notify_backup_all_dirty();
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
                if patched_hdr.text_encoding == 0 {
                    patched_hdr.text_encoding = 1;
                }
            }
            patched_hdr.page_size = pgsz_src;
            let ver = if self.is_wal_mode() { 2 } else { 1 };
            patched_hdr.write_version = ver;
            patched_hdr.read_version = ver;
            patched_hdr.database_size_pages = n_src_pages;

            let next_cc = self.db_file_vers_cc.wrapping_add(1);
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
                    self.cache.insert(s_pgno, page_bytes, true);
                }
                self.cache.truncate_above(n_src_pages);
                self.cache.remove(dest_lock_pg);
                self.page_count = n_src_pages;
                self.tx_journaled
                    .grow_to(self.tx_orig_page_count.max(n_src_pages));
                if patched_hdr.page_size != self.header.page_size {
                    self.bump_data_version();
                }
                self.header = patched_hdr;
                self.configured_auto_vacuum = AutoVacuumMode::from_header(&self.header);
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

            let mut new_cache = PageCache::with_capacity(n_dest_pages as usize);
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
                new_cache.insert(d_pgno, d_buf, true);
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
        if copy_res.is_ok() {
            self.notify_backup_all_dirty();
        }
        copy_res
    }

    /// Clears any staged cross-page-size backup pages when a backup restarts from Page 1.
    pub fn backup_reset_raw_staging(&mut self) {
        self.backup_raw_staging = None;
        self.pending_byte_copy = None;
    }

    /// Writes a single source page `s_pgno` (`1..=n_src_pages`) into this destination pager
    /// during [`Backup::step`] (`backupOnePage` in `sqlite/src/backup.c:237-350`).
    pub fn backup_write_page(
        &mut self,
        s_pgno: Pgno,
        pgsz_src: u32,
        src_page_bytes: &[u8],
    ) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "backup_write_page requires an active write transaction on destination",
            ));
        }
        if self.readonly {
            return Err(SqliteError::readonly("destination database is read-only"));
        }
        let pgsz_dest = self.tx_orig_header.page_size;
        if pgsz_src != pgsz_dest && self.is_wal_mode() {
            return Err(SqliteError::readonly(
                "destination database is in WAL mode and page sizes differ",
            ));
        }

        self.ensure_journal_started()?;

        if pgsz_src != pgsz_dest {
            self.backup_raw_staging
                .get_or_insert_with(BTreeMap::new)
                .insert(s_pgno, src_page_bytes.to_vec());
            self.cache_modified = true;
            self.commit_phase_one_done = false;
            return Ok(());
        }

        let dest_lock_pg = self.lock_byte_page();
        if s_pgno == dest_lock_pg {
            return Ok(());
        }

        if s_pgno <= self.page_count {
            self.mark_page_writable(s_pgno)?;
        } else {
            let ps = pgsz_dest as usize;
            for gap_pg in (self.page_count + 1)..s_pgno {
                if gap_pg != dest_lock_pg {
                    self.cache.insert(gap_pg, vec![0u8; ps], true);
                }
            }
            self.page_count = s_pgno;
            self.header.database_size_pages = s_pgno;
            self.tx_journaled
                .grow_to(self.tx_orig_page_count.max(s_pgno));
        }

        self.cache.insert(s_pgno, src_page_bytes.to_vec(), true);
        self.cache_modified = true;
        self.commit_phase_one_done = false;
        self.notify_backup_page_write(s_pgno);
        Ok(())
    }

    /// Finalizes the destination database image at the end of [`Backup::step`] when all source
    /// pages have been copied (`rc == SQLITE_DONE` in `sqlite/src/backup.c:444-560`).
    pub fn backup_finalize(
        &mut self,
        raw_src_pages: Pgno,
        src_hdr: &DatabaseHeader,
        src_auto_vacuum: AutoVacuumMode,
    ) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "backup_finalize requires an active write transaction on destination",
            ));
        }

        let pgsz_src = src_hdr.page_size;
        let pgsz_dest = self.tx_orig_header.page_size;
        if pgsz_src != pgsz_dest && self.is_wal_mode() {
            return Err(SqliteError::readonly(
                "destination database is in WAL mode and page sizes differ",
            ));
        }

        let n_src_pages = raw_src_pages.max(1);
        let mut patched_hdr = src_hdr.clone();
        if raw_src_pages == 0 {
            match src_auto_vacuum {
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
            if patched_hdr.text_encoding == 0 {
                patched_hdr.text_encoding = 1;
            }
        }
        patched_hdr.page_size = pgsz_src;
        let ver = if self.is_wal_mode()
            || (raw_src_pages > 0
                && self.db_kind == DatabaseKind::NamedDisk
                && !self.is_memdb_backing
                && src_hdr.is_wal())
        {
            2
        } else {
            1
        };
        patched_hdr.write_version = ver;
        patched_hdr.read_version = ver;
        patched_hdr.database_size_pages = n_src_pages;

        let next_cc = self.db_file_vers_cc.wrapping_add(1);
        patched_hdr.change_counter = next_cc;
        patched_hdr.version_valid_for = next_cc;
        patched_hdr.sqlite_version_number = SQLITE_VERSION_NUMBER;
        // `sqlite3BtreeUpdateMeta(p->pDest, 1, p->iDestSchema+1)` (`backup.c:456`): the copy's schema
        // cookie is the destination's cookie at the start of its transaction plus one, whatever the
        // source's cookie is.
        patched_hdr.schema_cookie = self.tx_orig_header.schema_cookie.wrapping_add(1);

        self.ensure_journal_started()?;

        if pgsz_src == pgsz_dest {
            if self.page_count > n_src_pages {
                self.shrink_page_count(n_src_pages)?;
            }
            if raw_src_pages == 0 {
                // `sqlite3BtreeNewDb(p->pDest)` → `newDatabase()` → `zeroPage(pP1, ..)`: the
                // file header and the 8-byte b-tree header of page 1 are rewritten; without
                // secure-delete the rest of an existing page 1 keeps the bytes it had (the old
                // cell pointers and cells are simply no longer referenced).
                let mut p1 = if self.page_count >= 1 {
                    let slot = self.mark_page_writable(1)?;
                    let mut old = self.cache.entry_data_mut(slot).to_vec();
                    old.resize(pgsz_src as usize, 0);
                    old
                } else {
                    vec![0u8; pgsz_src as usize]
                };
                p1[100] = 0x0d;
                p1[101..105].fill(0);
                let usable = patched_hdr.usable_size();
                let content_start: u16 = if usable == 65_536 { 0 } else { usable as u16 };
                p1[105..107].copy_from_slice(&content_start.to_be_bytes());
                p1[107] = 0;
                patched_hdr.write_into(&mut p1[0..DB_HEADER_SIZE]);
                self.cache.insert(1, p1, true);
            } else {
                let p1_slot = self.mark_page_writable(1)?;
                patched_hdr.write_into(&mut self.cache.entry_data_mut(p1_slot)[0..DB_HEADER_SIZE]);
            }
            let dest_lock_pg = self.lock_byte_page();
            self.cache.truncate_above(n_src_pages);
            self.cache.remove(dest_lock_pg);
            self.page_count = n_src_pages;
            self.tx_journaled
                .grow_to(self.tx_orig_page_count.max(n_src_pages));
            if patched_hdr.page_size != self.header.page_size {
                self.bump_data_version();
            }
            self.header = patched_hdr;
            self.configured_auto_vacuum = AutoVacuumMode::from_header(&self.header);
            self.change_counter_bumped = true;
            self.cache_modified = true;
            self.commit_phase_one_done = false;
            self.pending_byte_copy = None;
            self.backup_raw_staging = None;
            self.staged_raw_copy = true;
            self.next_page_size = None;
            self.notify_backup_all_dirty();
            return Ok(());
        }

        // `pgsz_src != pgsz_dest`: journal all existing destination pages in `pgsz_dest` geometry
        // and assemble the destination cache + `PendingByteCopy` from `self.backup_raw_staging`.
        let staged = self.backup_raw_staging.take().unwrap_or_default();
        let src_lock_pg = self.lock_geometry.lock_byte_page(pgsz_src);
        let get_staged_src_page = |s_pgno: Pgno| -> Vec<u8> {
            if s_pgno == 1 {
                let mut p1 = if raw_src_pages == 0 {
                    let mut buf = vec![0u8; pgsz_src as usize];
                    buf[100] = 0x0d;
                    let usable = patched_hdr.usable_size();
                    let content_start: u16 = if usable == 65_536 { 0 } else { usable as u16 };
                    buf[105..107].copy_from_slice(&content_start.to_be_bytes());
                    buf
                } else {
                    staged
                        .get(&1)
                        .cloned()
                        .unwrap_or_else(|| vec![0u8; pgsz_src as usize])
                };
                patched_hdr.write_into(&mut p1[0..DB_HEADER_SIZE]);
                p1
            } else if s_pgno == src_lock_pg || s_pgno > raw_src_pages {
                vec![0u8; pgsz_src as usize]
            } else {
                staged
                    .get(&s_pgno)
                    .cloned()
                    .unwrap_or_else(|| vec![0u8; pgsz_src as usize])
            }
        };

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

        let mut new_cache = PageCache::with_capacity(n_dest_pages as usize);
        for d_pgno in 1..=n_dest_pages {
            if d_pgno == dest_lock_pg {
                continue;
            }
            let d_off = u64::from(d_pgno - 1) * u64::from(pgsz_dest);
            let mut d_buf = vec![0u8; pgsz_dest as usize];
            if pgsz_src > pgsz_dest {
                let s_pgno = (d_off / u64::from(pgsz_src)) as Pgno + 1;
                let slice_off = (d_off % u64::from(pgsz_src)) as usize;
                let s_buf = get_staged_src_page(s_pgno);
                d_buf.copy_from_slice(&s_buf[slice_off..slice_off + pgsz_dest as usize]);
            } else {
                let ratio = pgsz_dest / pgsz_src;
                for k in 0..ratio {
                    let sub_off = d_off + u64::from(k) * u64::from(pgsz_src);
                    if sub_off >= total_bytes {
                        break;
                    }
                    let s_pgno = (sub_off / u64::from(pgsz_src)) as Pgno + 1;
                    let s_buf = get_staged_src_page(s_pgno);
                    let dst_start = (k * pgsz_src) as usize;
                    d_buf[dst_start..dst_start + pgsz_src as usize].copy_from_slice(&s_buf);
                }
            }
            new_cache.insert(d_pgno, d_buf, true);
        }

        let pb_off = self.lock_geometry.pending_byte_offset();
        let tail_start = pb_off + u64::from(pgsz_src);
        let tail_end = (pb_off + u64::from(pgsz_dest)).min(total_bytes);
        let post_pending_tail = if pgsz_src < pgsz_dest && tail_start < tail_end {
            let mut tail_bytes = Vec::with_capacity((tail_end - tail_start) as usize);
            let mut off = tail_start;
            while off < tail_end {
                let s_pgno = (off / u64::from(pgsz_src)) as Pgno + 1;
                let s_buf = get_staged_src_page(s_pgno);
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
        self.notify_backup_all_dirty();
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

    /// Appends a super-journal trailer to the open rollback journal (`writeSuperJournal` in
    /// `sqlite/src/pager.c:1752-1816`).
    ///
    /// No-op when `super_name` is `None`/empty, `self.set_super` is already set, WAL mode is
    /// active, `self.journal_mode` is `Memory` or `Off`, or no rollback journal file is open.
    pub fn write_super_journal(&mut self, super_name: Option<&str>) -> Result<()> {
        let Some(z_super) = super_name else {
            return Ok(());
        };
        if z_super.is_empty()
            || self.set_super
            || self.use_wal()
            || matches!(self.journal_mode, JournalMode::Memory | JournalMode::Off)
            || self.journal_file.is_none()
        {
            return Ok(());
        }
        if !super_journal::is_super_journal_name(z_super) {
            return Err(SqliteError::misuse(format!(
                "invalid super-journal filename: {z_super}"
            )));
        }

        self.set_super = true;
        if self.synchronous >= 2 {
            self.journal_off =
                super_journal::journal_hdr_offset(self.journal_off, self.journal_sector_size);
        }
        let i_hdr_off = self.journal_off;
        let name_bytes = z_super.as_bytes();
        let n_super = u32::try_from(name_bytes.len())
            .map_err(|_| SqliteError::misuse("super-journal filename length overflow"))?;
        let cksum = super_journal::super_journal_checksum(name_bytes);
        let sj_pgno = self.lock_geometry.sj_pgno(self.tx_orig_header.page_size);

        let jfd = self.journal_file.as_deref().expect("checked Some");
        jfd.write_at(&sj_pgno.to_be_bytes(), i_hdr_off)?;
        jfd.write_at(name_bytes, i_hdr_off + 4)?;
        jfd.write_at(&n_super.to_be_bytes(), i_hdr_off + 4 + u64::from(n_super))?;
        jfd.write_at(&cksum.to_be_bytes(), i_hdr_off + 8 + u64::from(n_super))?;
        jfd.write_at(&JOURNAL_MAGIC, i_hdr_off + 12 + u64::from(n_super))?;

        self.journal_off = i_hdr_off + u64::from(n_super) + 20;
        let jrnl_size = jfd.file_size()?;
        if jrnl_size > self.journal_off {
            jfd.truncate(self.journal_off)?;
        }
        Ok(())
    }

    /// Executes Phase 1 of a two-phase commit (`sqlite3PagerCommitPhaseOne` in
    /// `sqlite/src/pager.c:6527-6746`):
    ///
    /// - No-op in `TransactionState::Open` or `TransactionState::Read`.
    /// - In WAL mode: appends dirty cache pages as a commit frame batch to `-wal` and marks cache
    ///   pages clean (deferring `end_write_transaction` and lock downgrade to [`Self::commit_phase_two`]).
    /// - In rollback-journal mode: bumps Page 1's change counter, acquires `LockLevel::Exclusive`,
    ///   writes the optional `super_journal` trailer **before** syncing `<db>-journal`, flushes
    ///   dirty pages to `.db`, truncates `.db` if needed, syncs `.db`, and marks cache pages clean.
    pub fn commit_phase_one(&mut self, super_journal: Option<&str>) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write || self.commit_phase_one_done {
            return Ok(());
        }

        if self.use_wal() {
            let lock_byte_pgno = self.lock_byte_page();
            let has_dirty_pages = self
                .cache
                .has_valid_dirty_pages(self.page_count, lock_byte_pgno);
            let needs_db_flush = self.cache_modified
                || has_dirty_pages
                || self.page_count != self.tx_orig_page_count;

            if needs_db_flush && self.page_count > 0 {
                let p1_already_dirty = self.cache.is_dirty(1);
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
                    self.dv_wal_page1_written = true;
                    let p1_slot = self.mark_page_writable(1)?;
                    self.header.write_version = 2;
                    self.header.read_version = 2;
                    self.header.database_size_pages = self.page_count;
                    if !self.change_counter_bumped {
                        // `pagerWalFrames` → `pager_write_changecounter`: `dbFileVers + 1`.
                        // `dbFileVers` is not advanced by a WAL commit.
                        self.header.change_counter = self.db_file_vers_cc.wrapping_add(1);
                        self.header.version_valid_for = self.header.change_counter;
                        self.header.sqlite_version_number = SQLITE_VERSION_NUMBER;
                        self.change_counter_bumped = true;
                    }

                    let hdr_copy = self.header.clone();
                    hdr_copy.write_into(&mut self.cache.entry_data_mut(p1_slot)[0..DB_HEADER_SIZE]);
                }

                let dirty_pgnos = self
                    .cache
                    .collect_valid_dirty_pgnos_sorted(self.page_count, lock_byte_pgno);

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
                            data: self.cache.get_data(pgno).expect("dirty page exists"),
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

                self.cache.truncate_above(self.page_count);
                self.cache.clear_all_dirty();
            } else if self.page_count == 0 {
                self.wal
                    .as_mut()
                    .expect("use_wal checked")
                    .undo(|_| Ok(()))?;
            }

            self.commit_phase_one_done = true;
            self.notify_backup_self_commit();
            return Ok(());
        }

        let lock_byte_pgno = self.lock_byte_page();
        let has_dirty_pages = self
            .cache
            .has_valid_dirty_pages(self.page_count, lock_byte_pgno);
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
                let p1_slot = self.mark_page_writable(1)?;
                self.header.database_size_pages = self.page_count;
                if !self.change_counter_bumped {
                    // `pager_incr_changecounter` → `pager_write_changecounter`:
                    // `dbFileVers + 1`; `pager_write_pagelist` then copies page 1's bytes
                    // 24..39 back into `dbFileVers`.
                    self.header.change_counter = self.db_file_vers_cc.wrapping_add(1);
                    self.header.version_valid_for = self.header.change_counter;
                    self.header.sqlite_version_number = SQLITE_VERSION_NUMBER;
                    self.change_counter_bumped = true;
                }

                let hdr_copy = self.header.clone();
                hdr_copy.write_into(&mut self.cache.entry_data_mut(p1_slot)[0..DB_HEADER_SIZE]);
            }

            // Step 1: Acquire EXCLUSIVE lock BEFORE modifying `.db` (`syncJournal` in `pager.c:4349`).
            self.lock_db_file(LockLevel::Exclusive)?;

            // Step 2: Append optional super-journal trailer BEFORE syncing the rollback journal
            // (`pager.c:6660-6675`)!
            self.write_super_journal(super_journal)?;

            // Step 3: Update `nRec` in rollback journal header and sync BEFORE touching `.db`!
            if self.journal_started_in_tx
                && let Some(ref jfile) = self.journal_file
            {
                self.sync_journal_before_db_write(&**jfile)?;
            }

            // Step 4: Write all dirty pages in ascending `Pgno` order to `.db`, truncate, and sync.
            self.db_file_modified = true;

            let ps = u64::from(self.header.page_size);
            let dirty_pgnos = self
                .cache
                .collect_valid_dirty_pgnos_sorted(self.page_count, lock_byte_pgno);

            for &pgno in &dirty_pgnos {
                let page_data = self.cache.get_data(pgno).expect("dirty page exists");
                let offset = u64::from(pgno - 1) * ps;
                self.db_file()?.write_at(page_data, offset)?;
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

            self.cache.truncate_above(self.page_count);
            self.cache.clear_all_dirty();
            self.commit_phase_one_done = true;
            self.notify_backup_self_commit();
            if self.is_memdb_backing && self.cache_modified {
                // `sqlite3PagerCommitPhaseOne` (`pager.c:6554-6560`): "If this is an in-memory db
                // ... any backup in progress needs to be restarted." C does not feed the changed
                // pages of an in-memory source to its backups, it restarts them at page 1. (C
                // returns before that when the transaction made no page writable:
                // `if( pPager->eState<PAGER_WRITER_CACHEMOD ) return SQLITE_OK;`.)
                self.notify_backup_restart();
            }
        } else if self.locking_mode == LockingMode::Exclusive {
            self.lock_db_file(LockLevel::Exclusive)?;
        }

        Ok(())
    }

    /// Executes Phase 2 of a two-phase commit (`sqlite3PagerCommitPhaseTwo` in
    /// `sqlite/src/pager.c:6764-6802` and `sqlite3BtreeCommitPhaseTwo` in `btree.c:4432-4459`):
    ///
    /// - Finalizes the rollback journal (or releases `WAL_WRITE_LOCK` in WAL mode) and downgrades
    ///   database locks to `Read` (`retain_read == true`) or `Open` (`retain_read == false`).
    /// - When `cleanup_benign` is `true` (post-super-journal-unlink cleanup in `vdbeCommit`), any
    ///   I/O error while cleaning up cold journals or unlocking is absorbed without rolling back
    ///   the already-committed database, and the pager still transitions cleanly out of `Write`.
    pub fn commit_phase_two(&mut self, retain_read: bool, cleanup_benign: bool) -> Result<()> {
        self.ensure_not_closed()?;
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => {
                if cleanup_benign {
                    let _ = self.finish_tx_to_read_or_open(retain_read);
                    self.state = if retain_read {
                        TransactionState::Read
                    } else {
                        TransactionState::Open
                    };
                    Ok(())
                } else {
                    self.finish_tx_to_read_or_open(retain_read)
                }
            }
            TransactionState::Write => {
                if self.use_wal() {
                    if cleanup_benign {
                        let _ = self
                            .wal
                            .as_mut()
                            .expect("use_wal checked")
                            .end_write_transaction();
                    } else {
                        self.wal
                            .as_mut()
                            .expect("use_wal checked")
                            .end_write_transaction()?;
                    }

                    self.cache.truncate_above(self.page_count);
                    self.cache.clear_all_dirty();
                    self.set_super = false;
                    self.commit_phase_one_done = false;
                    self.db_file_modified = false;
                    self.cache_modified = false;
                    self.change_counter_bumped = false;
                    self.journal_started_in_tx = false;
                    self.pending_byte_copy = None;
                    self.staged_raw_copy = false;
                    self.backup_raw_staging = None;
                    self.tx_preimages.clear();
                    self.tx_journaled.clear_all();
                    self.subsavepoints.clear();
                    self.header_loaded = true;
                    self.page_size_fixed = self.page_count > 0;
                    if self.page_count > 0 {
                        self.configured_auto_vacuum = AutoVacuumMode::from_header(&self.header);
                    }
                    self.notify_backup_self_commit();

                    if cleanup_benign {
                        let _ = self.finish_tx_to_read_or_open(retain_read);
                        self.state = if retain_read {
                            TransactionState::Read
                        } else {
                            TransactionState::Open
                        };
                        return Ok(());
                    }
                    return self.finish_tx_to_read_or_open(retain_read);
                }

                let mut cold_journal_leftover = false;
                if cleanup_benign {
                    if self.finalize_journal_after_tx(true).is_err() {
                        cold_journal_leftover = true;
                        if let Some(mut jfd) = self.journal_file.take() {
                            let _ = jfd.close();
                        }
                        self.journal_off = 0;
                    }
                } else {
                    // `journal_started_in_tx`, `set_super`, `db_file_modified`, and `tx_preimages`
                    // are cleared ONLY AFTER `finalize_journal_after_tx(true)` succeeds so both
                    // `commit()` retry and `rollback()` remain safe in single-DB commits!
                    self.finalize_journal_after_tx(true)?;
                }

                if let Some(pending) = self.pending_byte_copy.take() {
                    self.cache.clear();
                    if pending.new_header.page_size != self.header.page_size {
                        // The copy changed the page size of the cache
                        // (`sqlite3PagerSetPagesize` → `pager_reset()`).
                        self.bump_data_version();
                    }
                    self.header = pending.new_header;
                    self.page_count = pending.new_page_count;
                    self.next_page_size = None;
                }
                if cleanup_benign {
                    self.cache.truncate_above(self.page_count);
                    self.cache.clear_all_dirty();
                }
                self.set_super = false;
                self.staged_raw_copy = false;
                self.backup_raw_staging = None;
                self.commit_phase_one_done = false;
                self.db_file_modified = false;
                self.cache_modified = false;
                if self.change_counter_bumped {
                    // `pager_write_pagelist`: page 1 reached the database file.
                    self.db_file_vers_cc = self.header.change_counter;
                }
                self.change_counter_bumped = false;
                self.journal_started_in_tx = false;
                self.tx_preimages.clear();
                self.tx_journaled.clear_all();
                self.subsavepoints.clear();
                self.header_loaded = !cold_journal_leftover;
                self.page_size_fixed = self.page_count > 0;
                if self.page_count > 0 {
                    self.configured_auto_vacuum = AutoVacuumMode::from_header(&self.header);
                }
                self.notify_backup_self_commit();

                if cleanup_benign {
                    let _ = self.finish_tx_to_read_or_open(retain_read);
                    self.state = if retain_read {
                        TransactionState::Read
                    } else {
                        TransactionState::Open
                    };
                    Ok(())
                } else {
                    self.finish_tx_to_read_or_open(retain_read)
                }
            }
        }
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
                self.commit_phase_one(None)?;
                self.commit_phase_two(retain_read, false)
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
                    // `pagerRollbackWal` calls `pagerUndoCallback` — which restarts the backups
                    // (`sqlite3BackupRestart`) — once per frame this transaction wrote to the
                    // log and once per page that is dirty in the cache. A transaction that
                    // changed no page (for example a statement that failed on its first row)
                    // restarts nothing.
                    let wal_tx_changed_pages = !reverted_pgnos.is_empty() || self.cache_modified;
                    for pgno in reverted_pgnos {
                        self.cache.remove(pgno);
                    }

                    self.cache.truncate_above(orig_count);
                    // `pagerRollbackWal` → `pagerUndoCallback` → `readDbPage(1)`: a page 1 that
                    // was dirty is re-read, which refreshes `dbFileVers`.
                    let page1_restored = self.tx_preimages.contains_key(&1);
                    for (pgno, orig_data) in self.tx_preimages.drain() {
                        if pgno <= orig_count {
                            self.cache.insert(pgno, orig_data, false);
                        }
                    }
                    self.cache.clear_all_dirty();

                    self.page_count = orig_count;
                    self.header = self.tx_orig_header.clone();
                    if page1_restored {
                        self.db_file_vers_cc = self.header.change_counter;
                    }
                    self.page_size_fixed = orig_count > 0;
                    self.configured_auto_vacuum = if orig_count > 0 {
                        AutoVacuumMode::from_header(&self.header)
                    } else {
                        self.tx_orig_auto_vacuum
                    };
                    self.tx_journaled.clear_all();
                    self.set_super = false;
                    self.commit_phase_one_done = false;
                    self.db_file_modified = false;
                    self.cache_modified = false;
                    self.change_counter_bumped = false;
                    self.journal_started_in_tx = false;
                    self.pending_byte_copy = None;
                    self.staged_raw_copy = false;
                    self.backup_raw_staging = None;
                    self.subsavepoints.clear();
                    if wal_tx_changed_pages {
                        self.notify_backup_restart();
                    }

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
                self.cache.truncate_above(orig_count);

                // Restore pre-images into cache
                for (pgno, orig_data) in self.tx_preimages.drain() {
                    if pgno <= orig_count {
                        self.cache.insert(pgno, orig_data, false);
                    }
                }
                self.cache.clear_all_dirty();

                self.page_count = orig_count;
                self.header = self.tx_orig_header.clone();
                self.page_size_fixed = orig_count > 0;
                self.configured_auto_vacuum = if orig_count > 0 {
                    AutoVacuumMode::from_header(&self.header)
                } else {
                    self.tx_orig_auto_vacuum
                };
                self.tx_journaled.clear_all();
                self.commit_phase_one_done = false;
                self.db_file_modified = false;
                self.cache_modified = false;
                self.change_counter_bumped = false;
                self.pending_byte_copy = None;
                self.staged_raw_copy = false;
                self.backup_raw_staging = None;
                self.subsavepoints.clear();
                // C plays the rollback journal back page by page and feeds each restored page
                // to `sqlite3BackupUpdate()` (`pager_playback_one_page`); a backup is not
                // restarted. The pages of this transaction are still in the trackers' dirty
                // sets, so the next step copies their restored content again.

                // Finalize journal file (`Delete` unlinks even if `journal_file` was already closed
                // by a failed `commit()`, `Truncate` truncates to 0, `Persist` zeroes 28B) BEFORE
                // clearing `self.journal_started_in_tx`!
                self.finalize_journal_after_tx(false)?;
                self.set_super = false;
                self.journal_started_in_tx = false;

                self.finish_tx_to_read_or_open(retain_read)
            }
        }
    }

    fn rollback_journal_mode_off(&mut self, retain_read: bool) -> Result<()> {
        let was_modified = self.cache_modified || self.db_file_modified || self.cache.any_dirty();

        if self.is_memdb_backing {
            // `pager.c:6850` / `2178`: in `MEMDB` with `journal_mode=OFF`, dirty flags are cleared
            // and the current in-memory pages remain intact!
            if was_modified && self.page_count > 0 {
                self.header.database_size_pages = self.page_count;
                let hdr_copy = self.header.clone();
                if let Some(p1) = self.cache.get_data_mut(1) {
                    hdr_copy.write_into(&mut p1[0..DB_HEADER_SIZE]);
                }
            }
            let ps = u64::from(self.header.page_size);
            self.cache.truncate_above(self.page_count);
            if let Some(ref db_file) = self.file {
                let lock_byte_pgno = self.lock_byte_page();
                let dirty_pgnos = self
                    .cache
                    .collect_valid_dirty_pgnos_sorted(self.page_count, lock_byte_pgno);
                for pgno in dirty_pgnos {
                    if let Some(page_data) = self.cache.get_data(pgno) {
                        let offset = u64::from(pgno - 1) * ps;
                        let _ = db_file.write_at(page_data, offset);
                    }
                }
                let target_size = u64::from(self.page_count) * ps;
                let _ = db_file.truncate(target_size);
            }
            self.cache.clear_all_dirty();
            self.page_size_fixed = self.page_count > 0;
            if self.page_count > 0 {
                self.configured_auto_vacuum = AutoVacuumMode::from_header(&self.header);
            }
            self.header_loaded = true;
        } else if was_modified {
            // Disk DB with `journal_mode=OFF` (`savepoint-13.4`): discard dirty in-memory cache
            // and mark `header_loaded = false` so subsequent reads reload untouched pages from disk!
            self.cache.clear();
            self.header = self.tx_orig_header.clone();
            self.configured_auto_vacuum = self.tx_orig_auto_vacuum;
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
        self.backup_raw_staging = None;
        self.tx_preimages.clear();
        self.tx_journaled.clear_all();
        self.subsavepoints.clear();
        self.notify_backup_restart();
        self.finalize_journal_after_tx(false)?;
        self.set_super = false;
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
                let sync_dir = is_commit && self.synchronous >= 3;
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
                    let has_super = self.set_super;
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
            let wsp = self.wal.as_ref().map(Wal::savepoint);
            let slot = if let Some(mut reused) = self.sp_slot_pool.pop() {
                reused.orig_page_count = self.page_count;
                reused.orig_header = self.header.clone();
                reused.saved_pages.clear();
                reused.wal_savepoint = wsp;
                reused
            } else {
                SubsavepointSlot {
                    orig_page_count: self.page_count,
                    orig_header: self.header.clone(),
                    saved_pages: FastPgnoMap::default(),
                    wal_savepoint: wsp,
                }
            };
            self.subsavepoints.push(slot);
        }
        Ok(())
    }

    #[inline]
    fn recycle_subsavepoint_slot(&mut self, mut sp: SubsavepointSlot) {
        for (_, buf) in sp.saved_pages.drain() {
            if self.sp_page_buf_pool.len() < 64 {
                self.sp_page_buf_pool.push(buf);
            }
        }
        if self.sp_slot_pool.len() < 8 {
            self.sp_slot_pool.push(sp);
        }
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
            self.recycle_subsavepoint_slot(sp);
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
            self.cache.retain_dirty_only();
        }
        let target = self.subsavepoints[index].clone();
        self.apply_subsavepoint_undo(&target);
        for (_, buf) in self.subsavepoints[index].saved_pages.drain() {
            if self.sp_page_buf_pool.len() < 64 {
                self.sp_page_buf_pool.push(buf);
            }
        }
        // (C: a savepoint rollback restores the pages in the cache; backups are neither
        // restarted nor updated. The pages stay in the trackers' dirty sets.)
        Ok(())
    }

    fn apply_subsavepoint_undo(&mut self, sp: &SubsavepointSlot) {
        self.commit_phase_one_done = false;
        self.pending_byte_copy = None;
        self.staged_raw_copy = false;
        self.backup_raw_staging = None;
        self.cache.truncate_above(sp.orig_page_count);
        for (&pgno, orig_bytes) in &sp.saved_pages {
            // (An empty image marks a page fetched with `PAGER_GET_NOCONTENT`: see
            // `Self::mark_no_content`. It is not played back.)
            if pgno <= sp.orig_page_count && !orig_bytes.is_empty() {
                self.cache.insert(pgno, orig_bytes.clone(), true);
            }
        }
        self.page_count = sp.orig_page_count;
        self.header = sp.orig_header.clone();
        if sp.saved_pages.contains_key(&1) {
            // `pager_playback_one_page`: "If this was page 1, then restore the value of
            // Pager.dbFileVers".
            self.db_file_vers_cc = self.header.change_counter;
        }
        if self.page_count == 0 {
            self.page_size_fixed = self.tx_orig_page_count > 0;
            self.configured_auto_vacuum = self.tx_orig_auto_vacuum;
        } else {
            self.configured_auto_vacuum = AutoVacuumMode::from_header(&self.header);
            let hdr_copy = self.header.clone();
            if let Some(p1) = self.cache.get_data_mut(1) {
                hdr_copy.write_into(&mut p1[0..DB_HEADER_SIZE]);
            }
        }
    }

    /// Releases sub-savepoint `index` and any inner slots (`index..`), merging their pre-images
    /// into parent slot `index - 1` when `index > 0`.
    pub fn release_subsavepoint(&mut self, index: usize) -> Result<()> {
        self.ensure_not_closed()?;
        if self.state != TransactionState::Write || index >= self.subsavepoints.len() {
            return Ok(());
        }
        while self.subsavepoints.len() > index {
            let mut sp = self.subsavepoints.pop().expect("len > index");
            if index > 0 {
                let parent = &mut self.subsavepoints[index - 1];
                for (pgno, data) in sp.saved_pages.drain() {
                    if pgno <= parent.orig_page_count && !parent.saved_pages.contains_key(&pgno) {
                        parent.saved_pages.insert(pgno, data);
                    } else if self.sp_page_buf_pool.len() < 64 {
                        self.sp_page_buf_pool.push(data);
                    }
                }
                if self.sp_slot_pool.len() < 8 {
                    self.sp_slot_pool.push(sp);
                }
            } else {
                self.recycle_subsavepoint_slot(sp);
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
        self.set_super = false;
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

    /// `setSectorSize` (`pager.c`): 512 for a temporary file or when the device promises
    /// power-safe overwrite (`SQLITE_IOCAP_POWERSAFE_OVERWRITE`, the default of the OS layer);
    /// otherwise the VFS sector size. It is the size of the rollback-journal header segment.
    fn effective_sector_size(&self) -> u32 {
        let psow = self.file.as_deref().is_some_and(|f| {
            f.device_characteristics() & sqliters_vfs::IOCAP_POWERSAFE_OVERWRITE != 0
        });
        sqliters_types::dtrace!(
            "TR|journal|sector|o={}",
            if self.db_kind != DatabaseKind::NamedDisk {
                "temp-512"
            } else if psow {
                "psow-512"
            } else {
                "vfs-sector-size"
            }
        );
        // (`tempFile` covers C's temporary and in-memory pagers alike.)
        if self.db_kind != DatabaseKind::NamedDisk || psow {
            return 512;
        }
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
            if self.locking_mode == LockingMode::Normal {
                self.unlock_db_file(LockLevel::Shared)?;
            }
            return Ok(true);
        }

        // Inspect optional super-journal trailer BEFORE playing back `.db` (`pager.c:2894-2905`).
        let z_super = match super_journal::read_super_journal(
            &*jfile,
            super_journal::SUPER_JOURNAL_MAX_PATH_BYTES,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = jfile.close();
                if self.locking_mode == LockingMode::Normal {
                    let _ = self.unlock_db_file(LockLevel::Shared);
                }
                return Err(e);
            }
        };

        if let Some(ref super_name) = z_super {
            // CRITICAL (`implementation_plan.md:589`): Any I/O error from `vfs.exists` MUST be
            // propagated and NEVER treated as `false` ("super-journal deleted -> committed")!
            let super_exists = match self.vfs.exists(Path::new(super_name)) {
                Ok(exists) => exists,
                Err(e) => {
                    let _ = jfile.close();
                    if self.locking_mode == LockingMode::Normal {
                        let _ = self.unlock_db_file(LockLevel::Shared);
                    }
                    return Err(e);
                }
            };

            if !super_exists {
                // Super-journal was already unlinked: the multi-DB transaction COMMITTED!
                // Do NOT play back `.db` (`pager.c:2895-2905`).
                let close_res = jfile.close();
                close_res?;
                self.finalize_recovered_journal_file(&jpath, true)?;
                self.cache.clear();
                self.page_size_fixed = false;
                self.header_loaded = false;
                if self.locking_mode == LockingMode::Normal {
                    self.unlock_db_file(LockLevel::Shared)?;
                }
                return Ok(true);
            }
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

        // Finalize `&jpath` BEFORE calling `pager_delsuper` (`pager.c:3036-3046`) so sibling
        // journal inspection sees this database's journal already deleted or truncated to 0 bytes!
        self.finalize_recovered_journal_file(&jpath, z_super.is_some())?;
        if let Some(ref super_name) = z_super {
            self.pager_delsuper(super_name)?;
        }

        self.cache.clear();
        self.page_size_fixed = false;
        self.header_loaded = false;
        if self.locking_mode == LockingMode::Normal {
            self.unlock_db_file(LockLevel::Shared)?;
        }
        Ok(true)
    }

    fn finalize_recovered_journal_file(&mut self, jpath: &Path, has_super: bool) -> Result<()> {
        if !has_super {
            return self.vfs.delete(jpath, true);
        }
        match self.journal_mode {
            JournalMode::Truncate | JournalMode::Persist => {
                let mut jf = self
                    .vfs
                    .open(Some(jpath), OpenFlags::READWRITE | OpenFlags::MAIN_JOURNAL)?;
                let res: Result<()> = (|| {
                    jf.truncate(0)?;
                    if self.journal_mode == JournalMode::Persist && self.synchronous > 0 {
                        jf.sync(SyncFlags::DataOnly)?;
                    } else if self.journal_mode == JournalMode::Truncate && self.synchronous >= 2 {
                        let sflags = if self.synchronous >= 3 {
                            SyncFlags::Full
                        } else {
                            SyncFlags::Normal
                        };
                        jf.sync(sflags)?;
                    }
                    Ok(())
                })();
                let close_res = jf.close();
                res?;
                close_res
            }
            JournalMode::Delete | JournalMode::Memory | JournalMode::Off | JournalMode::Wal => {
                self.vfs.delete(jpath, true)
            }
        }
    }

    /// Inspects super-journal `super_name` after rolling back this pager's hot journal and deletes
    /// `super_name` (`sync_dir = false`) if and only if:
    /// 1. [`super_journal::is_super_journal_name`] accepts `super_name`,
    /// 2. `super_name` lists this pager's `journal_path` (`b_seen == true`), and
    /// 3. no remaining existing sibling journal listed in `super_name` still ends with a valid
    ///    super-journal trailer pointing to `super_name` (`pager_delsuper` in `pager.c:2583-2703`).
    fn pager_delsuper(&self, super_name: &str) -> Result<()> {
        if !super_journal::is_super_journal_name(super_name) {
            return Ok(());
        }
        let super_path = Path::new(super_name);
        let mut p_super = self.vfs.open(
            Some(super_path),
            OpenFlags::READONLY | OpenFlags::SUPER_JOURNAL,
        )?;

        let inspect_res: Result<(bool, bool)> = (|| {
            let sz = p_super.file_size()?;
            let sz_usize = usize::try_from(sz).map_err(|_| {
                SqliteError::with_extended(
                    extended::SQLITE_IOERR_READ,
                    "super-journal size exceeds addressable memory",
                )
            })?;
            let mut buf = vec![0u8; sz_usize];
            if sz_usize > 0 {
                super_journal::read_exact_from_vfs(&*p_super, &mut buf, 0)?;
            }

            let own_jpath_str = self.journal_path().and_then(Path::to_str);
            let mut b_seen = false;
            let mut pos = 0usize;
            while pos < buf.len() {
                let rel_nul = buf[pos..]
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(buf.len() - pos);
                let entry_bytes = &buf[pos..pos + rel_nul];
                pos = pos + rel_nul + 1;

                if entry_bytes.is_empty() {
                    continue;
                }
                let Ok(entry_str) = std::str::from_utf8(entry_bytes) else {
                    continue;
                };
                if Some(entry_str) == own_jpath_str {
                    b_seen = true;
                } else {
                    let child_path = Path::new(entry_str);
                    if self.vfs.exists(child_path)? {
                        let mut child_jf = self.vfs.open(
                            Some(child_path),
                            OpenFlags::READONLY | OpenFlags::SUPER_JOURNAL,
                        )?;
                        let ptr_res = super_journal::read_super_journal(
                            &*child_jf,
                            super_journal::SUPER_JOURNAL_MAX_PATH_BYTES,
                        );
                        let close_child = child_jf.close();
                        let child_ptr = ptr_res?;
                        close_child?;
                        if child_ptr.as_deref() == Some(super_name) {
                            return Ok((b_seen, true));
                        }
                    }
                }
            }
            Ok((b_seen, false))
        })();

        let close_super = p_super.close();
        let (b_seen, sibling_active) = inspect_res?;
        close_super?;

        if !sibling_active && b_seen {
            self.vfs.delete(super_path, false)?;
        }
        Ok(())
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

    /// A WAL commit larger than one append batch must put exactly the same bytes in `-wal` as
    /// frame-by-frame writes: every frame decodes with the running checksum chain, page numbers
    /// ascend, only the final frame carries the commit size, and a second connection reads back
    /// every page through the WAL-index.
    #[test]
    fn test_wal_commit_spanning_multiple_append_batches_is_byte_exact() {
        let vfs = Arc::new(MemoryVfs::new());
        let db_path = Path::new("wal_batch.db");
        let mut pager = Pager::open_with_vfs(
            vfs.clone(),
            Some(db_path),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        pager.begin_write().unwrap();
        let p2 = pager.allocate_raw_page().unwrap();
        pager.write_page(p2).unwrap()[0..4].copy_from_slice(b"INIT");
        pager.commit().unwrap();
        assert_eq!(
            pager.set_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );

        // 1 small commit, then one commit of 49 frames (3 full 15-frame batches + a 4-frame tail
        // at 4096-byte pages), then a 2-frame commit.
        let page_size = pager.page_size() as usize;
        let stamp = |pgno: Pgno, round: u8, buf: &mut [u8]| {
            for (i, b) in buf.iter_mut().enumerate().skip(16) {
                *b = (i as u8) ^ (pgno as u8).wrapping_mul(31) ^ round;
            }
            buf[16..20].copy_from_slice(&pgno.to_be_bytes());
            buf[20] = round;
        };

        pager.begin_write().unwrap();
        stamp(2, 1, pager.write_page(2).unwrap());
        pager.commit().unwrap();

        pager.begin_write().unwrap();
        let mut big_pages = vec![2];
        stamp(2, 2, pager.write_page(2).unwrap());
        for _ in 0..47 {
            let pgno = pager.allocate_raw_page().unwrap();
            stamp(pgno, 2, pager.write_page(pgno).unwrap());
            big_pages.push(pgno);
        }
        pager.commit().unwrap();
        let final_pages = pager.page_count();
        assert_eq!(final_pages, 49);

        pager.begin_write().unwrap();
        stamp(7, 3, pager.write_page(7).unwrap());
        stamp(40, 3, pager.write_page(40).unwrap());
        pager.commit().unwrap();

        // Walk `-wal` independently of the WAL-index.
        let wal_path = pager.wal_path().unwrap().to_path_buf();
        let wal_file = vfs.open(Some(&wal_path), OpenFlags::READONLY).unwrap();
        let mut hdr_buf = [0u8; WAL_HDRSIZE];
        assert_eq!(wal_file.read_at(&mut hdr_buf, 0).unwrap(), WAL_HDRSIZE);
        let wal_hdr = WalFileHeader::decode_for_recovery(&hdr_buf)
            .unwrap()
            .unwrap();
        assert_eq!(wal_hdr.page_size as usize, page_size);

        let sz_frame = (WAL_FRAME_HDRSIZE + page_size) as u64;
        let n_frames = (wal_file.file_size().unwrap() - WAL_HDRSIZE as u64) / sz_frame;
        assert_eq!(
            wal_file.file_size().unwrap(),
            WAL_HDRSIZE as u64 + n_frames * sz_frame
        );
        let mut cksum = wal_hdr.cksum;
        let mut frames: Vec<(Pgno, u32)> = Vec::new();
        let mut latest: HashMap<Pgno, Vec<u8>> = HashMap::new();
        for frame_no in 1..=(n_frames as u32) {
            let off = wal_frame_offset(frame_no, page_size as u32);
            let mut fhdr = [0u8; WAL_FRAME_HDRSIZE];
            let mut data = vec![0u8; page_size];
            assert_eq!(wal_file.read_at(&mut fhdr, off).unwrap(), WAL_FRAME_HDRSIZE);
            assert_eq!(
                wal_file
                    .read_at(&mut data, off + WAL_FRAME_HDRSIZE as u64)
                    .unwrap(),
                page_size
            );
            let (pgno, n_truncate, next) =
                wal_decode_frame(&fhdr, &data, wal_hdr.salt, wal_hdr.big_end_cksum, cksum)
                    .unwrap()
                    .unwrap_or_else(|| panic!("frame {frame_no} fails checksum/salt validation"));
            cksum = next;
            frames.push((pgno, n_truncate));
            latest.insert(pgno, data);
        }

        // Split the frame log into commits (a commit frame has `n_truncate != 0`).
        let mut commits: Vec<(Vec<Pgno>, u32)> = Vec::new();
        let mut open_commit: Vec<Pgno> = Vec::new();
        for &(pgno, n_truncate) in &frames {
            open_commit.push(pgno);
            if n_truncate != 0 {
                commits.push((std::mem::take(&mut open_commit), n_truncate));
            }
        }
        assert!(open_commit.is_empty(), "WAL must end on a commit frame");
        let big_idx = commits
            .iter()
            .position(|(pgnos, _)| pgnos.len() >= 48)
            .expect("one commit spanning several append batches");
        // (Commits may be followed by sector-padding frames that repeat the commit frame.)
        assert!(big_idx + 1 < commits.len());
        for (i, (pgnos, n_truncate)) in commits.iter().enumerate() {
            assert!(
                pgnos.windows(2).all(|w| w[0] < w[1]),
                "frames ascend by pgno"
            );
            assert_eq!(*n_truncate, if i < big_idx { 2 } else { final_pages });
        }
        let big = &commits[big_idx].0;
        for pgno in &big_pages {
            assert!(
                big.contains(pgno),
                "page {pgno} missing from the large commit"
            );
        }
        assert!(
            commits[big_idx + 1..]
                .iter()
                .any(|(pgnos, _)| pgnos.contains(&7) && pgnos.contains(&40)),
            "final two-page commit present after the large commit"
        );

        // A second connection sees every page through the WAL-index, matching the frame bytes.
        let mut reader =
            Pager::open_with_vfs(vfs.clone(), Some(db_path), OpenFlags::READWRITE).unwrap();
        reader.begin_read().unwrap();
        assert_eq!(reader.page_count(), final_pages);
        for &pgno in &big_pages {
            let round = if pgno == 7 || pgno == 40 { 3 } else { 2 };
            let mut expect = vec![0u8; page_size];
            stamp(pgno, round, &mut expect);
            let got = reader.read_page(pgno).unwrap();
            assert_eq!(&got[16..], &expect[16..], "page {pgno} content");
            assert_eq!(
                &latest[&pgno][16..],
                &expect[16..],
                "page {pgno} frame bytes"
            );
        }
        reader.end_read_if_unused().unwrap();
        reader.close().unwrap();
        pager.close().unwrap();
    }

    // ------------------------------------------------------------------------------------------
    // WAL crash atomicity while a checksum rewrite is pending (`iReCksum`, `walRewriteChecksums`)
    // ------------------------------------------------------------------------------------------

    /// One operation issued against `<db>-wal`, in issue order.
    #[derive(Clone, Debug)]
    enum WalOp {
        Write { off: u64, data: Vec<u8> },
        Truncate(u64),
        Sync,
    }

    /// `MemoryVfs` wrapper that records every write / truncate / sync issued against files whose
    /// name ends in `-wal`, reports configurable device characteristics and hands out
    /// deterministic "randomness" (so WAL salts are reproducible).
    struct RecordingVfs {
        inner: Arc<MemoryVfs>,
        log: Arc<Mutex<Vec<WalOp>>>,
        device_characteristics: u32,
        rng: Mutex<u64>,
    }

    struct RecordingFile {
        inner: Box<dyn VfsFile>,
        log: Option<Arc<Mutex<Vec<WalOp>>>>,
        device_characteristics: u32,
    }

    impl Vfs for RecordingVfs {
        fn name(&self) -> &str {
            self.inner.name()
        }
        fn lock_geometry(&self) -> LockGeometry {
            self.inner.lock_geometry()
        }
        fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
            let inner = self.inner.open(path, flags)?;
            let is_wal = path.is_some_and(|p| p.to_string_lossy().ends_with("-wal"));
            Ok(Box::new(RecordingFile {
                inner,
                log: is_wal.then(|| self.log.clone()),
                device_characteristics: self.device_characteristics,
            }))
        }
        fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
            self.inner.delete(path, sync_dir)
        }
        fn exists(&self, path: &Path) -> Result<bool> {
            self.inner.exists(path)
        }
        fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
            self.inner.full_pathname(path)
        }
        fn randomness(&self, buf: &mut [u8]) {
            let mut state = self.rng.lock().unwrap();
            for b in buf {
                *state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                *b = (*state >> 56) as u8;
            }
        }
    }

    impl VfsFile for RecordingFile {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
            self.inner.read_at(buf, offset)
        }
        fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
            self.inner.write_at(buf, offset)?;
            if let Some(log) = &self.log {
                log.lock().unwrap().push(WalOp::Write {
                    off: offset,
                    data: buf.to_vec(),
                });
            }
            Ok(())
        }
        fn truncate(&self, size: u64) -> Result<()> {
            self.inner.truncate(size)?;
            if let Some(log) = &self.log {
                log.lock().unwrap().push(WalOp::Truncate(size));
            }
            Ok(())
        }
        fn sync(&self, flags: SyncFlags) -> Result<()> {
            self.inner.sync(flags)?;
            if let Some(log) = &self.log {
                log.lock().unwrap().push(WalOp::Sync);
            }
            Ok(())
        }
        fn file_size(&self) -> Result<u64> {
            self.inner.file_size()
        }
        fn lock(&self, level: LockLevel) -> Result<()> {
            self.inner.lock(level)
        }
        fn unlock(&self, level: LockLevel) -> Result<()> {
            self.inner.unlock(level)
        }
        fn check_reserved_lock(&self) -> Result<bool> {
            self.inner.check_reserved_lock()
        }
        fn lock_level(&self) -> LockLevel {
            self.inner.lock_level()
        }
        fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
            self.inner.lock_state_for_diagnostics()
        }
        fn close(&mut self) -> Result<()> {
            self.inner.close()
        }
        fn open_wal_shm_with_options(
            &self,
            options: ShmOpenOptions,
        ) -> Result<sqliters_vfs::WalShm> {
            self.inner.open_wal_shm_with_options(options)
        }
        fn supports_wal_shm(&self) -> bool {
            self.inner.supports_wal_shm()
        }
        fn device_characteristics(&self) -> u32 {
            self.device_characteristics
        }
        fn sector_size(&self) -> u32 {
            self.inner.sector_size()
        }
        fn is_readonly(&self) -> bool {
            self.inner.is_readonly()
        }
    }

    fn vfs_file_image(vfs: &dyn Vfs, path: &Path) -> Vec<u8> {
        if !vfs.exists(path).unwrap() {
            return Vec::new();
        }
        let file = vfs.open(Some(path), OpenFlags::READONLY).unwrap();
        let mut image = vec![0u8; file.file_size().unwrap() as usize];
        if !image.is_empty() {
            assert_eq!(file.read_at(&mut image, 0).unwrap(), image.len());
        }
        image
    }

    fn apply_wal_op(image: &mut Vec<u8>, op: &WalOp) {
        match op {
            WalOp::Write { off, data } => {
                let end = *off as usize + data.len();
                if image.len() < end {
                    image.resize(end, 0);
                }
                image[*off as usize..end].copy_from_slice(data);
            }
            WalOp::Truncate(size) => image.resize(*size as usize, 0),
            WalOp::Sync => {}
        }
    }

    /// Database content as a fresh connection sees it: `(page_count, pages 1..=page_count)`.
    type DbState = (Pgno, Vec<Vec<u8>>);

    /// Runs WAL recovery over `db_image` + `wal_image` in a pristine VFS (no `-shm`) and returns
    /// the state a new connection reads.
    fn recovered_state(db_image: &[u8], wal_image: &[u8]) -> DbState {
        let vfs = Arc::new(MemoryVfs::new());
        let db_path = Path::new("crash.db");
        let put = |path: &Path, image: &[u8]| {
            let file = vfs
                .open(Some(path), OpenFlags::READWRITE | OpenFlags::CREATE)
                .unwrap();
            file.write_at(image, 0).unwrap();
        };
        put(db_path, db_image);
        if !wal_image.is_empty() {
            put(Path::new("crash.db-wal"), wal_image);
        }
        let mut pager = Pager::open_with_vfs(
            vfs.clone(),
            Some(db_path),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        pager.begin_read().unwrap();
        let n_pages = pager.page_count();
        let pages = (1..=n_pages)
            .map(|pgno| pager.read_page(pgno).unwrap().to_vec())
            .collect();
        pager.end_read_if_unused().unwrap();
        pager.close().unwrap();
        (n_pages, pages)
    }

    /// Fills a whole (non-header) page with a pattern unique to `(pgno, version)`.
    fn stamp_page(pager: &mut Pager, pgno: Pgno, version: u8) {
        let buf = pager.write_page(pgno).unwrap();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i as u8)
                .wrapping_mul(7)
                .wrapping_add((pgno as u8).wrapping_mul(31))
                ^ version.wrapping_mul(0x9d);
        }
        buf[0..4].copy_from_slice(&pgno.to_be_bytes());
        buf[4] = version;
    }

    /// Everything recorded about one write transaction run against a [`RecordingVfs`].
    struct RecordedWalTxn {
        page_size: usize,
        /// `.db` image (unchanged by the transaction: nothing is checkpointed).
        db_image: Vec<u8>,
        /// `-wal` image when the transaction began (fully durable).
        base_wal: Vec<u8>,
        /// Every `-wal` operation of the transaction, in issue order.
        ops: Vec<WalOp>,
        /// Index into `ops` of the first operation issued by COMMIT.
        commit_start: usize,
        /// `-wal` image after COMMIT returned.
        final_wal: Vec<u8>,
        /// State a new connection reads before / after the transaction.
        before: DbState,
        after: DbState,
    }

    impl RecordedWalTxn {
        fn sz_frame(&self) -> usize {
            WAL_FRAME_HDRSIZE + self.page_size
        }

        /// Number of frames in the `-wal` file when the transaction began.
        fn base_frames(&self) -> u32 {
            (self.base_wal.len().saturating_sub(WAL_HDRSIZE) / self.sz_frame()) as u32
        }

        fn frame_offset(&self, frame_no: u32) -> u64 {
            wal_frame_offset(frame_no, self.page_size as u32)
        }

        fn wal_header(&self) -> WalFileHeader {
            let hdr: [u8; WAL_HDRSIZE] = self.final_wal[..WAL_HDRSIZE].try_into().unwrap();
            WalFileHeader::decode_for_recovery(&hdr).unwrap().unwrap()
        }

        /// `(off, data)` of every write issued by COMMIT, in order.
        fn commit_writes(&self) -> Vec<(u64, &[u8])> {
            self.ops[self.commit_start..]
                .iter()
                .filter_map(|op| match op {
                    WalOp::Write { off, data } => Some((*off, data.as_slice())),
                    _ => None,
                })
                .collect()
        }

        /// `-wal` image after the first `n_ops` operations of the transaction reached the file.
        fn image_after(&self, n_ops: usize) -> Vec<u8> {
            let mut image = self.base_wal.clone();
            for op in &self.ops[..n_ops] {
                apply_wal_op(&mut image, op);
            }
            image
        }

        /// Number of leading frames of `image` that validate against the salt and the checksum
        /// chain, and the commit size carried by the last valid frame.
        fn valid_chain(&self, image: &[u8]) -> (u32, u32) {
            let wal_hdr = self.wal_header();
            let mut cksum = wal_hdr.cksum;
            let mut valid = 0u32;
            let mut last_n_truncate = 0u32;
            let n_frames = (image.len().saturating_sub(WAL_HDRSIZE) / self.sz_frame()) as u32;
            for frame_no in 1..=n_frames {
                let off = self.frame_offset(frame_no) as usize;
                let fhdr: [u8; WAL_FRAME_HDRSIZE] =
                    image[off..off + WAL_FRAME_HDRSIZE].try_into().unwrap();
                let data = &image[off + WAL_FRAME_HDRSIZE..off + self.sz_frame()];
                let Some((_, n_truncate, next)) =
                    wal_decode_frame(&fhdr, data, wal_hdr.salt, wal_hdr.big_end_cksum, cksum)
                        .unwrap()
                else {
                    break;
                };
                cksum = next;
                valid = frame_no;
                last_n_truncate = n_truncate;
            }
            (valid, last_n_truncate)
        }

        /// Crash model: everything issued before a sync is durable; of the writes issued since
        /// the last sync ANY subset may have reached the disk (unordered writeback). Recovers
        /// every such image and requires the pre-transaction state or the complete transaction.
        /// Returns `(images recovered to "before", images recovered to "after")`.
        fn assert_every_crash_state_is_atomic(&self) -> (usize, usize) {
            let mut durable = self.base_wal.clone();
            let mut epoch: Vec<(usize, &WalOp)> = Vec::new();
            let (mut n_before, mut n_after) = (0usize, 0usize);
            let end_marker = WalOp::Sync;
            let numbered = self.ops.iter().enumerate();
            for (op_idx, op) in numbered.chain(std::iter::once((self.ops.len(), &end_marker))) {
                if !matches!(op, WalOp::Sync) {
                    epoch.push((op_idx, op));
                    continue;
                }
                assert!(
                    epoch.len() <= 14,
                    "scenario too large to enumerate: {} unsynced operations",
                    epoch.len()
                );
                for mask in 0u32..(1u32 << epoch.len()) {
                    let mut image = durable.clone();
                    let mut kept = Vec::new();
                    for (bit, (idx, pending)) in epoch.iter().enumerate() {
                        if mask & (1 << bit) != 0 {
                            apply_wal_op(&mut image, pending);
                            kept.push(*idx);
                        }
                    }
                    let got = recovered_state(&self.db_image, &image);
                    if got == self.before {
                        n_before += 1;
                    } else if got == self.after {
                        n_after += 1;
                    } else {
                        let lost: Vec<usize> = epoch
                            .iter()
                            .map(|(idx, _)| *idx)
                            .filter(|idx| !kept.contains(idx))
                            .collect();
                        panic!(
                            "HALF-APPLIED transaction after a crash before the sync at op \
                             {op_idx}: -wal operations {kept:?} reached the disk, {lost:?} were \
                             lost (COMMIT starts at op {}); recovered {} pages, valid chain {:?}",
                            self.commit_start,
                            got.0,
                            self.valid_chain(&image),
                        );
                    }
                }
                for (_, pending) in epoch.drain(..) {
                    apply_wal_op(&mut durable, pending);
                }
            }
            assert_eq!(
                durable, self.final_wal,
                "recorder replays to the final -wal"
            );
            (n_before, n_after)
        }
    }

    /// Builds a `n_pages`-page database in WAL mode on a [`RecordingVfs`], optionally commits two
    /// WAL transactions first (so the transaction under test does not start the log), then runs
    /// `body` inside one write transaction and commits it, recording every `-wal` operation.
    fn record_wal_txn(
        device_characteristics: u32,
        synchronous: u8,
        prior_wal_commits: bool,
        body: impl FnOnce(&mut Pager),
    ) -> RecordedWalTxn {
        let inner = Arc::new(MemoryVfs::new());
        let log = Arc::new(Mutex::new(Vec::new()));
        let vfs = Arc::new(RecordingVfs {
            inner: inner.clone(),
            log: log.clone(),
            device_characteristics,
            rng: Mutex::new(0x5eed_1234_abcd_ef01),
        });
        let db_path = Path::new("rewrite.db");
        let mut pager = Pager::open_with_vfs(
            vfs,
            Some(db_path),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        pager.set_synchronous(synchronous);
        pager.begin_write().unwrap();
        while pager.page_count() < 8 {
            let pgno = pager.allocate_raw_page().unwrap();
            stamp_page(&mut pager, pgno, 0);
        }
        pager.commit().unwrap();
        assert_eq!(
            pager.set_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );
        if prior_wal_commits {
            pager.begin_write().unwrap();
            stamp_page(&mut pager, 2, 1);
            stamp_page(&mut pager, 7, 1);
            pager.commit().unwrap();
            pager.begin_write().unwrap();
            stamp_page(&mut pager, 5, 1);
            pager.commit().unwrap();
        }
        let page_size = pager.page_size() as usize;
        let wal_path = pager.wal_path().unwrap().to_path_buf();
        let db_image = vfs_file_image(inner.as_ref(), db_path);
        let base_wal = vfs_file_image(inner.as_ref(), &wal_path);
        let before = recovered_state(&db_image, &base_wal);

        log.lock().unwrap().clear();
        pager.begin_write().unwrap();
        // A one-page cache: every statement boundary with two or more dirty pages spills.
        pager.set_cache_size(1);
        body(&mut pager);
        let commit_start = log.lock().unwrap().len();
        pager.commit().unwrap();
        let ops = log.lock().unwrap().clone();

        let final_wal = vfs_file_image(inner.as_ref(), &wal_path);
        assert_eq!(
            vfs_file_image(inner.as_ref(), db_path),
            db_image,
            "nothing is checkpointed during the transaction"
        );
        let after = recovered_state(&db_image, &final_wal);
        assert_ne!(before, after);
        pager.close().unwrap();
        RecordedWalTxn {
            page_size,
            db_image,
            base_wal,
            ops,
            commit_start,
            final_wal,
            before,
            after,
        }
    }

    /// The W1 situation: pages 2,3,4 are spilled, pages 3 and 4 are modified again, and COMMIT
    /// overwrites their frames in place and appends two more frames (the last is the commit
    /// frame). As in C (`walEncodeFrame` with `iReCksum != 0`), the appended frames must reach
    /// the file with ZERO salt and ZERO checksum and become valid only through the header
    /// rewrites of `walRewriteChecksums`; every subset of lost / kept unsynced writes must
    /// recover to the pre-transaction state or to the complete transaction.
    #[test]
    fn test_wal_commit_overwriting_spilled_frames_is_crash_atomic_and_c_identical() {
        let run = record_wal_txn(sqliters_vfs::IOCAP_POWERSAFE_OVERWRITE, 2, true, |pager| {
            stamp_page(pager, 2, 2);
            stamp_page(pager, 3, 2);
            stamp_page(pager, 4, 2);
            assert_eq!(pager.spill_if_over_cache_size().unwrap(), 3);
            stamp_page(pager, 3, 3);
            stamp_page(pager, 4, 3);
            stamp_page(pager, 6, 3);
            stamp_page(pager, 8, 3);
        });
        // Crash safety first: all 2^10 subsets of the transaction's 10 unsynced writes (plus
        // the state after the final sync) recover to "before" or "after".
        let (n_before, n_after) = run.assert_every_crash_state_is_atomic();
        assert_eq!(n_before + n_after, (1 << 10) + 1);
        assert!(n_before > 0 && n_after > 0);

        // Then the bytes, write by write, against what C puts on disk.
        let ps = run.page_size;
        let szf = run.sz_frame();
        let m = run.base_frames();
        assert!(m >= 3, "two earlier WAL commits precede the transaction");
        let wal_hdr = run.wal_header();
        let salt_bytes: Vec<u8> = wal_hdr.salt.iter().flat_map(|s| s.to_be_bytes()).collect();

        // Spill: three appended frames with the real salt and a valid (uncommitted) chain.
        assert_eq!(run.commit_start, 3);
        let spilled = run.image_after(run.commit_start);
        assert_eq!(run.valid_chain(&spilled), (m + 3, 0));

        // COMMIT, write by write (C: `walFrames` loop, then `walRewriteChecksums`).
        let writes = run.commit_writes();
        assert_eq!(
            writes.len(),
            7,
            "2 in-place + 1 append run + 4 header rewrites"
        );
        // 1-2: page images of pages 3 and 4 over frames m+2 and m+3 (frame headers untouched).
        for (i, frame_no) in [m + 2, m + 3].into_iter().enumerate() {
            let (off, data) = writes[i];
            assert_eq!(off, run.frame_offset(frame_no) + WAL_FRAME_HDRSIZE as u64);
            assert_eq!(data.len(), ps);
            assert_eq!(&data[0..4], &(3 + i as u32).to_be_bytes());
        }
        // 3: frames m+4 (page 6) and m+5 (page 8, commit) in one positioned write, both with
        //    salt and checksum zeroed exactly as `memset(&aFrame[8], 0, 16)` leaves them.
        let (off, data) = writes[2];
        assert_eq!(off, run.frame_offset(m + 4));
        assert_eq!(data.len(), 2 * szf);
        for (i, (pgno, n_truncate)) in [(6u32, 0u32), (8, 8)].into_iter().enumerate() {
            let fhdr = &data[i * szf..i * szf + WAL_FRAME_HDRSIZE];
            assert_eq!(&fhdr[0..4], &pgno.to_be_bytes());
            assert_eq!(&fhdr[4..8], &n_truncate.to_be_bytes());
            assert_eq!(
                &fhdr[8..24],
                &[0u8; 16],
                "frame appended while a checksum rewrite is pending must not carry a salt or \
                 checksum (C writes zeroes)"
            );
        }
        // Before `walRewriteChecksums` the log holds no valid frame past the first overwritten
        // one, whatever happened to the in-place writes.
        let pre_rewrite = run.image_after(run.commit_start + 3);
        assert_eq!(run.valid_chain(&pre_rewrite), (m + 1, 0));
        // 4-7: one 24-byte header per frame m+2 ..= m+5, ascending, now with the real salt.
        for (i, frame_no) in (m + 2..=m + 5).enumerate() {
            let (off, data) = writes[3 + i];
            assert_eq!(off, run.frame_offset(frame_no));
            assert_eq!(data.len(), WAL_FRAME_HDRSIZE);
            assert_eq!(&data[8..16], salt_bytes.as_slice());
        }
        assert!(
            matches!(run.ops.last(), Some(WalOp::Sync)),
            "synchronous=FULL syncs the log after the rewrite"
        );
        assert_eq!(run.valid_chain(&run.final_wal), (m + 5, 8));

        // The W1 hole: spilled frames and the appended commit frame durable, both in-place
        // overwrites (and the header rewrites) lost.
        let mut hole = spilled.clone();
        apply_wal_op(&mut hole, &run.ops[run.commit_start + 2]);
        assert_eq!(
            run.valid_chain(&hole),
            (m + 3, 0),
            "the appended commit frame must not validate over the stale spilled frames"
        );
        assert!(
            recovered_state(&run.db_image, &hole) == run.before,
            "commit frame durable + in-place overwrites lost must discard the transaction"
        );
    }

    /// The overwrite happens in a later SPILL (not at COMMIT): `iReCksum` stays set, so the
    /// frames appended by that spill and by COMMIT are all written unchained (zero salt and
    /// checksum) until COMMIT rewrites the checksums.
    #[test]
    fn test_wal_spill_overwriting_spilled_frame_is_crash_atomic() {
        for synchronous in [1u8, 2] {
            let run = record_wal_txn(
                sqliters_vfs::IOCAP_POWERSAFE_OVERWRITE,
                synchronous,
                true,
                |pager| {
                    stamp_page(pager, 2, 2);
                    stamp_page(pager, 3, 2);
                    stamp_page(pager, 4, 2);
                    assert_eq!(pager.spill_if_over_cache_size().unwrap(), 3);
                    stamp_page(pager, 3, 3);
                    stamp_page(pager, 6, 3);
                    assert_eq!(pager.spill_if_over_cache_size().unwrap(), 2);
                    stamp_page(pager, 8, 3);
                },
            );
            let (n_before, n_after) = run.assert_every_crash_state_is_atomic();
            assert!(n_before > 0 && n_after > 0);

            let m = run.base_frames();
            // Second spill: page 3 over frame m+2 in place, then page 6 appended unchained.
            let WalOp::Write { off, data } = &run.ops[3] else {
                panic!("in-place overwrite expected")
            };
            assert_eq!(*off, run.frame_offset(m + 2) + WAL_FRAME_HDRSIZE as u64);
            assert_eq!(data.len(), run.page_size);
            let WalOp::Write { off, data } = &run.ops[4] else {
                panic!("appended spill frame expected")
            };
            assert_eq!(*off, run.frame_offset(m + 4));
            assert_eq!(&data[0..4], &6u32.to_be_bytes());
            assert_eq!(&data[4..24], &[0u8; 20], "spill frame written unchained");
            assert_eq!(run.commit_start, 5);
            // COMMIT: page 8 appended as an unchained commit frame, then 4 header rewrites.
            let writes = run.commit_writes();
            assert_eq!(writes.len(), 5);
            assert_eq!(writes[0].0, run.frame_offset(m + 5));
            assert_eq!(&writes[0].1[0..4], &8u32.to_be_bytes());
            assert_eq!(&writes[0].1[4..8], &8u32.to_be_bytes());
            assert_eq!(&writes[0].1[8..24], &[0u8; 16]);
            let pre_rewrite = run.image_after(run.commit_start + 1);
            assert_eq!(run.valid_chain(&pre_rewrite), (m + 1, 0));
            assert_eq!(run.valid_chain(&run.final_wal), (m + 5, 8));
        }
    }

    /// The transaction starts the log (WAL header written and synced inside it) and overwrites
    /// frame 1, so the rewrite restarts from the WAL-header checksum (`iReCksum == 1`); the
    /// device has no power-safe overwrite, so COMMIT also appends a sector-padding copy of the
    /// commit frame after the rewrite, split around the sync point.
    #[test]
    fn test_wal_overwrite_of_frame_one_with_sector_padding_is_crash_atomic() {
        let run = record_wal_txn(0, 2, false, |pager| {
            stamp_page(pager, 2, 2);
            stamp_page(pager, 3, 2);
            assert_eq!(pager.spill_if_over_cache_size().unwrap(), 2);
            stamp_page(pager, 2, 3);
            stamp_page(pager, 5, 3);
            assert_eq!(pager.spill_if_over_cache_size().unwrap(), 2);
            stamp_page(pager, 8, 3);
        });
        let (n_before, n_after) = run.assert_every_crash_state_is_atomic();
        assert!(n_before > 0 && n_after > 0);

        assert_eq!(run.base_frames(), 0);
        assert!(
            matches!(run.ops[0], WalOp::Write { off: 0, ref data } if data.len() == WAL_HDRSIZE)
        );
        assert!(matches!(run.ops[1], WalOp::Sync), "WAL header is synced");
        // Second spill: frame 1 (page 2) overwritten in place, page 5 appended unchained.
        let WalOp::Write { off, data } = &run.ops[4] else {
            panic!("in-place overwrite expected")
        };
        assert_eq!(*off, run.frame_offset(1) + WAL_FRAME_HDRSIZE as u64);
        assert_eq!(data.len(), run.page_size);
        let WalOp::Write { off, data } = &run.ops[5] else {
            panic!("appended spill frame expected")
        };
        assert_eq!(*off, run.frame_offset(3));
        assert_eq!(&data[4..24], &[0u8; 20]);
        assert_eq!(run.commit_start, 6);
        // COMMIT appends page 8 as an unchained commit frame at frame 4 ...
        let writes = run.commit_writes();
        assert_eq!(writes[0].0, run.frame_offset(4));
        assert_eq!(writes[0].1.len(), run.sz_frame());
        assert_eq!(&writes[0].1[0..4], &8u32.to_be_bytes());
        assert_eq!(&writes[0].1[4..8], &8u32.to_be_bytes());
        assert_eq!(&writes[0].1[8..24], &[0u8; 16]);
        let pre_rewrite = run.image_after(run.commit_start + 1);
        assert_eq!(run.valid_chain(&pre_rewrite), (0, 0));
        // ... rewrites the headers of frames 1..=4 starting from the WAL-header checksum ...
        for (i, frame_no) in (1u32..=4).enumerate() {
            assert_eq!(writes[1 + i].0, run.frame_offset(frame_no));
            assert_eq!(writes[1 + i].1.len(), WAL_FRAME_HDRSIZE);
        }
        let rewritten = run.image_after(run.commit_start + 5);
        assert_eq!(run.valid_chain(&rewritten), (4, 8));
        // ... and pads to the sector boundary with valid copies of the commit frame.
        let (valid, n_truncate) = run.valid_chain(&run.final_wal);
        assert!(valid > 4, "sector padding frame(s) follow the commit frame");
        assert_eq!(n_truncate, 8);
        assert_eq!(
            run.final_wal.len(),
            WAL_HDRSIZE + valid as usize * run.sz_frame()
        );
    }

    /// A savepoint is opened while a checksum rewrite is pending, frames are spilled under it
    /// and rolled back (`sqlite3WalSavepointUndo` keeps `iReCksum` because it is not above the
    /// restored `mxFrame`), and COMMIT reuses their slots: the frames stay unchained until the
    /// rewrite and the final log is one valid chain.
    #[test]
    fn test_wal_savepoint_rollback_under_pending_rewrite_is_crash_atomic() {
        let run = record_wal_txn(sqliters_vfs::IOCAP_POWERSAFE_OVERWRITE, 2, true, |pager| {
            stamp_page(pager, 2, 2);
            stamp_page(pager, 3, 2);
            assert_eq!(pager.spill_if_over_cache_size().unwrap(), 2);
            stamp_page(pager, 3, 3);
            stamp_page(pager, 6, 3);
            assert_eq!(pager.spill_if_over_cache_size().unwrap(), 2);
            pager.ensure_subsavepoints(1).unwrap();
            stamp_page(pager, 4, 9);
            stamp_page(pager, 7, 9);
            assert_eq!(pager.spill_if_over_cache_size().unwrap(), 2);
            pager.rollback_to_subsavepoint(0).unwrap();
            pager.release_subsavepoint(0).unwrap();
            stamp_page(pager, 8, 3);
        });
        let (n_before, n_after) = run.assert_every_crash_state_is_atomic();
        assert!(n_before > 0 && n_after > 0);

        let m = run.base_frames();
        // Spills: m+1 (page 2), m+2 (page 3); page 3 in place over m+2, m+3 (page 6) unchained;
        // under the savepoint m+4 (page 4) and m+5 (page 7), both unchained.
        assert_eq!(run.commit_start, 6);
        for (op, frame_no) in [(3usize, m + 3), (4, m + 4), (5, m + 5)] {
            let WalOp::Write { off, data } = &run.ops[op] else {
                panic!("appended spill frame expected")
            };
            assert_eq!(*off, run.frame_offset(frame_no));
            assert_eq!(
                &data[4..24],
                &[0u8; 20],
                "frame {frame_no} written unchained"
            );
        }
        // COMMIT appends from frame m+4 again (the savepoint rollback dropped m+4 and m+5),
        // every frame unchained, the last one carrying the commit size.
        let writes = run.commit_writes();
        let (off, data) = writes[0];
        assert_eq!(off, run.frame_offset(m + 4));
        let n_appended = data.len() / run.sz_frame();
        assert_eq!(data.len(), n_appended * run.sz_frame());
        for i in 0..n_appended {
            let fhdr = &data[i * run.sz_frame()..i * run.sz_frame() + WAL_FRAME_HDRSIZE];
            let n_truncate = if i + 1 == n_appended { 8u32 } else { 0 };
            assert_eq!(&fhdr[4..8], &n_truncate.to_be_bytes());
            assert_eq!(&fhdr[8..24], &[0u8; 16]);
        }
        let last = m + 3 + n_appended as u32;
        let pre_rewrite = run.image_after(run.commit_start + 1);
        assert_eq!(run.valid_chain(&pre_rewrite), (m + 1, 0));
        assert_eq!(writes.len(), 1 + (last - (m + 1)) as usize);
        for (i, frame_no) in (m + 2..=last).enumerate() {
            assert_eq!(writes[1 + i].0, run.frame_offset(frame_no));
            assert_eq!(writes[1 + i].1.len(), WAL_FRAME_HDRSIZE);
        }
        assert_eq!(run.valid_chain(&run.final_wal), (last, 8));
        // The rolled-back images of pages 4 and 7 are not part of the committed state.
        assert_eq!(run.after.1[3], run.before.1[3]);
        assert_eq!(run.after.1[6], run.before.1[6]);
    }

    /// A database without a table carries 0 for the schema format (offset 44) and the text
    /// encoding (offset 56), exactly like a file C has only run `PRAGMA user_version`, an empty
    /// transaction, `journal_mode=WAL` or `VACUUM` on, and its encoding can still be chosen —
    /// by this pager or, after a reopen, by another one. The first table stamps both fields
    /// (`sqlite3StartTable`), after which the encoding is final.
    #[test]
    fn test_header_of_table_less_database_keeps_format_and_encoding_unset() {
        fn header_fields(vfs: &MemoryVfs, path: &Path) -> (u32, u32, i32) {
            let file = vfs.open(Some(path), OpenFlags::READONLY).unwrap();
            let mut hdr = [0u8; DB_HEADER_SIZE];
            assert_eq!(file.read_at(&mut hdr, 0).unwrap(), DB_HEADER_SIZE);
            let u = |off: usize| u32::from_be_bytes(hdr[off..off + 4].try_into().unwrap());
            (u(44), u(56), u(60) as i32)
        }
        let vfs = Arc::new(MemoryVfs::new());
        let path = Path::new("tableless.db");
        let flags = OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB;

        let mut pager = Pager::open_with_vfs(vfs.clone(), Some(path), flags).unwrap();
        pager.begin_write().unwrap();
        pager.header_mut().unwrap().user_version = 5;
        pager.commit().unwrap();
        assert_eq!(header_fields(&vfs, path), (0, 0, 5));
        assert_eq!(pager.encoding(), TextEncoding::Utf8);
        assert!(!pager.header().file_format_is_set());

        // The choice survives the header reloads at the start of later transactions and is
        // not written before a table exists.
        pager.set_encoding(TextEncoding::Utf16Be).unwrap();
        assert_eq!(pager.encoding(), TextEncoding::Utf16Be);
        pager.begin_write().unwrap();
        pager.header_mut().unwrap().user_version = 6;
        pager.commit().unwrap();
        pager.begin_read().unwrap();
        assert_eq!(pager.encoding(), TextEncoding::Utf16Be);
        pager.end_read_if_unused().unwrap();
        assert_eq!(header_fields(&vfs, path), (0, 0, 6));
        pager.close().unwrap();

        // Another connection sees an undecided file and may decide differently.
        let mut pager = Pager::open_with_vfs(vfs.clone(), Some(path), flags).unwrap();
        assert_eq!(pager.effective_encoding().unwrap(), TextEncoding::Utf8);
        pager.set_encoding(TextEncoding::Utf16Le).unwrap();
        pager.begin_write().unwrap();
        pager
            .header_mut()
            .unwrap()
            .set_file_format_for_first_table();
        pager.commit().unwrap();
        assert_eq!(header_fields(&vfs, path), (4, 2, 6));
        // From here on the encoding is fixed, for this pager and for new ones.
        pager.set_encoding(TextEncoding::Utf8).unwrap();
        assert_eq!(pager.encoding(), TextEncoding::Utf16Le);
        pager.close().unwrap();
        let mut pager = Pager::open_with_vfs(vfs.clone(), Some(path), flags).unwrap();
        pager.set_encoding(TextEncoding::Utf16Be).unwrap();
        assert_eq!(pager.effective_encoding().unwrap(), TextEncoding::Utf16Le);
        assert!(pager.header().file_format_is_set());
    }

    // ------------------------------------------------------------------------------------------
    // WAL intermediate states under I/O faults (`walLimitSize`, close-time checkpoint)
    // ------------------------------------------------------------------------------------------

    /// What a [`FaultVfs`] injects and observes.
    #[derive(Default)]
    struct FaultCtl {
        /// `xFileSize` of `<db>-wal` fails.
        fail_wal_file_size: std::sync::atomic::AtomicBool,
        /// `xTruncate` of `<db>-wal` fails.
        fail_wal_truncate: std::sync::atomic::AtomicBool,
        /// Ordered trace of the operations that matter for durability.
        events: Mutex<Vec<&'static str>>,
    }

    impl FaultCtl {
        fn take_events(&self) -> Vec<&'static str> {
            std::mem::take(&mut *self.events.lock().unwrap())
        }
    }

    /// `MemoryVfs` wrapper that can fail `file_size` / `truncate` on `-wal` files and records
    /// writes and syncs of the database and the `-wal`, and the removal of the `-wal`.
    struct FaultVfs {
        inner: Arc<MemoryVfs>,
        ctl: Arc<FaultCtl>,
    }

    struct FaultFile {
        inner: Box<dyn VfsFile>,
        ctl: Arc<FaultCtl>,
        is_wal: bool,
        is_db: bool,
    }

    impl Vfs for FaultVfs {
        fn name(&self) -> &str {
            self.inner.name()
        }
        fn lock_geometry(&self) -> LockGeometry {
            self.inner.lock_geometry()
        }
        fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
            let inner = self.inner.open(path, flags)?;
            let name = path
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Box::new(FaultFile {
                inner,
                ctl: self.ctl.clone(),
                is_wal: name.ends_with("-wal"),
                is_db: name.ends_with(".db"),
            }))
        }
        fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
            if path.to_string_lossy().ends_with("-wal") {
                self.ctl.events.lock().unwrap().push("wal-delete");
            }
            self.inner.delete(path, sync_dir)
        }
        fn exists(&self, path: &Path) -> Result<bool> {
            self.inner.exists(path)
        }
        fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
            self.inner.full_pathname(path)
        }
        fn randomness(&self, buf: &mut [u8]) {
            self.inner.randomness(buf);
        }
    }

    impl VfsFile for FaultFile {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
            self.inner.read_at(buf, offset)
        }
        fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
            if self.is_db {
                self.ctl.events.lock().unwrap().push("db-write");
            }
            self.inner.write_at(buf, offset)
        }
        fn truncate(&self, size: u64) -> Result<()> {
            if self.is_wal
                && self
                    .ctl
                    .fail_wal_truncate
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(SqliteError::new(
                    ErrorCode::IoErr,
                    "injected truncate failure",
                ));
            }
            self.inner.truncate(size)
        }
        fn sync(&self, flags: SyncFlags) -> Result<()> {
            if self.is_db {
                self.ctl.events.lock().unwrap().push("db-sync");
            } else if self.is_wal {
                self.ctl.events.lock().unwrap().push("wal-sync");
            }
            self.inner.sync(flags)
        }
        fn file_size(&self) -> Result<u64> {
            if self.is_wal
                && self
                    .ctl
                    .fail_wal_file_size
                    .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(SqliteError::new(ErrorCode::IoErr, "injected fstat failure"));
            }
            self.inner.file_size()
        }
        fn lock(&self, level: LockLevel) -> Result<()> {
            self.inner.lock(level)
        }
        fn unlock(&self, level: LockLevel) -> Result<()> {
            self.inner.unlock(level)
        }
        fn check_reserved_lock(&self) -> Result<bool> {
            self.inner.check_reserved_lock()
        }
        fn lock_level(&self) -> LockLevel {
            self.inner.lock_level()
        }
        fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
            self.inner.lock_state_for_diagnostics()
        }
        fn close(&mut self) -> Result<()> {
            self.inner.close()
        }
        fn open_wal_shm_with_options(
            &self,
            options: ShmOpenOptions,
        ) -> Result<sqliters_vfs::WalShm> {
            self.inner.open_wal_shm_with_options(options)
        }
        fn supports_wal_shm(&self) -> bool {
            self.inner.supports_wal_shm()
        }
        fn device_characteristics(&self) -> u32 {
            self.inner.device_characteristics()
        }
        fn sector_size(&self) -> u32 {
            self.inner.sector_size()
        }
        fn is_readonly(&self) -> bool {
            self.inner.is_readonly()
        }
    }

    /// An 8-page database in WAL mode on a [`FaultVfs`], with `synchronous = FULL`.
    fn open_wal_db_on_fault_vfs(db_path: &Path) -> (Arc<MemoryVfs>, Arc<FaultCtl>, Pager) {
        let inner = Arc::new(MemoryVfs::new());
        let ctl = Arc::new(FaultCtl::default());
        let vfs = Arc::new(FaultVfs {
            inner: inner.clone(),
            ctl: ctl.clone(),
        });
        let mut pager = Pager::open_with_vfs(
            vfs,
            Some(db_path),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        pager.set_synchronous(2);
        pager.begin_write().unwrap();
        while pager.page_count() < 8 {
            let pgno = pager.allocate_raw_page().unwrap();
            stamp_page(&mut pager, pgno, 0);
        }
        pager.commit().unwrap();
        assert_eq!(
            pager.set_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );
        (inner, ctl, pager)
    }

    /// `journal_size_limit` is applied by the first commit after the log was reset
    /// (`sqlite3WalFrames`, `wal.c:4306-4313`) through `walLimitSize`, which IGNORES a failing
    /// `xFileSize` / `xTruncate` ("If an error occurs while doing so, ignore it"). At that point
    /// the commit frame is already written and synced: reporting the failure would make COMMIT
    /// fail and roll the connection back while the `-wal` holds a valid, durable commit that
    /// the next recovery brings back.
    #[test]
    fn test_wal_size_limit_failure_does_not_fail_a_durable_commit() {
        for fault in ["none", "file_size", "truncate"] {
            let db_path = Path::new("limit.db");
            let (inner, ctl, mut pager) = open_wal_db_on_fault_vfs(db_path);
            let page_size = pager.page_size() as u64;
            let wal_path = pager.wal_path().unwrap().to_path_buf();
            let frame = page_size + WAL_FRAME_HDRSIZE as u64;
            // Grow the log to at least seven frames, then checkpoint everything so that the
            // next writer restarts it from frame 1.
            pager.begin_write().unwrap();
            for pgno in 2..=8 {
                stamp_page(&mut pager, pgno, 1);
            }
            pager.commit().unwrap();
            let big_len = vfs_file_image(inner.as_ref(), &wal_path).len() as u64;
            let big_frames = (big_len - WAL_HDRSIZE as u64) / frame;
            assert!(big_frames >= 7 && big_len == WAL_HDRSIZE as u64 + big_frames * frame);
            let ckpt = pager
                .wal_checkpoint(WalCheckpointMode::Full, None, None)
                .unwrap();
            assert_eq!(
                (i64::from(ckpt.n_log), i64::from(ckpt.n_ckpt)),
                (big_frames as i64, big_frames as i64)
            );
            // A limit smaller than one frame: the commit below must cut the file down to the
            // two frames it writes.
            assert_eq!(pager.set_journal_size_limit(1000), 1000);

            match fault {
                "file_size" => ctl
                    .fail_wal_file_size
                    .store(true, std::sync::atomic::Ordering::Relaxed),
                "truncate" => ctl
                    .fail_wal_truncate
                    .store(true, std::sync::atomic::Ordering::Relaxed),
                _ => {}
            }
            pager.begin_write().unwrap();
            stamp_page(&mut pager, 3, 2);
            stamp_page(&mut pager, 6, 2);
            let commit = pager.commit();
            ctl.fail_wal_file_size
                .store(false, std::sync::atomic::Ordering::Relaxed);
            ctl.fail_wal_truncate
                .store(false, std::sync::atomic::Ordering::Relaxed);
            assert!(
                commit.is_ok(),
                "{fault}: COMMIT failed after its frames were made durable: {:?}",
                commit.err().map(|e| e.message)
            );

            // The connection and a recovery of what is on disk agree: the transaction is in.
            let mut expect3 = vec![0u8; page_size as usize];
            let mut expect6 = vec![0u8; page_size as usize];
            pager.begin_read().unwrap();
            expect3.copy_from_slice(pager.read_page(3).unwrap());
            expect6.copy_from_slice(pager.read_page(6).unwrap());
            pager.end_read_if_unused().unwrap();
            assert_eq!((expect3[4], expect6[4]), (2, 2), "{fault}");
            let wal_image = vfs_file_image(inner.as_ref(), &wal_path);
            let (n_pages, pages) =
                recovered_state(&vfs_file_image(inner.as_ref(), db_path), &wal_image);
            assert_eq!(n_pages, 8, "{fault}");
            assert_eq!(pages[2], expect3, "{fault}: page 3 after recovery");
            assert_eq!(pages[5], expect6, "{fault}: page 6 after recovery");

            // Only the size differs: with the fault the log keeps its old tail.
            let new_len = wal_image.len() as u64;
            if fault == "none" {
                let new_frames = (new_len - WAL_HDRSIZE as u64) / frame;
                assert!(
                    (2..=3).contains(&new_frames)
                        && new_len == WAL_HDRSIZE as u64 + new_frames * frame,
                    "the log is cut down to the frames of the commit, got {new_len} bytes"
                );
            } else {
                assert_eq!(new_len, big_len, "{fault}");
            }

            // Later transactions work, and the limit is honoured again once the fault is gone.
            pager.begin_write().unwrap();
            stamp_page(&mut pager, 4, 3);
            pager.commit().unwrap();
            pager.close().unwrap();
        }
    }

    /// `sqlite3_deserialize` replaces the database of a connection: C closes the old b-tree the
    /// normal way (`attach.c` `REOPEN_AS_MEMDB` → `sqlite3BtreeClose` → `sqlite3WalClose` with
    /// the pager's sync flags), so the close-time checkpoint syncs the log before it copies
    /// frames into the database file and syncs the database file before the `-wal` is removed.
    /// Without the syncs a power failure after the unlink can leave a database file that has
    /// neither the frames nor a log to recover them from.
    #[test]
    fn test_deserialize_closes_the_wal_with_a_synced_checkpoint() {
        let db_path = Path::new("deser.db");
        let (inner, ctl, mut pager) = open_wal_db_on_fault_vfs(db_path);
        let wal_path = pager.wal_path().unwrap().to_path_buf();
        pager.begin_write().unwrap();
        stamp_page(&mut pager, 2, 7);
        stamp_page(&mut pager, 5, 7);
        pager.commit().unwrap();
        assert!(inner.exists(&wal_path).unwrap());
        let db_before = vfs_file_image(inner.as_ref(), db_path);
        ctl.take_events();

        // Any image will do; an empty one makes the connection an empty in-memory database.
        pager.load_deserialized_image(&[], false).unwrap();

        let events = ctl.take_events();
        let db_after = vfs_file_image(inner.as_ref(), db_path);
        assert_ne!(
            db_before, db_after,
            "the checkpoint moved frames into the file"
        );
        assert_eq!(
            db_after[2 * 4096 - 4096 + 4],
            7,
            "page 2 is the version from the log"
        );
        assert!(!inner.exists(&wal_path).unwrap(), "the log is gone");
        let pos = |what: &str| events.iter().position(|e| *e == what);
        let last = |what: &str| events.iter().rposition(|e| *e == what);
        let (Some(first_write), Some(last_write), Some(delete)) =
            (pos("db-write"), last("db-write"), pos("wal-delete"))
        else {
            panic!("unexpected trace: {events:?}");
        };
        assert!(
            pos("wal-sync").is_some_and(|i| i < first_write),
            "the log must be synced before its frames are copied: {events:?}"
        );
        assert!(
            last("db-sync").is_some_and(|i| i > last_write && i < delete),
            "the database must be synced before the log is removed: {events:?}"
        );
    }
}
