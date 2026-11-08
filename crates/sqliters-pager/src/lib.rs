//! Rollback journal (`pager.c`), page cache (`pcache1.c`), transactions, and savepoints.

pub mod header;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqliters_types::{
    Bitvec, ErrorCode, Pgno, Result, SQLITE_DEFAULT_PAGE_SIZE, SQLITE_VERSION_NUMBER, SqliteError,
    TextEncoding, extended,
};
use sqliters_vfs::{LockLevel, MemoryVfs, OpenFlags, OsVfs, SyncFlags, Vfs, VfsFile};

pub use header::{DB_HEADER_SIZE, DatabaseHeader};

/// Magic 8-byte string at the start of an SQLite rollback journal (`aJournalMagic` in `pager.c`).
pub const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

/// Minimum size of the SQLite rollback journal header (`MIN_SECTOR_SIZE` in `pager.c`; core fields occupy 28 bytes).
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

#[derive(Debug, Clone)]
struct SavepointFrame {
    name: String,
    orig_page_count: Pgno,
    orig_header: DatabaseHeader,
    /// Pre-images of pages modified since this savepoint started (`None` if allocated after savepoint).
    saved_pages: HashMap<Pgno, Vec<u8>>,
}

struct CachedPage {
    data: Vec<u8>,
    dirty: bool,
}

/// Manages page-level reads, writes, caching, rollback journaling, crash recovery, and savepoints.
pub struct Pager {
    vfs: Arc<dyn Vfs>,
    db_path: Option<PathBuf>,
    journal_path: Option<PathBuf>,
    wal_path: Option<PathBuf>,
    file: Box<dyn VfsFile>,
    journal_file: Option<Box<dyn VfsFile>>,
    in_memory: bool,
    readonly: bool,
    synchronous: u8,

    header: DatabaseHeader,
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
    journal_nonce: u32,
    journal_sector_size: u32,
    /// True once `commit()` has started writing/truncating the main database file (`PAGER_WRITER_DBMOD`).
    db_file_modified: bool,

    savepoints: Vec<SavepointFrame>,
}

impl Pager {
    /// Opens an OS-backed database file using [`OsVfs`].
    pub fn open_file(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Self> {
        let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        Self::open_with_vfs(vfs, Some(path.as_ref()), flags)
    }

    /// Creates an isolated in-memory database using [`MemoryVfs`].
    pub fn open_in_memory() -> Result<Self> {
        let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
        Self::open_with_vfs(
            vfs,
            None,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
        )
    }

    /// Opens a database on a custom [`Vfs`] implementation.
    pub fn open_with_vfs(
        vfs: Arc<dyn Vfs>,
        path: Option<&Path>,
        flags: OpenFlags,
    ) -> Result<Self> {
        let is_mem = path.is_none()
            || path == Some(Path::new(":memory:"))
            || flags.contains(OpenFlags::MEMORY);

        let full_path = match path {
            Some(p) if !is_mem => Some(vfs.full_pathname(p)?),
            _ => None,
        };
        let journal_path = full_path.as_ref().map(|p| {
            let mut os = p.as_os_str().to_os_string();
            os.push("-journal");
            PathBuf::from(os)
        });
        let wal_path = full_path.as_ref().map(|p| {
            let mut os = p.as_os_str().to_os_string();
            os.push("-wal");
            PathBuf::from(os)
        });

        let file = vfs.open(full_path.as_deref(), flags)?;
        let readonly = file.is_readonly() || flags.is_readonly();

        let mut pager = Self {
            vfs,
            db_path: full_path,
            journal_path,
            wal_path,
            file,
            journal_file: None,
            in_memory: is_mem,
            readonly,
            synchronous: 2,
            header: DatabaseHeader::new_with_page_size(SQLITE_DEFAULT_PAGE_SIZE),
            page_size_fixed: false,
            page_count: 0,
            cache: HashMap::new(),
            state: TransactionState::Open,
            tx_orig_page_count: 0,
            tx_orig_header: DatabaseHeader::default(),
            tx_journaled: Bitvec::new(64),
            tx_preimages: HashMap::new(),
            journal_records_written: 0,
            journal_nonce: 0,
            journal_sector_size: MIN_JOURNAL_HEADER_BYTES as u32,
            db_file_modified: false,
            savepoints: Vec::new(),
        };

        pager.reject_if_wal_present()?;

        // Note: Never touch or delete `<db>-journal` during `open_with_vfs` without holding
        // `SHARED` lock and verifying `check_reserved_lock()` (`pager.c:hasHotJournal`).
        let journal_exists = match pager.journal_path.as_deref() {
            Some(jp) => pager.vfs.exists(jp).unwrap_or(false),
            None => false,
        };
        match pager.load_initial_header() {
            Ok(()) => {}
            Err(e)
                if journal_exists
                    && matches!(e.code, ErrorCode::NotADb | ErrorCode::Corrupt) =>
            {
                // Defer header validation until `begin_read()` checks/replays any crashed hot journal.
            }
            Err(e) => return Err(e),
        }
        Ok(pager)
    }

    fn load_initial_header(&mut self) -> Result<()> {
        let file_len = self.file.file_size()?;
        if file_len == 0 {
            self.page_count = 0;
            self.header.database_size_pages = 0;
            return Ok(());
        }
        if file_len < DB_HEADER_SIZE as u64 {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                "file is not a database",
            ));
        }

        let mut hdr_bytes = [0u8; DB_HEADER_SIZE];
        let n = self.file.read_at(&mut hdr_bytes, 0)?;
        if n < DB_HEADER_SIZE {
            return Err(SqliteError::new(
                ErrorCode::NotADb,
                "short read on database header",
            ));
        }

        let mut hdr = DatabaseHeader::parse(&hdr_bytes)?;
        let ps = u64::from(hdr.page_size);
        let file_pages = (file_len / ps) as Pgno;

        // Per btree.c:3330: if header page count is 0 or change_counter != version_valid_for
        // (e.g. modified by pre-3.7.0 SQLite), fall back to physical file size in pages.
        let effective_pages =
            if hdr.database_size_pages == 0 || hdr.change_counter != hdr.version_valid_for {
                file_pages.max(1)
            } else if hdr.database_size_pages > file_pages && file_pages > 0 {
                return Err(SqliteError::corrupt(
                    "database header page count exceeds file size",
                ));
            } else {
                hdr.database_size_pages
            };

        hdr.database_size_pages = effective_pages;
        self.header = hdr;
        self.page_count = effective_pages;
        self.page_size_fixed = true;
        Ok(())
    }

    #[inline]
    pub const fn is_readonly(&self) -> bool {
        self.readonly
    }

    #[inline]
    pub const fn is_in_memory(&self) -> bool {
        self.in_memory
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
    pub fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
    }

    #[inline]
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    #[inline]
    pub const fn encoding(&self) -> TextEncoding {
        self.header.encoding()
    }

    /// Sets the database text encoding before the database is initialized (`PRAGMA encoding`).
    /// Per SQLite semantics, changing encoding after pages exist is a no-op.
    pub fn set_encoding(&mut self, enc: TextEncoding) -> Result<()> {
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
    pub const fn header(&self) -> &DatabaseHeader {
        &self.header
    }

    pub fn header_mut(&mut self) -> Result<&mut DatabaseHeader> {
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "cannot mutate database header outside write transaction",
            ));
        }
        if self.page_count > 0 {
            self.mark_page_writable(1)?;
        }
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
        if !DatabaseHeader::is_valid_page_size(new_size) {
            return Err(SqliteError::misuse(format!(
                "invalid page size {new_size}: must be power of two in 512..=65536"
            )));
        }
        if self.page_size_fixed && new_size != self.header.page_size {
            // In SQLite, PRAGMA page_size on an existing DB without VACUUM is a no-op.
            return Ok(());
        }
        self.header.page_size = new_size;
        Ok(())
    }

    fn reject_if_wal_present(&self) -> Result<()> {
        if let Some(ref wpath) = self.wal_path
            && self.vfs.exists(wpath)?
            && let Ok(wfile) = self.vfs.open(Some(wpath), OpenFlags::READONLY)
            && wfile.file_size()? > 0
        {
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                "WAL mode databases are not supported; checkpoint in SQLite with PRAGMA journal_mode=DELETE first",
            ));
        }
        Ok(())
    }

    /// Starts a read transaction (`SHARED` lock + safe hot-journal check/recovery).
    pub fn begin_read(&mut self) -> Result<()> {
        if self.state != TransactionState::Open {
            return Ok(());
        }
        self.file.lock(LockLevel::Shared)?;
        if let Err(e) = self.begin_read_under_shared_lock() {
            let _ = self.file.unlock(LockLevel::NoLock);
            return Err(e);
        }
        self.state = TransactionState::Read;
        Ok(())
    }

    fn begin_read_under_shared_lock(&mut self) -> Result<()> {
        self.reject_if_wal_present()?;
        if !self.in_memory {
            let recovered = self.recover_hot_journal_under_shared_lock()?;
            let prev_counter = self.header.change_counter;
            let file_len = self.file.file_size()?;
            if file_len == 0 {
                if recovered || self.page_count != 0 {
                    self.cache.clear();
                    self.page_count = 0;
                    self.header.database_size_pages = 0;
                    self.header.change_counter = 0;
                    self.header.schema_cookie = 0;
                    self.page_size_fixed = false;
                }
            } else if file_len >= DB_HEADER_SIZE as u64 {
                let mut hdr_bytes = [0u8; DB_HEADER_SIZE];
                self.file.read_at(&mut hdr_bytes, 0)?;
                let disk_hdr = DatabaseHeader::parse(&hdr_bytes)?;
                if recovered
                    || disk_hdr.change_counter != prev_counter
                    || disk_hdr.schema_cookie != self.header.schema_cookie
                    || !self.page_size_fixed
                {
                    self.cache.clear();
                    self.load_initial_header()?;
                }
            } else {
                return Err(SqliteError::new(
                    ErrorCode::NotADb,
                    "file is not a database",
                ));
            }
        }
        Ok(())
    }

    /// Starts or upgrades to a write transaction (`RESERVED` lock + journal initialization).
    pub fn begin_write(&mut self) -> Result<()> {
        self.begin_write_with_lock(LockLevel::Reserved)
    }

    /// Starts or upgrades to an exclusive write transaction (`BEGIN EXCLUSIVE`: acquires `EXCLUSIVE` lock immediately).
    pub fn begin_exclusive(&mut self) -> Result<()> {
        if self.state == TransactionState::Write {
            if !self.in_memory {
                self.file.lock(LockLevel::Exclusive)?;
            }
            return Ok(());
        }
        self.begin_write_with_lock(LockLevel::Exclusive)
    }

    fn begin_write_with_lock(&mut self, target_lock: LockLevel) -> Result<()> {
        if self.readonly {
            return Err(SqliteError::readonly(
                "attempt to write a readonly database",
            ));
        }
        if self.state == TransactionState::Write {
            return Ok(());
        }
        let acquired_read_here = self.state == TransactionState::Open;
        if acquired_read_here {
            self.begin_read()?;
        }

        // Reject writing to auto-vacuum (`FULL` / `INCREMENTAL`) pointer-map databases until ptrmap maintenance is implemented.
        if self.page_count > 0 && self.header.largest_root_btree_page != 0 {
            if acquired_read_here {
                let _ = self.file.unlock(LockLevel::NoLock);
                self.state = TransactionState::Open;
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                "writing to auto_vacuum (pointer-map) databases is not supported",
            ));
        }

        let lock_res = if target_lock >= LockLevel::Exclusive {
            self.file
                .lock(LockLevel::Reserved)
                .and_then(|()| self.file.lock(LockLevel::Exclusive))
        } else {
            self.file.lock(LockLevel::Reserved)
        };
        if let Err(e) = lock_res {
            if acquired_read_here {
                let _ = self.file.unlock(LockLevel::NoLock);
                self.state = TransactionState::Open;
            } else {
                let _ = self.file.unlock(LockLevel::Shared);
            }
            return Err(e);
        }

        self.tx_orig_page_count = self.page_count;
        self.tx_orig_header = self.header.clone();
        self.tx_journaled = Bitvec::new(self.page_count.max(64));
        self.tx_preimages.clear();
        self.journal_records_written = 0;
        self.journal_sector_size = self.effective_sector_size();
        self.db_file_modified = false;

        let mut nonce_bytes = [0u8; 4];
        self.vfs.randomness(&mut nonce_bytes);
        self.journal_nonce = u32::from_be_bytes(nonce_bytes);

        if let Some(ref jpath) = self.journal_path {
            let jopen_res = self
                .vfs
                .open(
                    Some(jpath),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_JOURNAL,
                )
                .and_then(|jfile| {
                    self.write_journal_header(&*jfile, 0)?;
                    Ok(jfile)
                });
            match jopen_res {
                Ok(jfile) => self.journal_file = Some(jfile),
                Err(e) => {
                    if acquired_read_here {
                        let _ = self.file.unlock(LockLevel::NoLock);
                        self.state = TransactionState::Open;
                    } else {
                        let _ = self.file.unlock(LockLevel::Shared);
                    }
                    return Err(e);
                }
            }
        }

        self.state = TransactionState::Write;

        // If the database is brand-new (0 pages), initialize Page 1 as an empty table leaf B-Tree (`sqlite_schema`).
        if self.page_count == 0 {
            self.initialize_empty_database()?;
        }

        Ok(())
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
        if pgno == 0 || pgno > self.page_count {
            return Err(SqliteError::corrupt(format!(
                "requested invalid page {pgno} (page_count = {})",
                self.page_count
            )));
        }
        if !self.cache.contains_key(&pgno) {
            let ps = self.header.page_size as usize;
            let mut buf = vec![0u8; ps];
            let offset = u64::from(pgno - 1) * (ps as u64);
            self.file.read_at(&mut buf, offset)?;
            self.cache.insert(
                pgno,
                CachedPage {
                    data: buf,
                    dirty: false,
                },
            );
        }
        Ok(self.cache.get(&pgno).expect("just inserted").data.as_slice())
    }

    /// Journals the pre-image of `pgno` (if not yet journaled in this tx/savepoints) and marks it dirty.
    fn mark_page_writable(&mut self, pgno: Pgno) -> Result<()> {
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

        // Ensure page is loaded into cache first
        let _ = self.read_page(pgno)?;
        let current_bytes = if !self.tx_journaled.test(pgno)
            || self
                .savepoints
                .iter()
                .any(|sp| pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno))
        {
            Some(self.cache.get(&pgno).expect("loaded").data.clone())
        } else {
            None
        };

        if let Some(orig) = current_bytes {
            // Save for active savepoints if page existed when savepoint began
            for sp in &mut self.savepoints {
                if pgno <= sp.orig_page_count && !sp.saved_pages.contains_key(&pgno) {
                    sp.saved_pages.insert(pgno, orig.clone());
                }
            }
            // Save for top-level transaction & disk rollback journal if page existed before tx
            if pgno <= self.tx_orig_page_count && !self.tx_journaled.test(pgno) {
                self.tx_journaled.grow_to(self.tx_orig_page_count.max(pgno));
                self.tx_journaled.set(pgno)?;
                if self.journal_file.is_some() {
                    self.append_journal_page(pgno, &orig)?;
                }
                self.tx_preimages.insert(pgno, orig);
            }
        }

        self.cache.get_mut(&pgno).expect("loaded").dirty = true;
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

    /// Appends a brand-new zeroed page at the end of the database file, returning its `Pgno`.
    pub fn allocate_raw_page(&mut self) -> Result<Pgno> {
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "cannot allocate page outside write transaction",
            ));
        }
        let next_pgno = self
            .page_count
            .checked_add(1)
            .ok_or_else(|| SqliteError::new(ErrorCode::Full, "database page count overflow"))?;

        // Skip the lock-byte page at 1 GiB boundary just like SQLite (`PENDING_BYTE_PAGE`)!
        let ps = u64::from(self.header.page_size);
        let lock_byte_pgno = ((sqliters_types::PENDING_BYTE_OFFSET / ps) + 1) as Pgno;
        let allocated_pgno = if next_pgno == lock_byte_pgno {
            // Allocate dummy lock-byte page and advance one more page
            self.cache.insert(
                lock_byte_pgno,
                CachedPage {
                    data: vec![0u8; ps as usize],
                    dirty: true,
                },
            );
            self.page_count = lock_byte_pgno;
            lock_byte_pgno
                .checked_add(1)
                .ok_or_else(|| SqliteError::new(ErrorCode::Full, "database page count overflow"))?
        } else {
            next_pgno
        };

        self.page_count = allocated_pgno;
        self.header.database_size_pages = allocated_pgno;
        self.tx_journaled.grow_to(allocated_pgno);
        self.cache.insert(
            allocated_pgno,
            CachedPage {
                data: vec![0u8; ps as usize],
                dirty: true,
            },
        );
        // Page 1 header will need database_size_pages updated on commit
        self.mark_page_writable(1)?;
        Ok(allocated_pgno)
    }

    /// Shrinks the database page count to `new_count` within a write transaction (used by `VACUUM`),
    /// journaling all truncated pages first so `rollback()` / hot-journal recovery can restore them.
    pub fn shrink_page_count(&mut self, new_count: Pgno) -> Result<()> {
        if self.state != TransactionState::Write {
            return Err(SqliteError::misuse(
                "cannot shrink page count outside write transaction",
            ));
        }
        if new_count < self.page_count {
            for pgno in (new_count + 1)..=self.page_count {
                self.mark_page_writable(pgno)?;
            }
            self.cache.retain(|&pgno, _| pgno <= new_count);
            self.page_count = new_count;
            self.header.database_size_pages = new_count;
            if new_count > 0 {
                self.mark_page_writable(1)?;
            }
        }
        Ok(())
    }

    /// Commits the active write transaction (or releases read lock if in read transaction).
    pub fn commit(&mut self) -> Result<()> {
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => {
                self.file.unlock(LockLevel::NoLock)?;
                self.state = TransactionState::Open;
                Ok(())
            }
            TransactionState::Write => {
                // Bump file change counter & version_valid_for on Page 1
                if self.page_count > 0 {
                    self.mark_page_writable(1)?;
                    self.header.database_size_pages = self.page_count;
                    self.header.change_counter = self.header.change_counter.wrapping_add(1);
                    if self.header.change_counter == 0 {
                        self.header.change_counter = 1;
                    }
                    self.header.version_valid_for = self.header.change_counter;
                    self.header.sqlite_version_number = SQLITE_VERSION_NUMBER;

                    let hdr_copy = self.header.clone();
                    let p1 = self.cache.get_mut(&1).expect("page 1 in cache");
                    hdr_copy.write_into(&mut p1.data[0..DB_HEADER_SIZE]);
                    p1.dirty = true;
                }

                // Step 1: Finalize and fsync rollback journal BEFORE touching the main DB file!
                if let Some(ref jfile) = self.journal_file {
                    self.write_journal_header(&**jfile, self.journal_records_written)?;
                    if self.synchronous > 0 {
                        jfile.sync(SyncFlags::Normal)?;
                    }
                }

                // Step 2: Acquire EXCLUSIVE lock on main DB file and write all dirty pages in Pgno order.
                // Do NOT clear `page.dirty` or undo state until all writes, truncate, sync, and journal deletion succeed!
                self.file.lock(LockLevel::Exclusive)?;
                self.db_file_modified = true;

                let ps = u64::from(self.header.page_size);
                let mut dirty_pgnos: Vec<Pgno> = self
                    .cache
                    .iter()
                    .filter_map(|(&pgno, p)| if p.dirty && pgno <= self.page_count { Some(pgno) } else { None })
                    .collect();
                dirty_pgnos.sort_unstable();

                for &pgno in &dirty_pgnos {
                    let page = self.cache.get(&pgno).expect("dirty page exists");
                    let offset = u64::from(pgno - 1) * ps;
                    self.file.write_at(&page.data, offset)?;
                }

                let target_size = u64::from(self.page_count) * ps;
                if self.file.file_size()? != target_size {
                    self.file.truncate(target_size)?;
                }

                if !self.in_memory && self.synchronous > 0 {
                    self.file.sync(SyncFlags::Normal)?;
                }

                // Step 3: Delete rollback journal (atomic commit point in SQLite!)
                self.journal_file = None;
                if let Some(ref jpath) = self.journal_path {
                    self.vfs.delete(jpath, true)?;
                }

                // Step 4: Mark all committed pages clean, clear undo state, and unlock
                for pgno in dirty_pgnos {
                    if let Some(page) = self.cache.get_mut(&pgno) {
                        page.dirty = false;
                    }
                }
                self.db_file_modified = false;
                self.tx_preimages.clear();
                self.tx_journaled.clear_all();
                self.savepoints.clear();
                self.file.unlock(LockLevel::NoLock)?;
                self.state = TransactionState::Open;
                Ok(())
            }
        }
    }

    /// Rolls back the active transaction, restoring pre-images and original page count
    /// (including replaying pre-images back to disk if `commit()` partially modified the main DB file).
    pub fn rollback(&mut self) -> Result<()> {
        match self.state {
            TransactionState::Open => Ok(()),
            TransactionState::Read => {
                self.file.unlock(LockLevel::NoLock)?;
                self.state = TransactionState::Open;
                Ok(())
            }
            TransactionState::Write => {
                let orig_count = self.tx_orig_page_count;

                // If `commit()` already started modifying the main DB file (`db_file_modified`),
                // we MUST restore all pre-images and original file size on disk and sync BEFORE
                // unlinking `<db>-journal`. If any disk operation fails here, keep `<db>-journal`
                // intact on disk so hot-journal recovery can still restore atomicity!
                if self.db_file_modified && !self.in_memory {
                    self.file.lock(LockLevel::Exclusive)?;
                    let ps = u64::from(self.tx_orig_header.page_size);
                    let mut restore_pgnos: Vec<Pgno> = self
                        .tx_preimages
                        .keys()
                        .copied()
                        .filter(|&pgno| pgno > 0 && pgno <= orig_count)
                        .collect();
                    restore_pgnos.sort_unstable();

                    for pgno in restore_pgnos {
                        let orig_data = self.tx_preimages.get(&pgno).expect("key exists");
                        let offset = u64::from(pgno - 1) * ps;
                        self.file.write_at(orig_data, offset)?;
                    }

                    let orig_size = u64::from(orig_count) * ps;
                    if self.file.file_size()? != orig_size {
                        self.file.truncate(orig_size)?;
                    }
                    if self.synchronous > 0 {
                        self.file.sync(SyncFlags::Normal)?;
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
                self.db_file_modified = false;
                self.savepoints.clear();

                // Close & remove journal file now that main DB file is verified clean
                self.journal_file = None;
                if let Some(ref jpath) = self.journal_path {
                    let _ = self.vfs.delete(jpath, false);
                }

                self.file.unlock(LockLevel::NoLock)?;
                self.state = TransactionState::Open;
                Ok(())
            }
        }
    }

    /// Returns the number of active savepoints on the stack.
    #[inline]
    pub fn savepoint_depth(&self) -> usize {
        self.savepoints.len()
    }

    /// Creates a named SQL savepoint (`SAVEPOINT name`).
    pub fn savepoint(&mut self, name: &str) -> Result<()> {
        if self.state != TransactionState::Write {
            self.begin_write()?;
        }
        self.savepoints.push(SavepointFrame {
            name: name.to_owned(),
            orig_page_count: self.page_count,
            orig_header: self.header.clone(),
            saved_pages: HashMap::new(),
        });
        Ok(())
    }

    /// Rolls back changes to the most recent matching savepoint (`ROLLBACK TO SAVEPOINT name`),
    /// keeping the savepoint itself active on the stack per SQLite semantics.
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Result<()> {
        let Some(pos) = self
            .savepoints
            .iter()
            .rposition(|sp| sp.name.eq_ignore_ascii_case(name))
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such savepoint: {name}"),
            ));
        };

        // Roll back frames from top down to `pos` (inclusive), then re-keep empty frame `pos`.
        while self.savepoints.len() > pos + 1 {
            let sp = self.savepoints.pop().expect("len checked");
            self.apply_savepoint_undo(&sp);
        }
        let target = self.savepoints[pos].clone();
        self.apply_savepoint_undo(&target);
        self.savepoints[pos].saved_pages.clear();
        Ok(())
    }

    fn apply_savepoint_undo(&mut self, sp: &SavepointFrame) {
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
    }

    /// Releases (merges) `SAVEPOINT name` and any inner savepoints (`RELEASE [SAVEPOINT] name`).
    pub fn release_savepoint(&mut self, name: &str) -> Result<()> {
        let Some(pos) = self
            .savepoints
            .iter()
            .rposition(|sp| sp.name.eq_ignore_ascii_case(name))
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such savepoint: {name}"),
            ));
        };

        // Merge saved preimages into parent savepoint if one exists above `pos`
        let drained: Vec<SavepointFrame> = self.savepoints.drain(pos..).collect();
        if pos > 0 {
            let parent = &mut self.savepoints[pos - 1];
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
    // Rollback Journal Binary Format (`writeJournalHdr`, `readJournalHdr`, `pager_playback` in `sqlite/src/pager.c`)
    // ========================================================================

    fn effective_sector_size(&self) -> u32 {
        let raw = self
            .file
            .sector_size()
            .clamp(MIN_JOURNAL_HEADER_BYTES as u32, MAX_JOURNAL_SECTOR_BYTES);
        if raw.is_power_of_two() {
            raw
        } else {
            raw.next_power_of_two()
                .clamp(MIN_JOURNAL_HEADER_BYTES as u32, MAX_JOURNAL_SECTOR_BYTES)
        }
    }

    fn write_journal_header(&self, jfile: &dyn VfsFile, record_count: u32) -> Result<()> {
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
        let hdr_sz = u64::from(self.journal_sector_size.max(MIN_JOURNAL_HEADER_BYTES as u32));
        let ps = page_data.len();
        let rec_size = 4 + ps + 4;
        let offset = hdr_sz + u64::from(self.journal_records_written) * (rec_size as u64);

        let cksum = Self::journal_checksum(self.journal_nonce, page_data);
        let mut buf = Vec::with_capacity(rec_size);
        buf.extend_from_slice(&pgno.to_be_bytes());
        buf.extend_from_slice(page_data);
        buf.extend_from_slice(&cksum.to_be_bytes());

        jfile.write_at(&buf, offset)?;
        self.journal_records_written += 1;
        Ok(())
    }

    /// Safe hot-journal detection and multi-section recovery (`hasHotJournal` + `readJournalHdr` + `pager_playback`
    /// in `sqlite/src/pager.c`), called ONLY while holding at least `LockLevel::Shared` on the main database file!
    fn recover_hot_journal_under_shared_lock(&mut self) -> Result<bool> {
        let Some(ref jpath) = self.journal_path else {
            return Ok(false);
        };
        if !self.vfs.exists(jpath)? {
            return Ok(false);
        }

        // CRITICAL (`pager.c:5254`): If ANY handle/process holds RESERVED (or higher),
        // an active writer owns `<db>-journal`. It is NOT a hot journal — do not touch it!
        if self.file.check_reserved_lock()? {
            return Ok(false);
        }

        let Ok(jfile) = self.vfs.open(Some(jpath), OpenFlags::READONLY) else {
            return Ok(false);
        };
        let jsize = jfile.file_size()?;
        if jsize == 0 {
            return Ok(false);
        }

        let mut first_byte = [0u8; 1];
        if jfile.read_at(&mut first_byte, 0)? == 0 || first_byte[0] == 0 {
            // `journal_mode=PERSIST` zeroes byte 0 to mark an inactive journal.
            return Ok(false);
        }

        if self.readonly {
            return Err(SqliteError::with_extended(
                extended::SQLITE_READONLY_ROLLBACK,
                "unable to open database: hot journal requires write access for recovery",
            ));
        }

        // Upgrade directly from SHARED -> EXCLUSIVE (`pager.c:5606`); fails with SQLITE_BUSY
        // if another reader also holds SHARED, leaving the hot journal intact for retry.
        self.file.lock(LockLevel::Exclusive)?;

        if jsize < 28 {
            drop(jfile);
            self.vfs.delete(jpath, true)?;
            self.file.unlock(LockLevel::Shared)?;
            return Ok(true);
        }

        let mut hdr0 = [0u8; 28];
        jfile.read_at(&mut hdr0, 0)?;
        if hdr0[0..8] != JOURNAL_MAGIC {
            drop(jfile);
            self.vfs.delete(jpath, true)?;
            self.file.unlock(LockLevel::Shared)?;
            return Ok(true);
        }

        let orig_pages = u32::from_be_bytes([hdr0[16], hdr0[17], hdr0[18], hdr0[19]]);
        let raw_sector_size = u32::from_be_bytes([hdr0[20], hdr0[21], hdr0[22], hdr0[23]]);
        let raw_page_size = u32::from_be_bytes([hdr0[24], hdr0[25], hdr0[26], hdr0[27]]);
        let page_size = if raw_page_size == 0 {
            self.header.page_size
        } else {
            raw_page_size
        };

        // Validate `page_size` and `sector_size` per `readJournalHdr` (`pager.c:1700-1710`).
        let valid_sector = (32..=MAX_JOURNAL_SECTOR_BYTES).contains(&raw_sector_size)
            && raw_sector_size.is_power_of_two();
        let hdr_sz = u64::from(raw_sector_size);
        if !DatabaseHeader::is_valid_page_size(page_size) || !valid_sector || jsize < hdr_sz {
            drop(jfile);
            self.vfs.delete(jpath, true)?;
            self.file.unlock(LockLevel::Shared)?;
            return Ok(true);
        }

        // Truncate main DB back to its pre-transaction size (`pager.c:2959`).
        self.file
            .truncate(u64::from(orig_pages) * u64::from(page_size))?;

        let ps = page_size as usize;
        let rec_size = (4 + ps + 4) as u64;
        let sj_pgno = ((sqliters_types::PENDING_BYTE_OFFSET / u64::from(page_size)) + 1) as Pgno;
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

            let mut sec_hdr = [0u8; 28];
            jfile.read_at(&mut sec_hdr, journal_off)?;
            if sec_hdr[0..8] != JOURNAL_MAGIC {
                break;
            }

            let raw_nrec = u32::from_be_bytes([sec_hdr[8], sec_hdr[9], sec_hdr[10], sec_hdr[11]]);
            let nonce = u32::from_be_bytes([sec_hdr[12], sec_hdr[13], sec_hdr[14], sec_hdr[15]]);
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
                self.file.write_at(data, db_off)?;
                replayed_pages.set(pgno)?;
            }

            // Both `0xffffffff` (no-sync mode) and `0` (active un-spilled tail section) consume up to EOF.
            if raw_nrec == 0xffff_ffff || raw_nrec == 0 {
                break;
            }
        }

        if !self.in_memory {
            self.file.sync(SyncFlags::Normal)?;
        }
        drop(jfile);
        self.vfs.delete(jpath, true)?;
        self.cache.clear();
        self.page_size_fixed = false;
        self.file.unlock(LockLevel::Shared)?;
        Ok(true)
    }
}

impl Drop for Pager {
    fn drop(&mut self) {
        if self.state != TransactionState::Open && self.rollback().is_err() {
            // Rollback failed (e.g. persistent disk I/O error after partial commit):
            // preserve `<db>-journal` on disk for future hot-journal recovery and release OS locks.
            self.journal_file = None;
            let _ = self.file.unlock(LockLevel::NoLock);
            self.state = TransactionState::Open;
        }
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

        pager.begin_write().unwrap();
        assert_eq!(pager.page_count(), 1);
        let p2 = pager.allocate_raw_page().unwrap();
        assert_eq!(p2, 2);
        pager.write_page(2).unwrap()[0..4].copy_from_slice(b"ABCD");
        pager.commit().unwrap();

        // Reopen and verify persistence
        let mut pager2 = Pager::open_with_vfs(
            vfs.clone(),
            Some(Path::new("unit.db")),
            OpenFlags::READWRITE,
        )
        .unwrap();
        assert_eq!(pager2.page_count(), 2);
        assert_eq!(&pager2.read_page(2).unwrap()[0..4], b"ABCD");

        // Modify in write tx + savepoint, rollback savepoint, then rollback tx
        pager2.begin_write().unwrap();
        pager2.write_page(2).unwrap()[0..4].copy_from_slice(b"XXXX");
        pager2.savepoint("sp1").unwrap();
        let p3 = pager2.allocate_raw_page().unwrap();
        assert_eq!(p3, 3);
        pager2.write_page(2).unwrap()[0..4].copy_from_slice(b"YYYY");

        pager2.rollback_to_savepoint("sp1").unwrap();
        assert_eq!(pager2.page_count(), 2);
        assert_eq!(&pager2.read_page(2).unwrap()[0..4], b"XXXX");

        pager2.rollback().unwrap();
        assert_eq!(pager2.page_count(), 2);
        assert_eq!(&pager2.read_page(2).unwrap()[0..4], b"ABCD");
    }
}
