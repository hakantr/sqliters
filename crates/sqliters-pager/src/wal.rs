//! Write-Ahead Log (`-wal`) engine ported from `sqlite/src/wal.c` and `sqlite/src/wal.h` (3.54.1).
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::IoSlice;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqliters_types::{
    ErrorCode, Pgno, Result, SqliteError, decode_wal_header_page_size, encode_wal_header_page_size,
    extended,
};
use sqliters_vfs::{
    CheckpointMode, FastBeginReadOutcome, IOCAP_POWERSAFE_OVERWRITE, IOCAP_SEQUENTIAL, OpenFlags,
    OwnedWalReadGuard, OwnedWriterTxnGuard, READMARK_NOT_USED, ReadonlyHeaderProbe, ShmDmsState,
    ShmOpenOptions, SyncFlags, Vfs, VfsFile, WAL_NREADER, WALINDEX_MAX_VERSION, WalChecksumEndian,
    WalCkptInfoSnapshot, WalIndexHeaderData, WalShm, wal_checksum_with_endian, wal_frame_page,
};

/// Byte size of the WAL file header (`WAL_HDRSIZE = 32` in `sqlite/src/wal.c:393`).
pub const WAL_HDRSIZE: usize = 32;

/// Byte size of each WAL frame header (`WAL_FRAME_HDRSIZE = 24` in `sqlite/src/wal.c:390`).
pub const WAL_FRAME_HDRSIZE: usize = 24;

/// WAL file header magic for little-endian checksum words (`WAL_MAGIC = 0x377f0682`).
pub const WAL_MAGIC_LE: u32 = 0x377f_0682;

/// WAL file header magic for big-endian checksum words (`WAL_MAGIC | 1 = 0x377f0683`).
pub const WAL_MAGIC_BE: u32 = 0x377f_0683;

/// Supported WAL file format version (`WAL_MAX_VERSION = 3007000` in `sqlite/src/wal.c:383`).
pub const WAL_MAX_VERSION: u32 = 3_007_000;

/// Number of 32-bit words in a WAL sub-savepoint token (`WAL_SAVEPOINT_NDATA = 4` in `wal.h:26`).
pub const WAL_SAVEPOINT_NDATA: usize = 4;

/// Retry ceiling in `walTryBeginRead` before returning `ErrorCode::Protocol` (`wal.c:3045`).
pub const WAL_RETRY_PROTOCOL_LIMIT: u32 = 100;

const WAL_RDWR: u8 = 0x00;
const WAL_RDONLY: u8 = 0x01;
const WAL_SHM_RDONLY: u8 = 0x02;

/// Checkpoint execution modes matching `SQLITE_CHECKPOINT_*` (`sqlite.h.in:9019-9023`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum WalCheckpointMode {
    Noop = -1,
    Passive = 0,
    Full = 1,
    Restart = 2,
    Truncate = 3,
}

impl WalCheckpointMode {
    pub fn from_pragma_str(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("noop") {
            Some(Self::Noop)
        } else if s.eq_ignore_ascii_case("passive") {
            Some(Self::Passive)
        } else if s.eq_ignore_ascii_case("full") {
            Some(Self::Full)
        } else if s.eq_ignore_ascii_case("restart") {
            Some(Self::Restart)
        } else if s.eq_ignore_ascii_case("truncate") {
            Some(Self::Truncate)
        } else {
            None
        }
    }

    pub const fn from_i32(v: i32) -> Option<Self> {
        match v {
            -1 => Some(Self::Noop),
            0 => Some(Self::Passive),
            1 => Some(Self::Full),
            2 => Some(Self::Restart),
            3 => Some(Self::Truncate),
            _ => None,
        }
    }

    const fn to_shm_mode(self) -> Option<CheckpointMode> {
        match self {
            Self::Noop => None,
            Self::Passive => Some(CheckpointMode::Passive),
            Self::Full => Some(CheckpointMode::Full),
            Self::Restart => Some(CheckpointMode::Restart),
            Self::Truncate => Some(CheckpointMode::Truncate),
        }
    }
}

/// Result of a WAL checkpoint (`sqlite3_wal_checkpoint_v2` / `PRAGMA wal_checkpoint`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalCheckpointResult {
    /// `true` when checkpoint could not complete due to active readers/writers (`SQLITE_BUSY`).
    pub busy: bool,
    /// Total frames in the WAL (`pnLog`), or `-1` when not in WAL mode.
    pub n_log: i32,
    /// Total backfilled frames in the WAL (`pnCkpt`), or `-1` when not in WAL mode.
    pub n_ckpt: i32,
}

impl WalCheckpointResult {
    /// Sentinel result returned when the target database is not in WAL mode (`(-1, -1)`).
    #[inline]
    pub const fn not_wal() -> Self {
        Self {
            busy: false,
            n_log: -1,
            n_ckpt: -1,
        }
    }

    /// Converts into `(busy_flag, n_log, n_ckpt)` matching `PRAGMA wal_checkpoint` / Tcl `sqlite3_wal_checkpoint_v2`.
    #[inline]
    pub const fn into_c_tuple(self) -> (i32, i32, i32) {
        (self.busy as i32, self.n_log, self.n_ckpt)
    }
}

/// Exclusive WAL-index operating mode (`pWal->exclusiveMode` in `sqlite/src/wal.c:564-566`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalExclusiveMode {
    Normal = 0,
    Exclusive = 1,
    HeapMemory = 2,
}

/// Decoded 32-byte WAL file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalFileHeader {
    pub magic: u32,
    pub version: u32,
    pub page_size: u32,
    pub n_ckpt: u32,
    pub salt: [u32; 2],
    pub big_end_cksum: bool,
    pub cksum: [u32; 2],
}

impl WalFileHeader {
    /// Encodes a 32-byte WAL file header (`wal.c:4178-4186`).
    pub fn encode(
        page_size: u32,
        n_ckpt: u32,
        salt: [u32; 2],
        big_end_cksum: bool,
    ) -> Result<([u8; WAL_HDRSIZE], Self)> {
        let raw_sz = encode_wal_header_page_size(page_size)?;
        let magic = if big_end_cksum {
            WAL_MAGIC_BE
        } else {
            WAL_MAGIC_LE
        };
        let mut buf = [0u8; WAL_HDRSIZE];
        buf[0..4].copy_from_slice(&magic.to_be_bytes());
        buf[4..8].copy_from_slice(&WAL_MAX_VERSION.to_be_bytes());
        buf[8..12].copy_from_slice(&raw_sz);
        buf[12..16].copy_from_slice(&n_ckpt.to_be_bytes());
        buf[16..20].copy_from_slice(&salt[0].to_be_bytes());
        buf[20..24].copy_from_slice(&salt[1].to_be_bytes());

        let endian = cksum_endian(big_end_cksum);
        let cksum = wal_checksum_with_endian(endian, &buf[0..24], None)?;
        buf[24..28].copy_from_slice(&cksum[0].to_be_bytes());
        buf[28..32].copy_from_slice(&cksum[1].to_be_bytes());

        Ok((
            buf,
            Self {
                magic,
                version: WAL_MAX_VERSION,
                page_size,
                n_ckpt,
                salt,
                big_end_cksum,
                cksum,
            },
        ))
    }

    /// Parses and validates a 32-byte WAL file header (`walIndexRecover`, `wal.c:1477-1507`):
    /// - Returns `Ok(None)` if magic, `szPage`, or header checksum is invalid (unusable WAL).
    /// - Returns `Err(ErrorCode::CantOpen)` if header checksum is valid with `version != 3007000`.
    /// - Returns `Ok(Some(hdr))` when valid.
    pub fn decode_for_recovery(buf: &[u8; WAL_HDRSIZE]) -> Result<Option<Self>> {
        let magic = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if (magic & 0xffff_fffe) != WAL_MAGIC_LE {
            return Ok(None);
        }
        let Ok(page_size) = decode_wal_header_page_size([buf[8], buf[9], buf[10], buf[11]]) else {
            return Ok(None);
        };
        let big_end_cksum = (magic & 1) != 0;
        let endian = cksum_endian(big_end_cksum);
        let expected_cksum = wal_checksum_with_endian(endian, &buf[0..24], None)?;
        let c0 = u32::from_be_bytes([buf[24], buf[25], buf[26], buf[27]]);
        let c1 = u32::from_be_bytes([buf[28], buf[29], buf[30], buf[31]]);
        if expected_cksum != [c0, c1] {
            return Ok(None);
        }

        let version = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if version != WAL_MAX_VERSION {
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                format!("unsupported WAL format version: {version}"),
            ));
        }

        let n_ckpt = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let s0 = u32::from_be_bytes([buf[16], buf[17], buf[18], buf[19]]);
        let s1 = u32::from_be_bytes([buf[20], buf[21], buf[22], buf[23]]);

        Ok(Some(Self {
            magic,
            version,
            page_size,
            n_ckpt,
            salt: [s0, s1],
            big_end_cksum,
            cksum: [c0, c1],
        }))
    }
}

#[inline]
const fn cksum_endian(big_end_cksum: bool) -> WalChecksumEndian {
    if big_end_cksum {
        WalChecksumEndian::Big
    } else {
        WalChecksumEndian::Little
    }
}

/// Computes the byte offset in `-wal` of 1-based `frame_no` (`walFrameOffset`, `wal.c:556`).
#[inline]
pub const fn wal_frame_offset(frame_no: u32, page_size: u32) -> u64 {
    (WAL_HDRSIZE as u64)
        + ((frame_no - 1) as u64) * ((WAL_FRAME_HDRSIZE as u64) + (page_size as u64))
}

/// Encodes a 24-byte WAL frame header and returns `(encoded_header, updated_cksum)` (`walEncodeFrame`).
pub fn wal_encode_frame_header(
    pgno: Pgno,
    n_truncate: u32,
    salt: [u32; 2],
    big_end_cksum: bool,
    prev_cksum: [u32; 2],
    page_data: &[u8],
) -> Result<([u8; WAL_FRAME_HDRSIZE], [u32; 2])> {
    let mut hdr = [0u8; WAL_FRAME_HDRSIZE];
    hdr[0..4].copy_from_slice(&pgno.to_be_bytes());
    hdr[4..8].copy_from_slice(&n_truncate.to_be_bytes());
    hdr[8..12].copy_from_slice(&salt[0].to_be_bytes());
    hdr[12..16].copy_from_slice(&salt[1].to_be_bytes());

    let endian = cksum_endian(big_end_cksum);
    let mid = wal_checksum_with_endian(endian, &hdr[0..8], Some(prev_cksum))?;
    let next_cksum = wal_checksum_with_endian(endian, page_data, Some(mid))?;
    hdr[16..20].copy_from_slice(&next_cksum[0].to_be_bytes());
    hdr[20..24].copy_from_slice(&next_cksum[1].to_be_bytes());
    Ok((hdr, next_cksum))
}

/// Decodes and verifies a 24-byte WAL frame header + `page_data` (`walDecodeFrame`, `wal.c:1032`),
/// returning `Ok(Some((pgno, n_truncate, next_cksum)))` when valid or `Ok(None)` on salt, pgno, or
/// checksum mismatch.
pub fn wal_decode_frame(
    frame_hdr: &[u8; WAL_FRAME_HDRSIZE],
    page_data: &[u8],
    expected_salt: [u32; 2],
    big_end_cksum: bool,
    prev_cksum: [u32; 2],
) -> Result<Option<(Pgno, u32, [u32; 2])>> {
    let s0 = u32::from_be_bytes([frame_hdr[8], frame_hdr[9], frame_hdr[10], frame_hdr[11]]);
    let s1 = u32::from_be_bytes([frame_hdr[12], frame_hdr[13], frame_hdr[14], frame_hdr[15]]);
    if [s0, s1] != expected_salt {
        return Ok(None);
    }
    let pgno = u32::from_be_bytes([frame_hdr[0], frame_hdr[1], frame_hdr[2], frame_hdr[3]]);
    if pgno == 0 {
        return Ok(None);
    }
    let n_truncate = u32::from_be_bytes([frame_hdr[4], frame_hdr[5], frame_hdr[6], frame_hdr[7]]);

    let endian = cksum_endian(big_end_cksum);
    let mid = wal_checksum_with_endian(endian, &frame_hdr[0..8], Some(prev_cksum))?;
    let calc = wal_checksum_with_endian(endian, page_data, Some(mid))?;
    let c0 = u32::from_be_bytes([frame_hdr[16], frame_hdr[17], frame_hdr[18], frame_hdr[19]]);
    let c1 = u32::from_be_bytes([frame_hdr[20], frame_hdr[21], frame_hdr[22], frame_hdr[23]]);
    if calc != [c0, c1] {
        return Ok(None);
    }
    Ok(Some((pgno, n_truncate, calc)))
}

/// Private heap-backed WAL-index used ONLY when SQLite 3.54.1 explicitly permits heap mode:
/// - `WAL_HEAPMEMORY_MODE` (`locking_mode = EXCLUSIVE` at open or `immutable = true`)
/// - `b_shm_unreliable = true` (`WAL_SHM_RDONLY` with `SQLITE_READONLY_CANTINIT` under `READ(0)`)
#[derive(Debug, Clone)]
struct HeapWalIndex {
    hdr: Option<WalIndexHeaderData>,
    ckpt: WalCkptInfoSnapshot,
    frame_pgnos: Vec<Pgno>,
    page_frames: HashMap<Pgno, Vec<u32>>,
}

impl Default for HeapWalIndex {
    fn default() -> Self {
        Self {
            hdr: None,
            ckpt: WalCkptInfoSnapshot {
                n_backfill: 0,
                read_marks: [
                    0,
                    READMARK_NOT_USED,
                    READMARK_NOT_USED,
                    READMARK_NOT_USED,
                    READMARK_NOT_USED,
                ],
                n_backfill_attempted: 0,
            },
            frame_pgnos: Vec::new(),
            page_frames: HashMap::new(),
        }
    }
}

impl HeapWalIndex {
    fn reset_empty(&mut self) {
        self.frame_pgnos.clear();
        self.page_frames.clear();
    }

    fn record_frame(&mut self, frame_no: u32, pgno: Pgno) -> Result<()> {
        if frame_no == 0 || pgno == 0 {
            return Err(SqliteError::misuse("WAL frame_no and pgno must be >= 1"));
        }
        if (frame_no as usize) <= self.frame_pgnos.len() {
            self.truncate_frames_above(frame_no - 1);
        }
        if (frame_no as usize) != self.frame_pgnos.len() + 1 {
            return Err(SqliteError::misuse(
                "non-sequential WAL frame append in heap index",
            ));
        }
        self.frame_pgnos.push(pgno);
        self.page_frames.entry(pgno).or_default().push(frame_no);
        Ok(())
    }

    fn truncate_frames_above(&mut self, max_frame: u32) {
        let target_len = max_frame as usize;
        while self.frame_pgnos.len() > target_len {
            let f = self.frame_pgnos.len() as u32;
            let pgno = self.frame_pgnos.pop().unwrap();
            if let Some(list) = self.page_frames.get_mut(&pgno) {
                if list.last() == Some(&f) {
                    list.pop();
                } else {
                    list.retain(|&x| x <= max_frame);
                }
                if list.is_empty() {
                    self.page_frames.remove(&pgno);
                }
            }
        }
    }

    fn lookup_page_in_range(&self, pgno: Pgno, min_frame: u32, max_frame: u32) -> Option<u32> {
        if pgno == 0 || max_frame == 0 || min_frame > max_frame {
            return None;
        }
        let frames = self.page_frames.get(&pgno)?;
        let idx = frames.partition_point(|&f| f <= max_frame);
        if idx == 0 {
            return None;
        }
        let candidate = frames[idx - 1];
        if candidate >= min_frame.max(1) {
            Some(candidate)
        } else {
            None
        }
    }

    fn read_frame_pgno(&self, frame_no: u32) -> Result<Pgno> {
        self.frame_pgnos
            .get((frame_no.saturating_sub(1)) as usize)
            .copied()
            .ok_or_else(|| SqliteError::corrupt("WAL frame out of bounds in heap index"))
    }
}

#[derive(Debug)]
enum ActiveShmLock {
    None,
    Reader(OwnedWalReadGuard),
    Writer(OwnedWriterTxnGuard),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TryBeginOutcome {
    Ok,
    Retry,
}

/// Dirty page descriptor passed into [`Wal::write_frames`].
pub struct WalFrameInput<'a> {
    pub pgno: Pgno,
    pub data: &'a [u8],
}

/// Per-connection WAL state (`struct Wal` in `sqlite/src/wal.c:515-550`).
pub struct Wal {
    vfs: Arc<dyn Vfs>,
    wal_path: PathBuf,
    wal_file: Option<Box<dyn VfsFile>>,
    shm: Option<Arc<WalShm>>,
    shm_options: ShmOpenOptions,
    active_lock: ActiveShmLock,
    heap_index: HeapWalIndex,
    exclusive_mode: WalExclusiveMode,
    read_only_flags: u8,
    immutable: bool,
    b_shm_unreliable: bool,
    sync_header: bool,
    pad_to_sector_boundary: bool,
    truncate_on_commit: bool,
    mx_wal_size: i64,
    sz_page: u32,
    n_ckpt: u32,
    hdr: WalIndexHeaderData,
    hdr_valid: bool,
    min_frame: u32,
    read_lock: Option<u8>,
    write_lock: bool,
    i_re_cksum: u32,
    i_callback: u32,
    write_hdr_scratch: Vec<[u8; WAL_FRAME_HDRSIZE]>,
    appended_pgnos_scratch: Vec<Pgno>,
    pending_append_pairs: Vec<(usize, usize)>,
}

impl std::fmt::Debug for Wal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wal")
            .field("wal_path", &self.wal_path)
            .field("exclusive_mode", &self.exclusive_mode)
            .field("read_only_flags", &self.read_only_flags)
            .field("b_shm_unreliable", &self.b_shm_unreliable)
            .field("sz_page", &self.sz_page)
            .field("n_ckpt", &self.n_ckpt)
            .field("read_lock", &self.read_lock)
            .field("write_lock", &self.write_lock)
            .finish_non_exhaustive()
    }
}

impl Wal {
    /// Opens `<db>-wal` (`sqlite3WalOpen`, `wal.c:1679-1777`).
    ///
    /// Note that `<db>-shm` is attached lazily on first header access (`walIndexPage`) when
    /// `exclusive_mode != WalExclusiveMode::HeapMemory`.
    pub fn open(
        vfs: Arc<dyn Vfs>,
        db_file: &dyn VfsFile,
        wal_path: &Path,
        no_shm: bool,
        mx_wal_size: i64,
        shm_options: ShmOpenOptions,
        immutable: bool,
    ) -> Result<Self> {
        let mut read_only_flags = if db_file.is_readonly() || immutable {
            WAL_RDONLY
        } else {
            WAL_RDWR
        };
        if shm_options.readonly_shm {
            read_only_flags |= WAL_SHM_RDONLY;
        }

        let wal_file = if immutable {
            if vfs.exists(wal_path)? {
                Some(vfs.open(Some(wal_path), OpenFlags::READONLY | OpenFlags::WAL)?)
            } else {
                None
            }
        } else {
            match vfs.open(
                Some(wal_path),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::WAL,
            ) {
                Ok(f) => {
                    if f.is_readonly() {
                        read_only_flags |= WAL_RDONLY;
                    }
                    Some(f)
                }
                Err(rw_err) => {
                    if vfs.exists(wal_path)? {
                        let ro = vfs.open(Some(wal_path), OpenFlags::READONLY | OpenFlags::WAL)?;
                        read_only_flags |= WAL_RDONLY;
                        Some(ro)
                    } else {
                        return Err(rw_err);
                    }
                }
            }
        };

        let dc = db_file.device_characteristics();
        let sync_header = (dc & IOCAP_SEQUENTIAL) == 0;
        let pad_to_sector_boundary = (dc & IOCAP_POWERSAFE_OVERWRITE) == 0;
        let exclusive_mode = if no_shm || immutable {
            WalExclusiveMode::HeapMemory
        } else {
            WalExclusiveMode::Normal
        };

        let initial_hdr =
            WalIndexHeaderData::new(4096, 0, 0, 0, cfg!(target_endian = "big"), [0, 0], [0, 0])?;

        Ok(Self {
            vfs,
            wal_path: wal_path.to_path_buf(),
            wal_file,
            shm: None,
            shm_options: ShmOpenOptions {
                readonly_shm: shm_options.readonly_shm,
                allow_unreliable_readonly: true,
            },
            active_lock: ActiveShmLock::None,
            heap_index: HeapWalIndex::default(),
            exclusive_mode,
            read_only_flags,
            immutable,
            b_shm_unreliable: false,
            sync_header,
            pad_to_sector_boundary,
            truncate_on_commit: false,
            mx_wal_size,
            sz_page: 0,
            n_ckpt: 0,
            hdr: initial_hdr,
            hdr_valid: false,
            min_frame: 0,
            read_lock: None,
            write_lock: false,
            i_re_cksum: 0,
            i_callback: 0,
            write_hdr_scratch: Vec::new(),
            appended_pgnos_scratch: Vec::new(),
            pending_append_pairs: Vec::new(),
        })
    }

    #[inline]
    pub const fn exclusive_mode(&self) -> WalExclusiveMode {
        self.exclusive_mode
    }

    #[inline]
    pub const fn is_heap_memory(&self) -> bool {
        matches!(self.exclusive_mode, WalExclusiveMode::HeapMemory)
    }

    #[inline]
    pub const fn is_readonly(&self) -> bool {
        self.read_only_flags != WAL_RDWR
    }

    #[inline]
    pub const fn is_shm_unreliable(&self) -> bool {
        self.b_shm_unreliable
    }

    #[inline]
    pub const fn read_lock_slot(&self) -> Option<u8> {
        self.read_lock
    }

    #[inline]
    pub const fn holds_write_lock(&self) -> bool {
        self.write_lock
    }

    #[inline]
    pub const fn header(&self) -> &WalIndexHeaderData {
        &self.hdr
    }

    #[inline]
    pub const fn min_frame(&self) -> u32 {
        self.min_frame
    }

    #[inline]
    pub const fn n_ckpt(&self) -> u32 {
        self.n_ckpt
    }

    /// Returns the database page count stored in the current WAL snapshot header (`sqlite3WalDbsize`),
    /// or `0` if the WAL contains no committed frames (`mx_frame == 0`).
    #[inline]
    pub const fn db_size(&self) -> Pgno {
        if self.hdr_valid && self.hdr.mx_frame > 0 {
            self.hdr.n_page
        } else {
            0
        }
    }

    /// Updates `mxWalSize` (`PRAGMA journal_size_limit`, `sqlite3WalLimit` in `wal.c:1782`).
    #[inline]
    pub fn set_journal_size_limit(&mut self, limit: i64) {
        self.mx_wal_size = limit;
    }

    /// Consumes and returns the post-commit frame count for `wal_autocheckpoint` (`sqlite3WalCallback`).
    pub fn take_callback_frame_count(&mut self) -> u32 {
        let v = self.i_callback;
        self.i_callback = 0;
        v
    }

    /// Inspects the underlying `Arc<WalShm>` if attached (for diagnostics and protocol tests).
    pub fn shm_handle(&self) -> Option<&Arc<WalShm>> {
        self.shm.as_ref()
    }

    fn ensure_shm_attached(&mut self, db_file: &dyn VfsFile) -> Result<Arc<WalShm>> {
        if let Some(ref existing) = self.shm {
            return Ok(Arc::clone(existing));
        }
        let opened = db_file.open_wal_shm_with_options(self.shm_options)?;
        if opened.is_readonly() {
            self.read_only_flags |= WAL_SHM_RDONLY;
        }
        let arc = Arc::new(opened);
        self.shm = Some(Arc::clone(&arc));
        Ok(arc)
    }

    /// Transitions between `WAL_NORMAL_MODE` and `WAL_EXCLUSIVE_MODE` (`sqlite3WalExclusiveMode`,
    /// `wal.c:4558-4595`):
    /// - `op < 0`: queries whether WAL is currently in `Normal` mode (meaning caller must acquire
    ///   `.db` `EXCLUSIVE` lock and call `exclusive_mode_transition(1)`).
    /// - `op == 0`: transitions `Exclusive -> Normal`, re-acquiring OS `SHARED WAL_READ_LOCK(slot)`.
    /// - `op > 0`: transitions `Normal -> Exclusive` (caller ALREADY holds `.db` `EXCLUSIVE`),
    ///   releasing OS `SHARED WAL_READ_LOCK(slot)`.
    pub fn exclusive_mode_transition(&mut self, op: i32) -> Result<bool> {
        if self.write_lock {
            return Err(SqliteError::misuse(
                "sqlite3WalExclusiveMode called while writeLock is held",
            ));
        }
        if op < 0 {
            return Ok(self.exclusive_mode == WalExclusiveMode::Normal);
        }
        if op == 0 {
            if self.exclusive_mode == WalExclusiveMode::Exclusive {
                if let ActiveShmLock::Reader(ref mut rg) = self.active_lock {
                    if rg.exit_exclusive_mode().is_ok() {
                        self.exclusive_mode = WalExclusiveMode::Normal;
                        return Ok(true);
                    }
                    return Ok(false);
                }
                self.exclusive_mode = WalExclusiveMode::Normal;
                return Ok(true);
            }
            return Ok(false);
        }

        // `op > 0`: `Normal -> Exclusive`
        if self.exclusive_mode == WalExclusiveMode::Normal {
            if let ActiveShmLock::Reader(ref mut rg) = self.active_lock {
                rg.enter_exclusive_mode()?;
            }
            self.exclusive_mode = WalExclusiveMode::Exclusive;
            return Ok(true);
        }
        Ok(false)
    }

    // ========================================================================
    // A. Read Snapshot Acquisition (`sqlite3WalBeginReadTransaction` / `walTryBeginRead`)
    // ========================================================================

    /// Begins a WAL read snapshot (`sqlite3WalBeginReadTransaction`, `wal.c:3369-3400`), returning
    /// `Ok(changed)` where `changed == true` instructs the pager to discard clean cached pages.
    pub fn begin_read_transaction(&mut self, db_file: &dyn VfsFile) -> Result<bool> {
        let mut changed = false;
        let mut cnt = 0u32;
        loop {
            match self.wal_try_begin_read(db_file, &mut changed, false, &mut cnt)? {
                TryBeginOutcome::Ok => return Ok(changed),
                TryBeginOutcome::Retry => continue,
            }
        }
    }

    /// Ends the current WAL read snapshot (`sqlite3WalEndReadTransaction`, `wal.c:3409-3416`).
    #[inline]
    pub fn end_read_transaction(&mut self) -> Result<()> {
        if !self.write_lock && matches!(self.active_lock, ActiveShmLock::None) {
            self.read_lock = None;
            return Ok(());
        }
        self.end_write_transaction()?;
        let prev = std::mem::replace(&mut self.active_lock, ActiveShmLock::None);
        let res = match prev {
            ActiveShmLock::Reader(rg) => rg.unlock(),
            ActiveShmLock::Writer(wg) => wg.unlock_all(),
            ActiveShmLock::None => Ok(()),
        };
        self.read_lock = None;
        res
    }

    fn retry_sleep_if_needed(&self, cnt: &mut u32) -> Result<()> {
        *cnt = cnt.saturating_add(1);
        if *cnt >= WAL_RETRY_PROTOCOL_LIMIT {
            return Err(SqliteError::new(
                ErrorCode::Protocol,
                "locking protocol retry limit exceeded in walTryBeginRead",
            ));
        }
        let mut delay_us: u64 = 1;
        if *cnt >= 10 {
            let d = u64::from(*cnt - 9);
            delay_us = d.saturating_mul(d).saturating_mul(39);
        }
        if *cnt >= 5 {
            self.vfs.sleep(delay_us);
        }
        Ok(())
    }

    fn wal_try_begin_read(
        &mut self,
        db_file: &dyn VfsFile,
        changed: &mut bool,
        use_wal: bool,
        cnt: &mut u32,
    ) -> Result<TryBeginOutcome> {
        self.retry_sleep_if_needed(cnt)?;

        // Heap-memory mode (`locking_mode=EXCLUSIVE` at open or `immutable=1`).
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            if !use_wal && self.heap_index.hdr.is_none() {
                self.recover_into_heap_index()?;
                *changed = true;
            }
            if let Some(h) = self.heap_index.hdr {
                self.hdr = h;
                self.hdr_valid = true;
            }
            if !use_wal && self.heap_index.ckpt.n_backfill == self.hdr.mx_frame {
                self.read_lock = Some(0);
                self.min_frame = 0;
            } else {
                self.heap_index.ckpt.read_marks[1] = self.hdr.mx_frame;
                self.read_lock = Some(1);
                self.min_frame = self.heap_index.ckpt.n_backfill + 1;
            }
            return Ok(TryBeginOutcome::Ok);
        }

        // Fast steady-state path (`!use_wal`, reliable `-shm` attached, no active lock):
        // combines raw 12-word header comparison, `WalCkptInfo` read, `WAL_READ_LOCK(slot)`,
        // and single-barrier post-lock verification without redundant locks or checksums.
        if !use_wal
            && !self.b_shm_unreliable
            && matches!(self.active_lock, ActiveShmLock::None)
            && let Some(ref shm_arc) = self.shm
        {
            let known_hdr = self.hdr_valid.then_some(&self.hdr);
            let exclusive_db = self.exclusive_mode == WalExclusiveMode::Exclusive;
            match WalShm::try_fast_begin_read_snapshot(shm_arc, known_hdr, exclusive_db)? {
                FastBeginReadOutcome::Acquired {
                    guard,
                    slot,
                    min_frame,
                    live_hdr,
                } => {
                    if let Some(loaded) = live_hdr {
                        self.apply_loaded_header(loaded);
                        *changed = true;
                    }
                    self.read_lock = Some(slot);
                    self.min_frame = min_frame;
                    self.active_lock = ActiveShmLock::Reader(guard);
                    return Ok(TryBeginOutcome::Ok);
                }
                FastBeginReadOutcome::Retry { live_hdr } => {
                    if let Some(loaded) = live_hdr {
                        self.apply_loaded_header(loaded);
                        *changed = true;
                    }
                    return Ok(TryBeginOutcome::Retry);
                }
                FastBeginReadOutcome::FallbackSlowPath => {}
            }
        }

        // Step A.1: `!use_wal` reads/recovers `WalIndexHdr` (or validates `b_shm_unreliable`).
        let mut prefetched_ckpt: Option<WalCkptInfoSnapshot> = None;
        if !use_wal {
            if !self.b_shm_unreliable {
                match self.wal_index_read_hdr(db_file, changed)? {
                    (TryBeginOutcome::Ok, snap) => {
                        prefetched_ckpt = snap;
                    }
                    (TryBeginOutcome::Retry, _) => return Ok(TryBeginOutcome::Retry),
                }
            }
            if self.b_shm_unreliable {
                return self.wal_begin_shm_unreliable(db_file, changed);
            }
        }

        if self.shm.is_none() {
            let _ = self.ensure_shm_attached(db_file)?;
        }
        let shm_ref = self.shm.as_ref().expect("shm attached");
        let ckpt_info = match &self.active_lock {
            ActiveShmLock::Writer(wg) => wg.read_ckpt_info()?,
            _ => match prefetched_ckpt {
                Some(info) => info,
                None => {
                    let Some((_, info)) = shm_ref.try_read_header_and_ckpt_info()? else {
                        return Ok(TryBeginOutcome::Retry);
                    };
                    info
                }
            },
        };

        let exclusive_db = self.exclusive_mode == WalExclusiveMode::Exclusive;

        // Step A.2 (`!use_wal && nBackfill == hdr.mx_frame`): acquire `READ(0)` and re-verify header.
        if !use_wal && ckpt_info.n_backfill == self.hdr.mx_frame {
            match WalShm::acquire_owned_read_slot_with_mark(shm_ref, 0, None, exclusive_db) {
                Ok(rg) => {
                    if !rg.verify_post_lock(None, &self.hdr)? {
                        rg.unlock()?;
                        return Ok(TryBeginOutcome::Retry);
                    }
                    self.read_lock = Some(0);
                    self.min_frame = 0;
                    self.active_lock = ActiveShmLock::Reader(rg);
                    return Ok(TryBeginOutcome::Ok);
                }
                Err(e) if e.code == ErrorCode::Busy => {
                    // Fall through to `READ(1..=4)` selection just like `wal.c:3246-3249`!
                }
                Err(other) => return Err(other),
            }
        }

        // Step A.3: Select best existing `aReadMark[1..=4] <= mx_frame` (`0` IS a valid mark,
        // while `READMARK_NOT_USED` (`0xffff_ffff`) is NEVER selectable even if `mx_frame == u32::MAX`!).
        let mx_frame = self.hdr.mx_frame;
        let mut mx_read_mark = 0u32;
        let mut mx_i: u8 = 0;
        for slot in 1..(WAL_NREADER as u8) {
            let this_mark = ckpt_info.read_marks[usize::from(slot)];
            if this_mark != READMARK_NOT_USED && mx_read_mark <= this_mark && this_mark <= mx_frame
            {
                mx_read_mark = this_mark;
                mx_i = slot;
            }
        }

        let shm_readonly = (self.read_only_flags & WAL_SHM_RDONLY) != 0;
        let mut saw_busy = false;

        if !shm_readonly && (mx_read_mark < mx_frame || mx_i == 0) {
            // Step A.3a (`wal.c:3275-3286`): Claim `aReadMark[slot] = mx_frame` under brief
            // `EXCLUSIVE WAL_READ_LOCK(slot)` first (separate from acquiring `SHARED` below!).
            for slot in 1..(WAL_NREADER as u8) {
                let claimed_slot = match &mut self.active_lock {
                    ActiveShmLock::Writer(wg) => {
                        wg.try_claim_positive_read_mark_under_write(slot, mx_frame)?
                    }
                    ActiveShmLock::None => {
                        shm_ref.try_claim_read_mark(slot, mx_frame, exclusive_db)?
                    }
                    ActiveShmLock::Reader(_) => {
                        return Err(SqliteError::misuse(
                            "unexpected active Reader guard inside wal_try_begin_read",
                        ));
                    }
                };
                if claimed_slot {
                    mx_read_mark = mx_frame;
                    mx_i = slot;
                    break;
                }
                saw_busy = true;
            }
        }

        if mx_i == 0 {
            return if !shm_readonly && saw_busy {
                Ok(TryBeginOutcome::Retry)
            } else {
                Err(SqliteError::with_extended(
                    extended::SQLITE_READONLY_CANTINIT,
                    "no usable WAL read-mark slot available",
                ))
            };
        }

        // Step A.3b (`wal.c:3294-3306`): Acquire `SHARED WAL_READ_LOCK(mx_i)` as a distinct step.
        let lock_res = match &mut self.active_lock {
            ActiveShmLock::Writer(wg) => wg.acquire_positive_read_slot_under_write(mx_i, None),
            ActiveShmLock::None => {
                match WalShm::acquire_owned_read_slot_with_mark(shm_ref, mx_i, None, exclusive_db) {
                    Ok(rg) => {
                        self.active_lock = ActiveShmLock::Reader(rg);
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            ActiveShmLock::Reader(_) => {
                return Err(SqliteError::misuse(
                    "unexpected active Reader guard before shared read lock",
                ));
            }
        };
        if let Err(err) = lock_res {
            if err.code == ErrorCode::Busy {
                return Ok(TryBeginOutcome::Retry);
            }
            return Err(err);
        }

        // Step A.4: Read `nBackfill + 1`, `shm_barrier()`, verify `aReadMark[mx_i] == mx_read_mark` & `WalIndexHdr == self.hdr`.
        let (post_ckpt, verified) = match &self.active_lock {
            ActiveShmLock::Reader(rg) => {
                let info = rg.read_ckpt_info()?;
                let ok = rg.verify_post_lock(Some(mx_read_mark), &self.hdr)?;
                (info, ok)
            }
            ActiveShmLock::Writer(wg) => {
                let info = wg.read_ckpt_info()?;
                let ok = wg.verify_post_read_lock(Some(mx_read_mark), &self.hdr)?;
                (info, ok)
            }
            ActiveShmLock::None => unreachable!(),
        };

        if !verified {
            match std::mem::replace(&mut self.active_lock, ActiveShmLock::None) {
                ActiveShmLock::Reader(rg) => {
                    rg.unlock()?;
                }
                ActiveShmLock::Writer(mut wg) => {
                    wg.release_positive_read_slot_under_write()?;
                    self.active_lock = ActiveShmLock::Writer(wg);
                }
                ActiveShmLock::None => {}
            }
            return Ok(TryBeginOutcome::Retry);
        }

        self.min_frame = post_ckpt.n_backfill + 1;
        self.read_lock = Some(mx_i);
        Ok(TryBeginOutcome::Ok)
    }

    /// Reads and validates `WalIndexHdr` (`walIndexReadHdr`, `wal.c:2743-2845`), running
    /// `walIndexRecover` under `WAL_WRITE_LOCK` if uninitialized/corrupt.
    fn wal_index_read_hdr(
        &mut self,
        db_file: &dyn VfsFile,
        changed: &mut bool,
    ) -> Result<(TryBeginOutcome, Option<WalCkptInfoSnapshot>)> {
        if self.shm.is_none() {
            let _ = self.ensure_shm_attached(db_file)?;
        }
        let hdr_probe = self
            .shm
            .as_ref()
            .expect("shm attached")
            .try_read_header_and_ckpt_info();
        match hdr_probe {
            Ok(Some((live_hdr, ckpt_info))) => {
                if !self.hdr_valid || live_hdr != self.hdr {
                    self.apply_loaded_header(live_hdr);
                    *changed = true;
                }
                return Ok((TryBeginOutcome::Ok, Some(ckpt_info)));
            }
            Ok(None) => {
                // Header uninitialized or torn write; fall through to `badHdr` handling.
            }
            Err(e) if e.extended_code == extended::SQLITE_READONLY_CANTINIT => {
                // Enter `b_shm_unreliable` mode (`wal.c:2754-2766, 2806-2842`)!
                self.b_shm_unreliable = true;
                *changed = true;
                match self.recover_into_heap_index() {
                    Ok(()) => {
                        if let Some(h) = self.heap_index.hdr {
                            self.apply_loaded_header(h);
                        }
                        return Ok((TryBeginOutcome::Ok, None));
                    }
                    Err(rec_err) => {
                        self.b_shm_unreliable = false;
                        self.heap_index = HeapWalIndex::default();
                        if rec_err.extended_code == extended::SQLITE_IOERR_SHORT_READ {
                            return Ok((TryBeginOutcome::Retry, None));
                        }
                        return Err(rec_err);
                    }
                }
            }
            Err(e) if e.code == ErrorCode::Busy => {
                return Ok((TryBeginOutcome::Retry, None));
            }
            Err(other) => return Err(other),
        }

        let shm = self.ensure_shm_attached(db_file)?;

        // `badHdr == true` on reliable `-shm`.
        let shm_readonly = (self.read_only_flags & WAL_SHM_RDONLY) != 0;
        if shm_readonly {
            return match shm.probe_readonly_bad_header()? {
                ReadonlyHeaderProbe::ReadonlyRecovery => Err(SqliteError::with_extended(
                    extended::SQLITE_READONLY_RECOVERY,
                    "cannot recover WAL-index on read-only shared memory",
                )),
                ReadonlyHeaderProbe::WriterActiveRetry => Ok((TryBeginOutcome::Retry, None)),
                ReadonlyHeaderProbe::BusyRecovery => Err(SqliteError::with_extended(
                    extended::SQLITE_BUSY_RECOVERY,
                    "database is being recovered by another process",
                )),
            };
        }

        // Writable `-shm`: acquire `EXCLUSIVE WAL_WRITE_LOCK`, extend region 0, retry header, else recover!
        let mut init_write = match WalShm::acquire_owned_init_write(&shm) {
            Ok(g) => g,
            Err(e) if e.code == ErrorCode::Busy => {
                return match shm.probe_recover_lock()? {
                    ReadonlyHeaderProbe::WriterActiveRetry
                    | ReadonlyHeaderProbe::ReadonlyRecovery => Ok((TryBeginOutcome::Retry, None)),
                    ReadonlyHeaderProbe::BusyRecovery => Err(SqliteError::with_extended(
                        extended::SQLITE_BUSY_RECOVERY,
                        "database is being recovered by another process",
                    )),
                };
            }
            Err(other) => return Err(other),
        };

        let mut post_ckpt: Option<WalCkptInfoSnapshot> = None;
        let res: Result<TryBeginOutcome> = match init_write.extend_and_try_read_header()? {
            Some((live_hdr, info)) => {
                if !self.hdr_valid || live_hdr != self.hdr {
                    self.apply_loaded_header(live_hdr);
                    *changed = true;
                }
                post_ckpt = Some(info);
                Ok(TryBeginOutcome::Ok)
            }
            None => {
                let recovered = self.recover_into_shm_from_init_write(&mut init_write)?;
                self.apply_loaded_header(recovered);
                *changed = true;
                Ok(TryBeginOutcome::Ok)
            }
        };
        let release_res = init_write.release();
        res?;
        release_res?;
        Ok((TryBeginOutcome::Ok, post_ckpt))
    }

    /// Validates private heap WAL-index under `SHARED WAL_READ_LOCK(0)` (`walBeginShmUnreliable`,
    /// `wal.c:2874-3019`).
    fn wal_begin_shm_unreliable(
        &mut self,
        db_file: &dyn VfsFile,
        changed: &mut bool,
    ) -> Result<TryBeginOutcome> {
        let shm = self.ensure_shm_attached(db_file)?;
        let rg = match WalShm::acquire_owned_read_slot_with_mark(&shm, 0, None, false) {
            Ok(g) => g,
            Err(e) if e.code == ErrorCode::Busy => {
                self.discard_unreliable_heap_index(changed)?;
                return Ok(TryBeginOutcome::Retry);
            }
            Err(other) => {
                self.discard_unreliable_heap_index(changed)?;
                return Err(other);
            }
        };
        self.active_lock = ActiveShmLock::Reader(rg);
        self.read_lock = Some(0);
        self.min_frame = 1;

        // Re-probe DMS: if a writer attached and made DMS `ActiveShared`, switch back to `-shm`!
        match shm.reprobe_dms_ready() {
            Ok(ShmDmsState::ReadonlyCantInit) => {}
            Ok(ShmDmsState::ActiveShared) => {
                self.discard_unreliable_heap_index(changed)?;
                return Ok(TryBeginOutcome::Retry);
            }
            Err(err) => {
                self.discard_unreliable_heap_index(changed)?;
                return Err(err);
            }
        }

        if let Some(h) = self.heap_index.hdr {
            self.hdr = h;
            self.hdr_valid = true;
        }

        let Some(ref wal_fd) = self.wal_file else {
            *changed = true;
            return Ok(TryBeginOutcome::Ok);
        };

        let sz_wal = match wal_fd.file_size() {
            Ok(sz) => sz,
            Err(e) => {
                self.discard_unreliable_heap_index(changed)?;
                return Err(e);
            }
        };
        if sz_wal < WAL_HDRSIZE as u64 {
            *changed = true;
            if self.hdr.mx_frame == 0 {
                return Ok(TryBeginOutcome::Ok);
            }
            self.discard_unreliable_heap_index(changed)?;
            return Ok(TryBeginOutcome::Retry);
        }

        let mut hdr_buf = [0u8; WAL_HDRSIZE];
        let n_hdr = match wal_fd.read_at(&mut hdr_buf, 0) {
            Ok(n) => n,
            Err(e) => {
                self.discard_unreliable_heap_index(changed)?;
                return Err(e);
            }
        };
        if n_hdr < WAL_HDRSIZE {
            self.discard_unreliable_heap_index(changed)?;
            return Ok(TryBeginOutcome::Retry);
        }
        let disk_s0 = u32::from_be_bytes([hdr_buf[16], hdr_buf[17], hdr_buf[18], hdr_buf[19]]);
        let disk_s1 = u32::from_be_bytes([hdr_buf[20], hdr_buf[21], hdr_buf[22], hdr_buf[23]]);
        if [disk_s0, disk_s1] != self.hdr.salt {
            self.discard_unreliable_heap_index(changed)?;
            return Ok(TryBeginOutcome::Retry);
        }

        // Scan any frames appended after `self.hdr.mx_frame` to see if a new commit (`nTruncate != 0`) landed.
        if self.sz_page >= 512 {
            let sz_frame = (WAL_FRAME_HDRSIZE as u64) + u64::from(self.sz_page);
            let mut offset = wal_frame_offset(self.hdr.mx_frame + 1, self.sz_page);
            let mut running_cksum = self.hdr.frame_cksum;
            let mut frame_hdr = [0u8; WAL_FRAME_HDRSIZE];
            let mut page_buf = vec![0u8; self.sz_page as usize];

            while offset + sz_frame <= sz_wal {
                let n1 = match wal_fd.read_at(&mut frame_hdr, offset) {
                    Ok(n) => n,
                    Err(e) => {
                        self.discard_unreliable_heap_index(changed)?;
                        return Err(e);
                    }
                };
                let n2 = match wal_fd.read_at(&mut page_buf, offset + (WAL_FRAME_HDRSIZE as u64)) {
                    Ok(n) => n,
                    Err(e) => {
                        self.discard_unreliable_heap_index(changed)?;
                        return Err(e);
                    }
                };
                if n1 < WAL_FRAME_HDRSIZE || n2 < page_buf.len() {
                    break;
                }
                let Some((_pgno, n_truncate, next_cksum)) = wal_decode_frame(
                    &frame_hdr,
                    &page_buf,
                    self.hdr.salt,
                    self.hdr.big_end_cksum != 0,
                    running_cksum,
                )?
                else {
                    break;
                };
                running_cksum = next_cksum;
                if n_truncate != 0 {
                    self.discard_unreliable_heap_index(changed)?;
                    return Ok(TryBeginOutcome::Retry);
                }
                offset += sz_frame;
            }
        }

        Ok(TryBeginOutcome::Ok)
    }

    fn discard_unreliable_heap_index(&mut self, changed: &mut bool) -> Result<()> {
        self.b_shm_unreliable = false;
        self.heap_index = HeapWalIndex::default();
        *changed = true;
        self.end_read_transaction()
    }

    fn apply_loaded_header(&mut self, loaded: WalIndexHeaderData) {
        self.hdr = loaded;
        self.hdr_valid = true;
        if loaded.mx_frame > 0 {
            self.sz_page = loaded.page_size;
        }
    }

    // ========================================================================
    // Page Lookup & Frame Read (`sqlite3WalFindFrame` / `sqlite3WalReadFrame`)
    // ========================================================================

    /// Finds the latest WAL frame in `[min_frame, hdr.mx_frame]` containing `pgno`
    /// (`sqlite3WalFindFrame`, `wal.c:3607-3709`).
    pub fn find_frame(&self, pgno: Pgno) -> Result<Option<u32>> {
        let i_last = self.hdr.mx_frame;
        if !self.hdr_valid || i_last == 0 || (self.read_lock == Some(0) && !self.b_shm_unreliable) {
            return Ok(None);
        }
        if self.exclusive_mode == WalExclusiveMode::HeapMemory || self.b_shm_unreliable {
            let min_f = if self.b_shm_unreliable {
                1
            } else {
                self.min_frame
            };
            return Ok(self.heap_index.lookup_page_in_range(pgno, min_f, i_last));
        }

        match &self.active_lock {
            ActiveShmLock::Reader(rg) => rg.lookup_page_in_range(pgno, self.min_frame, i_last),
            ActiveShmLock::Writer(wg) => wg.lookup_page_in_range(pgno, self.min_frame, i_last),
            ActiveShmLock::None => Err(SqliteError::misuse(
                "find_frame called without an active WAL read or write snapshot",
            )),
        }
    }

    /// Reads `buf.len()` bytes of frame `frame_no`'s payload from `-wal` (`sqlite3WalReadFrame`,
    /// `wal.c:3723-3760`).
    pub fn read_frame(&self, frame_no: u32, buf: &mut [u8]) -> Result<()> {
        let wal_fd = self
            .wal_file
            .as_ref()
            .ok_or_else(|| SqliteError::corrupt("cannot read WAL frame without open -wal file"))?;
        let sz_page = if self.sz_page != 0 {
            self.sz_page
        } else {
            self.hdr.page_size
        };
        let offset = wal_frame_offset(frame_no, sz_page) + (WAL_FRAME_HDRSIZE as u64);
        let read_len = buf.len().min(sz_page as usize);
        let n = wal_fd.read_at(&mut buf[..read_len], offset)?;
        if n < read_len {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHORT_READ,
                format!("short read on WAL frame {frame_no} ({n} < {read_len})"),
            ));
        }
        if buf.len() > read_len {
            buf[read_len..].fill(0);
        }
        Ok(())
    }

    // ========================================================================
    // B. Write Transaction, `walRestartLog`, Spill/Commit (`walFrames`), Undo & Savepoints
    // ========================================================================

    /// Upgrades the active WAL read snapshot to a write transaction (`sqlite3WalBeginWriteTransaction`,
    /// `wal.c:3785-3835`).
    ///
    /// Returns `SQLITE_BUSY_SNAPSHOT` (releasing `WAL_WRITE_LOCK` while preserving the read snapshot!)
    /// if another writer committed after this connection's read snapshot was established.
    pub fn begin_write_transaction(&mut self) -> Result<()> {
        if self.read_only_flags != WAL_RDWR || self.immutable {
            return Err(SqliteError::readonly(
                "cannot write to a read-only WAL or shared-memory handle",
            ));
        }
        if self.write_lock {
            return Err(SqliteError::misuse(
                "WAL write transaction is already active",
            ));
        }
        if self.read_lock.is_none() {
            return Err(SqliteError::misuse(
                "begin_write_transaction requires an active WAL read snapshot",
            ));
        }

        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            self.write_lock = true;
            self.i_re_cksum = 0;
            return Ok(());
        }

        let prev = std::mem::replace(&mut self.active_lock, ActiveShmLock::None);
        let ActiveShmLock::Reader(rg) = prev else {
            self.active_lock = prev;
            return Err(SqliteError::misuse(
                "expected OwnedWalReadGuard when starting WAL write transaction",
            ));
        };

        let wg = match rg.upgrade_to_writer() {
            Ok(w) => w,
            Err((restored_rg, err)) => {
                self.active_lock = ActiveShmLock::Reader(restored_rg);
                return Err(err);
            }
        };

        // Stale snapshot detection (`wal.c:3821-3830`)!
        let live_hdr = match wg.read_header_verified() {
            Ok(h) => h,
            Err(e) => {
                if let Ok(rg_back) = wg.downgrade_to_reader() {
                    self.active_lock = ActiveShmLock::Reader(rg_back);
                }
                return Err(e);
            }
        };
        if live_hdr != self.hdr {
            let rg_back = wg.downgrade_to_reader()?;
            self.active_lock = ActiveShmLock::Reader(rg_back);
            return Err(SqliteError::with_extended(
                extended::SQLITE_BUSY_SNAPSHOT,
                "cannot upgrade stale WAL read snapshot to write transaction",
            ));
        }

        self.active_lock = ActiveShmLock::Writer(wg);
        self.write_lock = true;
        self.i_re_cksum = 0;
        Ok(())
    }

    /// Releases `WAL_WRITE_LOCK` while retaining the active read snapshot (`sqlite3WalEndWriteTransaction`,
    /// `wal.c:3843-3852`).
    pub fn end_write_transaction(&mut self) -> Result<()> {
        if !self.write_lock {
            return Ok(());
        }
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            self.write_lock = false;
            self.i_re_cksum = 0;
            self.truncate_on_commit = false;
            return Ok(());
        }

        let prev = std::mem::replace(&mut self.active_lock, ActiveShmLock::None);
        match prev {
            ActiveShmLock::Writer(wg) => {
                let rg = wg.downgrade_to_reader()?;
                self.read_lock = Some(rg.slot());
                self.active_lock = ActiveShmLock::Reader(rg);
                self.write_lock = false;
                self.i_re_cksum = 0;
                self.truncate_on_commit = false;
                Ok(())
            }
            other => {
                self.active_lock = other;
                self.write_lock = false;
                Ok(())
            }
        }
    }

    /// Implements `walRestartLog` (`wal.c:3961-4001`):
    /// when `read_lock == Some(0)`, optionally resets the WAL header if `nBackfill > 0` and no
    /// readers hold `READ(1..=4)`, then drops `READ(0)` and runs `wal_try_begin_read(..., use_wal=true)`
    /// while `WAL_WRITE_LOCK` remains held so `read_lock` transitions to `Some(1..=4)`!
    fn wal_restart_log(&mut self, db_file: &dyn VfsFile) -> Result<()> {
        if self.read_lock != Some(0) {
            return Ok(());
        }

        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            if self.heap_index.ckpt.n_backfill > 0 {
                let mut rand4 = [0u8; 4];
                self.vfs.randomness(&mut rand4);
                let salt1 = u32::from_be_bytes(rand4);
                self.n_ckpt = self.n_ckpt.wrapping_add(1);
                self.hdr.mx_frame = 0;
                self.hdr.salt[0] = self.hdr.salt[0].wrapping_add(1);
                self.hdr.salt[1] = salt1;
                self.heap_index.hdr = Some(self.hdr);
                self.heap_index.ckpt.n_backfill = 0;
                self.heap_index.ckpt.n_backfill_attempted = 0;
                self.heap_index.ckpt.read_marks = [
                    0,
                    0,
                    READMARK_NOT_USED,
                    READMARK_NOT_USED,
                    READMARK_NOT_USED,
                ];
                self.heap_index.reset_empty();
            }
            self.heap_index.ckpt.read_marks[1] = self.hdr.mx_frame;
            self.read_lock = Some(1);
            self.min_frame = self.heap_index.ckpt.n_backfill + 1;
            return Ok(());
        }

        let ckpt_n_backfill = match &self.active_lock {
            ActiveShmLock::Writer(wg) => wg.read_ckpt_info()?.n_backfill,
            _ => {
                return Err(SqliteError::misuse(
                    "walRestartLog requires an active OwnedWriterTxnGuard",
                ));
            }
        };

        if ckpt_n_backfill > 0 {
            let mut rand4 = [0u8; 4];
            self.vfs.randomness(&mut rand4);
            let salt1 = u32::from_be_bytes(rand4);
            let restarted_opt = match &mut self.active_lock {
                ActiveShmLock::Writer(wg) => {
                    wg.try_reset_header_if_no_positive_readers(&self.hdr, salt1)?
                }
                _ => unreachable!(),
            };
            if let Some(restarted_hdr) = restarted_opt {
                self.n_ckpt = self.n_ckpt.wrapping_add(1);
                self.hdr = restarted_hdr;
            }
        }

        // Drop `READ(0)` while keeping `WAL_WRITE_LOCK` (`wal.c:3984-3985`).
        if let ActiveShmLock::Writer(ref mut wg) = self.active_lock {
            wg.release_read_slot_zero_under_write()?;
        }
        self.read_lock = None;

        // Loop `wal_try_begin_read(..., use_wal = true)` to claim & lock a positive `READ(1..=4)` slot!
        let mut unused_changed = false;
        let mut cnt = 0u32;
        loop {
            match self.wal_try_begin_read(db_file, &mut unused_changed, true, &mut cnt) {
                Ok(TryBeginOutcome::Ok) => {
                    debug_assert!(matches!(self.read_lock, Some(1..=4)));
                    return Ok(());
                }
                Ok(TryBeginOutcome::Retry) => continue,
                Err(err) => {
                    // Clean up `WAL_WRITE_LOCK` on fatal error (`wal.c:3995-3996`).
                    self.write_lock = false;
                    self.read_lock = None;
                    if let ActiveShmLock::Writer(wg) =
                        std::mem::replace(&mut self.active_lock, ActiveShmLock::None)
                    {
                        let _ = wg.unlock_all();
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Writes dirty pages into `-wal` (`sqlite3WalFrames` / `walFrames`, `wal.c:4124-4352`).
    ///
    /// - When `is_commit == false`: `n_truncate` must be `0` (mid-transaction cache spill).
    /// - When `is_commit == true`: `n_truncate` must be `> 0` (`database_size_pages` after commit).
    #[allow(clippy::too_many_arguments)]
    pub fn write_frames(
        &mut self,
        db_file: &dyn VfsFile,
        page_size: u32,
        frames: &[WalFrameInput<'_>],
        n_truncate: Pgno,
        is_commit: bool,
        wal_sync_flags: Option<SyncFlags>,
        ckpt_sync_flags: Option<SyncFlags>,
    ) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        if !self.write_lock {
            return Err(SqliteError::misuse("write_frames requires WAL write_lock"));
        }
        if is_commit != (n_truncate != 0) {
            return Err(SqliteError::misuse(
                "write_frames invariant violated: is_commit must match (n_truncate != 0)",
            ));
        }

        let live_hdr_before = self.read_live_committed_header()?;
        let mut i_first = match live_hdr_before {
            Some(live) if live != self.hdr => live.mx_frame + 1,
            _ => 0,
        };

        self.wal_restart_log(db_file)?;
        if self.hdr.mx_frame == 0 {
            i_first = 0;
        }

        // Write 32-byte WAL file header when starting at frame 0 (`wal.c:4173-4211`).
        let mut i_frame = self.hdr.mx_frame;
        let mut pending_file_hdr: Option<[u8; WAL_HDRSIZE]> = None;
        if i_frame == 0 {
            if self.n_ckpt == 0 {
                let mut salt_bytes = [0u8; 8];
                self.vfs.randomness(&mut salt_bytes);
                self.hdr.salt = [
                    u32::from_be_bytes([
                        salt_bytes[0],
                        salt_bytes[1],
                        salt_bytes[2],
                        salt_bytes[3],
                    ]),
                    u32::from_be_bytes([
                        salt_bytes[4],
                        salt_bytes[5],
                        salt_bytes[6],
                        salt_bytes[7],
                    ]),
                ];
            }
            let native_be = cfg!(target_endian = "big");
            let (hdr_bytes, file_hdr) =
                WalFileHeader::encode(page_size, self.n_ckpt, self.hdr.salt, native_be)?;

            let wal_fd = self.wal_file.as_ref().ok_or_else(|| {
                SqliteError::new(ErrorCode::CantOpen, "missing writable -wal handle")
            })?;

            self.sz_page = page_size;
            self.hdr.page_size = page_size;
            self.hdr.big_end_cksum = u8::from(file_hdr.big_end_cksum);
            self.hdr.frame_cksum = file_hdr.cksum;
            self.hdr_valid = true;
            self.truncate_on_commit = true;

            if self.sync_header
                && let Some(c_sync) = ckpt_sync_flags
            {
                wal_fd.write_at(&hdr_bytes, 0)?;
                wal_fd.sync(c_sync)?;
            } else {
                pending_file_hdr = Some(hdr_bytes);
            }
        }

        if self.sz_page != page_size {
            return Err(SqliteError::corrupt(format!(
                "WAL page size mismatch ({} vs {page_size})",
                self.sz_page
            )));
        }

        if self.wal_file.is_none() {
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                "missing writable -wal handle",
            ));
        }
        let sz_frame = (WAL_FRAME_HDRSIZE as u64) + u64::from(page_size);
        let mut i_offset = wal_frame_offset(i_frame + 1, page_size);
        let mut run_start_offset = if pending_file_hdr.is_some() {
            0
        } else {
            i_offset
        };
        let mut last_idx: Option<usize> = None;

        self.write_hdr_scratch.clear();
        if self.write_hdr_scratch.capacity() < frames.len() {
            self.write_hdr_scratch
                .reserve(frames.len() - self.write_hdr_scratch.capacity());
        }
        self.appended_pgnos_scratch.clear();
        if self.appended_pgnos_scratch.capacity() < frames.len() + 2 {
            self.appended_pgnos_scratch
                .reserve(frames.len() + 2 - self.appended_pgnos_scratch.capacity());
        }
        self.pending_append_pairs.clear();
        if self.pending_append_pairs.capacity() < frames.len() {
            self.pending_append_pairs
                .reserve(frames.len() - self.pending_append_pairs.capacity());
        }

        let flush_append_run = |wal_fd: &dyn VfsFile,
                                pending_hdr: &mut Option<[u8; WAL_HDRSIZE]>,
                                hdr_scratch: &[[u8; WAL_FRAME_HDRSIZE]],
                                pairs: &mut Vec<(usize, usize)>,
                                start_off: u64|
         -> Result<()> {
            if pending_hdr.is_none() && pairs.is_empty() {
                return Ok(());
            }
            if pairs.is_empty() {
                let hb = pending_hdr.take().unwrap();
                return wal_fd.write_at(&hb, 0);
            }
            let hdr_ref = pending_hdr.as_ref();
            let mut iovs: Vec<IoSlice<'_>> =
                Vec::with_capacity(usize::from(hdr_ref.is_some()) + pairs.len() * 2);
            if let Some(hb) = hdr_ref {
                iovs.push(IoSlice::new(hb));
            }
            for &(h_idx, f_idx) in pairs.iter() {
                iovs.push(IoSlice::new(&hdr_scratch[h_idx]));
                iovs.push(IoSlice::new(frames[f_idx].data));
            }
            wal_fd.write_vectored_at(&iovs, start_off)?;
            *pending_hdr = None;
            pairs.clear();
            Ok(())
        };

        for (idx, frame) in frames.iter().enumerate() {
            if frame.data.len() != page_size as usize {
                return Err(SqliteError::misuse("WAL frame data length != page_size"));
            }
            let is_last = idx + 1 == frames.len();

            // Check if this page was ALREADY spilled earlier in this uncommitted transaction
            // (`wal.c:4233-4249`). Note: on `is_commit`, the final frame is ALWAYS appended so
            // `nTruncate > 0` lands at the tail of the transaction!
            if i_first > 0
                && (!is_last || !is_commit)
                && let Some(i_write) = self.find_frame(frame.pgno)?
                && i_write >= i_first
            {
                {
                    let wal_fd = self.wal_file.as_ref().expect("checked Some");
                    flush_append_run(
                        wal_fd.as_ref(),
                        &mut pending_file_hdr,
                        &self.write_hdr_scratch,
                        &mut self.pending_append_pairs,
                        run_start_offset,
                    )?;
                }
                run_start_offset = i_offset;
                let data_off = wal_frame_offset(i_write, page_size) + (WAL_FRAME_HDRSIZE as u64);
                if self.i_re_cksum == 0 || i_write < self.i_re_cksum {
                    self.i_re_cksum = i_write;
                }
                self.wal_file
                    .as_ref()
                    .expect("checked Some")
                    .write_at(frame.data, data_off)?;
                continue;
            }

            i_frame += 1;
            let n_db_size = if is_commit && is_last { n_truncate } else { 0 };
            let (fhdr, next_cksum) = wal_encode_frame_header(
                frame.pgno,
                n_db_size,
                self.hdr.salt,
                self.hdr.big_end_cksum != 0,
                self.hdr.frame_cksum,
                frame.data,
            )?;
            let hdr_idx = self.write_hdr_scratch.len();
            self.write_hdr_scratch.push(fhdr);
            self.pending_append_pairs.push((hdr_idx, idx));
            self.hdr.frame_cksum = next_cksum;
            i_offset += sz_frame;
            self.appended_pgnos_scratch.push(frame.pgno);
            last_idx = Some(idx);
        }

        {
            let wal_fd = self.wal_file.as_ref().expect("checked Some");
            flush_append_run(
                wal_fd.as_ref(),
                &mut pending_file_hdr,
                &self.write_hdr_scratch,
                &mut self.pending_append_pairs,
                run_start_offset,
            )?;
        }

        // Recompute checksum chain if any earlier uncommitted frame was overwritten (`wal.c:4262-4265`).
        if is_commit && self.i_re_cksum > 0 {
            self.wal_rewrite_checksums(i_frame)?;
        }

        // Pad to sector boundary (unless `IOCAP_POWERSAFE_OVERWRITE`) and sync WAL BEFORE publishing
        // `WalIndexHdr` (`wal.c:4281-4300`).
        let mut n_extra = 0u32;
        if is_commit && let Some(w_sync) = wal_sync_flags {
            let wal_fd = self.wal_file.as_ref().expect("checked Some");
            let mut b_sync = true;
            if self.pad_to_sector_boundary {
                let sector_size = u64::from(wal_fd.sector_size().max(512));
                let i_sync_point = i_offset.div_ceil(sector_size) * sector_size;
                b_sync = i_sync_point == i_offset;
                if let Some(li) = last_idx {
                    let last_frame = &frames[li];
                    while i_offset < i_sync_point {
                        let (fhdr, next_cksum) = wal_encode_frame_header(
                            last_frame.pgno,
                            n_truncate,
                            self.hdr.salt,
                            self.hdr.big_end_cksum != 0,
                            self.hdr.frame_cksum,
                            last_frame.data,
                        )?;
                        if i_offset + sz_frame < i_sync_point {
                            let pad_iovs = [IoSlice::new(&fhdr), IoSlice::new(last_frame.data)];
                            wal_fd.write_vectored_at(&pad_iovs, i_offset)?;
                        } else {
                            Self::write_with_sync_point(
                                wal_fd.as_ref(),
                                &fhdr,
                                i_offset,
                                i_sync_point,
                                w_sync,
                            )?;
                            Self::write_with_sync_point(
                                wal_fd.as_ref(),
                                last_frame.data,
                                i_offset + (WAL_FRAME_HDRSIZE as u64),
                                i_sync_point,
                                w_sync,
                            )?;
                        }
                        self.hdr.frame_cksum = next_cksum;
                        i_offset += sz_frame;
                        n_extra += 1;
                    }
                }
            }
            if b_sync {
                wal_fd.sync(w_sync)?;
            }
        }

        // Apply `journal_size_limit` on the first commit after a WAL reset (`wal.c:4306-4313`).
        if is_commit && self.truncate_on_commit && self.mx_wal_size >= 0 {
            let min_bytes = wal_frame_offset(i_frame + n_extra + 1, page_size);
            let target_sz = (self.mx_wal_size as u64).max(min_bytes);
            let wal_fd = self.wal_file.as_ref().expect("checked Some");
            if wal_fd.file_size()? > target_sz {
                wal_fd.truncate(target_sz)?;
            }
            self.truncate_on_commit = false;
        }

        // Append newly written frames (plus any sector-padding frames) into the WAL-index (`wal.c:4320-4331`).
        if let Some(li) = last_idx
            && n_extra > 0
        {
            let last_pgno = frames[li].pgno;
            for _ in 0..n_extra {
                self.appended_pgnos_scratch.push(last_pgno);
            }
        }
        let idx_frame = self
            .hdr
            .mx_frame
            .checked_add(self.appended_pgnos_scratch.len() as u32)
            .ok_or_else(|| SqliteError::misuse("WAL frame index overflow"))?;
        if !self.appended_pgnos_scratch.is_empty() {
            let first_new_frame = self.hdr.mx_frame + 1;
            self.record_index_frames_batch(first_new_frame)?;
        }

        // Update local header and (if `is_commit`) publish `WalIndexHdr` (`wal.c:4334-4347`).
        self.hdr.page_size = page_size;
        self.hdr.mx_frame = idx_frame;
        if is_commit {
            self.hdr.change_counter = self.hdr.change_counter.wrapping_add(1);
            self.hdr.n_page = n_truncate;
            let next_hdr = self.hdr;
            let published = self.publish_committed_header(&next_hdr)?;
            self.hdr = published;
            self.i_callback = idx_frame;
        }
        self.hdr_valid = true;
        Ok(())
    }

    fn write_with_sync_point(
        wal_fd: &dyn VfsFile,
        content: &[u8],
        offset: u64,
        sync_point: u64,
        sync_flags: SyncFlags,
    ) -> Result<()> {
        let end = offset + (content.len() as u64);
        if offset < sync_point && end >= sync_point {
            let first_amt = (sync_point - offset) as usize;
            wal_fd.write_at(&content[..first_amt], offset)?;
            wal_fd.sync(sync_flags)?;
            if first_amt < content.len() {
                wal_fd.write_at(&content[first_amt..], sync_point)?;
            }
            return Ok(());
        }
        wal_fd.write_at(content, offset)
    }

    /// Recomputes frame checksums from `self.i_re_cksum` through `i_last` (`walRewriteChecksums`,
    /// `wal.c:4075-4118`).
    fn wal_rewrite_checksums(&mut self, i_last: u32) -> Result<()> {
        let wal_fd = self
            .wal_file
            .as_ref()
            .ok_or_else(|| SqliteError::corrupt("missing -wal file during checksum rewrite"))?;
        let sz_page = self.sz_page;
        let cksum_off = if self.i_re_cksum == 1 {
            24
        } else {
            wal_frame_offset(self.i_re_cksum - 1, sz_page) + 16
        };
        let mut raw_ck = [0u8; 8];
        let n_ck = wal_fd.read_at(&mut raw_ck, cksum_off)?;
        if n_ck < 8 {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHORT_READ,
                "short read loading baseline checksum in walRewriteChecksums",
            ));
        }
        self.hdr.frame_cksum = [
            u32::from_be_bytes([raw_ck[0], raw_ck[1], raw_ck[2], raw_ck[3]]),
            u32::from_be_bytes([raw_ck[4], raw_ck[5], raw_ck[6], raw_ck[7]]),
        ];

        let i_start = self.i_re_cksum;
        self.i_re_cksum = 0;
        let mut frame_hdr = [0u8; WAL_FRAME_HDRSIZE];
        let mut page_buf = vec![0u8; sz_page as usize];

        for i_read in i_start..=i_last {
            let off = wal_frame_offset(i_read, sz_page);
            let n1 = wal_fd.read_at(&mut frame_hdr, off)?;
            let n2 = wal_fd.read_at(&mut page_buf, off + (WAL_FRAME_HDRSIZE as u64))?;
            if n1 < WAL_FRAME_HDRSIZE || n2 < page_buf.len() {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHORT_READ,
                    "short read rewriting WAL frame checksum",
                ));
            }
            let pgno = u32::from_be_bytes([frame_hdr[0], frame_hdr[1], frame_hdr[2], frame_hdr[3]]);
            let n_db_size =
                u32::from_be_bytes([frame_hdr[4], frame_hdr[5], frame_hdr[6], frame_hdr[7]]);
            let (new_hdr, next_cksum) = wal_encode_frame_header(
                pgno,
                n_db_size,
                self.hdr.salt,
                self.hdr.big_end_cksum != 0,
                self.hdr.frame_cksum,
                &page_buf,
            )?;
            wal_fd.write_at(&new_hdr, off)?;
            self.hdr.frame_cksum = next_cksum;
        }
        Ok(())
    }

    fn read_live_committed_header(&self) -> Result<Option<WalIndexHeaderData>> {
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            return Ok(self.heap_index.hdr);
        }
        match &self.active_lock {
            ActiveShmLock::Writer(wg) => match wg.read_header_verified() {
                Ok(h) => Ok(Some(h)),
                Err(_) => Ok(None),
            },
            ActiveShmLock::Reader(rg) => match rg.read_header_verified() {
                Ok(h) => Ok(Some(h)),
                Err(_) => Ok(None),
            },
            ActiveShmLock::None => Ok(None),
        }
    }

    fn record_index_frames_batch(&mut self, first_frame_no: u32) -> Result<()> {
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            for (i, &pgno) in self.appended_pgnos_scratch.iter().enumerate() {
                self.heap_index
                    .record_frame(first_frame_no + (i as u32), pgno)?;
            }
            return Ok(());
        }
        match &mut self.active_lock {
            ActiveShmLock::Writer(wg) => {
                wg.record_frames_batch(first_frame_no, &self.appended_pgnos_scratch)
            }
            _ => Err(SqliteError::misuse(
                "record_index_frames_batch requires OwnedWriterTxnGuard",
            )),
        }
    }

    fn publish_committed_header(
        &mut self,
        header: &WalIndexHeaderData,
    ) -> Result<WalIndexHeaderData> {
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            let (_, normalized) = header.encode_for_publish()?;
            self.heap_index.hdr = Some(normalized);
            return Ok(normalized);
        }
        match &mut self.active_lock {
            ActiveShmLock::Writer(wg) => wg.publish_header(header),
            _ => Err(SqliteError::misuse(
                "publish_committed_header requires OwnedWriterTxnGuard",
            )),
        }
    }

    /// Records the 4-word WAL savepoint token `[mx_frame, frame_cksum[0], frame_cksum[1], n_ckpt]`
    /// (`sqlite3WalSavepoint`, `wal.c:3904-3911`).
    #[inline]
    pub const fn savepoint(&self) -> [u32; WAL_SAVEPOINT_NDATA] {
        [
            self.hdr.mx_frame,
            self.hdr.frame_cksum[0],
            self.hdr.frame_cksum[1],
            self.n_ckpt,
        ]
    }

    /// Rolls back uncommitted WAL frames written after `savepoint_data` (`sqlite3WalSavepointUndo`,
    /// `wal.c:3919-3947`), normalizing `savepoint_data[0] = 0` if `n_ckpt` wrapped via `walRestartLog`!
    pub fn savepoint_undo(
        &mut self,
        savepoint_data: &mut [u32; WAL_SAVEPOINT_NDATA],
    ) -> Result<()> {
        if !self.write_lock {
            return Err(SqliteError::misuse(
                "savepoint_undo requires an active WAL write transaction",
            ));
        }
        if savepoint_data[3] != self.n_ckpt {
            savepoint_data[0] = 0;
            savepoint_data[3] = self.n_ckpt;
        }
        if savepoint_data[0] < self.hdr.mx_frame {
            self.hdr.mx_frame = savepoint_data[0];
            self.hdr.frame_cksum = [savepoint_data[1], savepoint_data[2]];
            self.cleanup_hash_above(self.hdr.mx_frame)?;
            if self.i_re_cksum > self.hdr.mx_frame {
                self.i_re_cksum = 0;
            }
        }
        Ok(())
    }

    /// Rolls back all uncommitted frames written in the current write transaction (`sqlite3WalUndo`,
    /// `wal.c:3860-3896`), invoking `on_page(pgno)` for each reverted frame in `live_mx + 1 ..= old_mx`.
    pub fn undo<F>(&mut self, mut on_page: F) -> Result<()>
    where
        F: FnMut(Pgno) -> Result<()>,
    {
        if !self.write_lock {
            return Ok(());
        }
        let i_max = self.hdr.mx_frame;
        let restored_hdr = match self.read_live_committed_header()? {
            Some(h) => h,
            None => WalIndexHeaderData::new(
                self.hdr.page_size.max(512),
                0,
                0,
                0,
                self.hdr.big_end_cksum != 0,
                [0, 0],
                [0, 0],
            )?,
        };
        self.hdr = restored_hdr;

        for frame_no in (self.hdr.mx_frame + 1)..=i_max {
            let pgno = self.read_uncommitted_frame_pgno(frame_no)?;
            if pgno > 0 {
                on_page(pgno)?;
            }
        }
        if i_max > self.hdr.mx_frame {
            self.cleanup_hash_above(self.hdr.mx_frame)?;
        }
        self.i_re_cksum = 0;
        Ok(())
    }

    fn read_uncommitted_frame_pgno(&self, frame_no: u32) -> Result<Pgno> {
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            return self.heap_index.read_frame_pgno(frame_no);
        }
        let wal_fd = self
            .wal_file
            .as_ref()
            .ok_or_else(|| SqliteError::corrupt("missing -wal file during WAL undo"))?;
        let off = wal_frame_offset(frame_no, self.sz_page.max(512));
        let mut b = [0u8; 4];
        let n = wal_fd.read_at(&mut b, off)?;
        if n < 4 {
            return Ok(0);
        }
        Ok(u32::from_be_bytes(b))
    }

    fn cleanup_hash_above(&mut self, max_frame: u32) -> Result<()> {
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            self.heap_index.truncate_frames_above(max_frame);
            return Ok(());
        }
        match &mut self.active_lock {
            ActiveShmLock::Writer(wg) => wg.truncate_frames_above(max_frame),
            _ => Err(SqliteError::misuse(
                "cleanup_hash_above requires OwnedWriterTxnGuard",
            )),
        }
    }

    // ========================================================================
    // C. WAL Recovery (`walIndexRecover`, `wal.c:1422-1646`)
    // ========================================================================

    /// Scans `-wal` from byte 0, verifying header & cumulative frame checksums up to the last
    /// commit frame (`nTruncate != 0`), returning `(recovered_hdr, n_ckpt, sz_page, committed_frames)`.
    fn scan_wal_file_for_recovery(&self) -> Result<(WalIndexHeaderData, u32, u32, Vec<Pgno>)> {
        let default_be = cfg!(target_endian = "big");
        let fallback_ps = if self.sz_page >= 512 {
            self.sz_page
        } else {
            self.hdr.page_size.max(4096)
        };
        let empty_hdr = WalIndexHeaderData::new(fallback_ps, 0, 0, 0, default_be, [0, 0], [0, 0])?;

        let Some(ref wal_fd) = self.wal_file else {
            return Ok((empty_hdr, 0, 0, Vec::new()));
        };

        let n_size = wal_fd.file_size()?;
        if n_size <= WAL_HDRSIZE as u64 {
            return Ok((empty_hdr, 0, 0, Vec::new()));
        }

        let mut hdr_buf = [0u8; WAL_HDRSIZE];
        let n_hdr = wal_fd.read_at(&mut hdr_buf, 0)?;
        if n_hdr < WAL_HDRSIZE {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHORT_READ,
                "short read on 32-byte WAL header during recovery",
            ));
        }

        let Some(file_hdr) = WalFileHeader::decode_for_recovery(&hdr_buf)? else {
            return Ok((empty_hdr, 0, 0, Vec::new()));
        };

        let sz_page = file_hdr.page_size;
        let sz_frame = (WAL_FRAME_HDRSIZE as u64) + u64::from(sz_page);
        let i_last_frame = ((n_size - (WAL_HDRSIZE as u64)) / sz_frame) as u32;

        let mut running_cksum = file_hdr.cksum;
        let mut last_commit_frame = 0u32;
        let mut last_commit_npage = 0u32;
        let mut last_commit_cksum = file_hdr.cksum;
        let mut candidate_pgnos: Vec<Pgno> = Vec::with_capacity(i_last_frame as usize);

        let mut frame_hdr = [0u8; WAL_FRAME_HDRSIZE];
        let mut page_buf = vec![0u8; sz_page as usize];

        for i_frame in 1..=i_last_frame {
            let offset = wal_frame_offset(i_frame, sz_page);
            let n1 = wal_fd.read_at(&mut frame_hdr, offset)?;
            let n2 = wal_fd.read_at(&mut page_buf, offset + (WAL_FRAME_HDRSIZE as u64))?;
            if n1 < WAL_FRAME_HDRSIZE || n2 < page_buf.len() {
                return Err(SqliteError::with_extended(
                    extended::SQLITE_IOERR_SHORT_READ,
                    format!("short read scanning WAL frame {i_frame} during recovery"),
                ));
            }

            let Some((pgno, n_truncate, next_cksum)) = wal_decode_frame(
                &frame_hdr,
                &page_buf,
                file_hdr.salt,
                file_hdr.big_end_cksum,
                running_cksum,
            )?
            else {
                break;
            };

            running_cksum = next_cksum;
            candidate_pgnos.push(pgno);
            if n_truncate != 0 {
                last_commit_frame = i_frame;
                last_commit_npage = n_truncate;
                last_commit_cksum = next_cksum;
            }
        }

        candidate_pgnos.truncate(last_commit_frame as usize);

        let mut rec_hdr = WalIndexHeaderData::new(
            sz_page,
            last_commit_frame,
            last_commit_npage,
            0,
            file_hdr.big_end_cksum,
            last_commit_cksum,
            file_hdr.salt,
        )?;
        rec_hdr.version = WALINDEX_MAX_VERSION;
        let (_, normalized) = rec_hdr.encode_for_publish()?;
        Ok((normalized, file_hdr.n_ckpt, sz_page, candidate_pgnos))
    }

    fn recover_into_heap_index(&mut self) -> Result<()> {
        let (rec_hdr, n_ckpt, sz_page, committed_pgnos) = self.scan_wal_file_for_recovery()?;
        let mut heap = HeapWalIndex::default();
        for (i, &pgno) in committed_pgnos.iter().enumerate() {
            heap.record_frame((i as u32) + 1, pgno)?;
        }
        heap.ckpt.n_backfill = 0;
        heap.ckpt.n_backfill_attempted = rec_hdr.mx_frame;
        heap.ckpt.read_marks[0] = 0;
        heap.ckpt.read_marks[1] = if rec_hdr.mx_frame > 0 {
            rec_hdr.mx_frame
        } else {
            READMARK_NOT_USED
        };
        for slot in 2..WAL_NREADER {
            heap.ckpt.read_marks[slot] = READMARK_NOT_USED;
        }
        heap.hdr = Some(rec_hdr);
        self.heap_index = heap;
        self.n_ckpt = n_ckpt;
        if sz_page >= 512 {
            self.sz_page = sz_page;
        }
        Ok(())
    }

    fn recover_into_shm_from_init_write(
        &mut self,
        init_write: &mut sqliters_vfs::OwnedInitWriteGuard,
    ) -> Result<WalIndexHeaderData> {
        let mut rec_guard = init_write.begin_recovery()?;
        let scan_res = self.scan_wal_file_for_recovery();
        let (rec_hdr, n_ckpt, sz_page, committed_pgnos) = match scan_res {
            Ok(v) => v,
            Err(e) => {
                let _ = rec_guard.finish();
                return Err(e);
            }
        };

        let max_region = if committed_pgnos.is_empty() {
            0
        } else {
            wal_frame_page(committed_pgnos.len() as u32)
        };
        for r in 0..=max_region {
            rec_guard.reset_region_for_recovery(r)?;
        }
        for (i, &pgno) in committed_pgnos.iter().enumerate() {
            let fno = (i as u32) + 1;
            rec_guard.record_frame(fno, pgno, fno - 1)?;
        }
        rec_guard.truncate_frames_above(rec_hdr.mx_frame)?;
        let (published, _) = rec_guard.finish_recovery_header_and_marks(&rec_hdr)?;
        rec_guard.finish()?;

        self.n_ckpt = n_ckpt;
        if sz_page >= 512 {
            self.sz_page = sz_page;
        }
        Ok(published)
    }

    fn recover_into_shm_from_checkpoint(
        &mut self,
        ckpt_guard: &mut sqliters_vfs::CheckpointTxnGuard<'_>,
    ) -> Result<WalIndexHeaderData> {
        let mut rec_guard = ckpt_guard.begin_recovery_under_checkpoint()?;
        let scan_res = self.scan_wal_file_for_recovery();
        let (rec_hdr, n_ckpt, sz_page, committed_pgnos) = match scan_res {
            Ok(v) => v,
            Err(e) => {
                let _ = rec_guard.finish();
                return Err(e);
            }
        };

        let max_region = if committed_pgnos.is_empty() {
            0
        } else {
            wal_frame_page(committed_pgnos.len() as u32)
        };
        for r in 0..=max_region {
            rec_guard.reset_region_for_recovery(r)?;
        }
        for (i, &pgno) in committed_pgnos.iter().enumerate() {
            let fno = (i as u32) + 1;
            rec_guard.record_frame(fno, pgno, fno - 1)?;
        }
        rec_guard.truncate_frames_above(rec_hdr.mx_frame)?;
        let (published, _) = rec_guard.finish_recovery_header_and_marks(&rec_hdr)?;
        rec_guard.finish()?;

        self.n_ckpt = n_ckpt;
        if sz_page >= 512 {
            self.sz_page = sz_page;
        }
        Ok(published)
    }

    // ========================================================================
    // E. Checkpointing (`sqlite3WalCheckpoint` / `walCheckpoint`, `wal.c:2281-2477, 4387-4518`)
    // ========================================================================

    /// Executes a WAL checkpoint (`Noop`, `Passive`, `Full`, `Restart`, or `Truncate`).
    pub fn checkpoint(
        &mut self,
        db_file: &dyn VfsFile,
        mode: WalCheckpointMode,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
        sync_flags: Option<SyncFlags>,
        interrupt_flag: Option<&AtomicBool>,
    ) -> Result<(WalCheckpointResult, bool)> {
        self.checkpoint_with_hook(
            db_file,
            mode,
            busy_handler,
            sync_flags,
            interrupt_flag,
            None,
        )
    }

    /// Executes a WAL checkpoint with an optional deterministic hook fired right after
    /// `walIndexReadHdr` and before `mxSafeFrame` / `WAL_READ_LOCK(0)` acquisition (used to test
    /// the March 2026 WAL-reset salt race deterministically without timing sleeps).
    pub fn checkpoint_with_hook(
        &mut self,
        db_file: &dyn VfsFile,
        mode: WalCheckpointMode,
        mut busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
        sync_flags: Option<SyncFlags>,
        interrupt_flag: Option<&AtomicBool>,
        after_read_hdr_hook: Option<&mut dyn FnMut()>,
    ) -> Result<(WalCheckpointResult, bool)> {
        if self.read_lock.is_some() && self.exclusive_mode != WalExclusiveMode::Exclusive {
            return Err(SqliteError::new(
                ErrorCode::Locked,
                "database table is locked",
            ));
        }
        if self.write_lock {
            return Err(SqliteError::new(
                ErrorCode::Locked,
                "database table is locked",
            ));
        }
        if self.read_only_flags != WAL_RDWR || self.immutable {
            return Err(SqliteError::readonly(
                "attempt to write a readonly database",
            ));
        }

        // Heap-memory WAL (`locking_mode=EXCLUSIVE` at open).
        if self.exclusive_mode == WalExclusiveMode::HeapMemory {
            return self.checkpoint_heap_memory(db_file, mode, sync_flags, interrupt_flag);
        }

        // `NOOP (-1)` (`wal.c:4417, 4467-4515`): does NOT acquire `WAL_CKPT_LOCK` or `WAL_WRITE_LOCK`
        // (except transiently inside `walIndexReadHdr` if recovery is required!), does NOT backfill,
        // DOES call `walIndexReadHdr(&mut is_changed)` and reports `(mx_frame, n_backfill)`!
        if mode == WalCheckpointMode::Noop {
            let mut is_changed = false;
            loop {
                match self.wal_index_read_hdr(db_file, &mut is_changed)? {
                    (TryBeginOutcome::Ok, _) => break,
                    (TryBeginOutcome::Retry, _) => continue,
                }
            }
            let shm = self.ensure_shm_attached(db_file)?;
            let n_backfill = shm
                .try_read_header_and_ckpt_info()?
                .map(|(_, info)| info.n_backfill)
                .unwrap_or(0);
            let n_log = i32::try_from(self.hdr.mx_frame).unwrap_or(i32::MAX);
            let n_ckpt = i32::try_from(n_backfill).unwrap_or(i32::MAX);
            if is_changed {
                self.hdr_valid = false;
            }
            return Ok((
                WalCheckpointResult {
                    busy: false,
                    n_log,
                    n_ckpt,
                },
                is_changed,
            ));
        }

        let shm = self.ensure_shm_attached(db_file)?;
        let shm_mode = mode.to_shm_mode().unwrap();

        // Acquire `WAL_CKPT_LOCK` WITHOUT busy_handler (`R-53820-33897`), and (if `mode > Passive`)
        // try `WAL_WRITE_LOCK` WITH busy_handler, downgrading to `Passive` if `WRITE` stays `Busy`!
        let (mut ckpt_guard, downgraded_busy) = match shm.acquire_checkpoint_with_busy(
            shm_mode,
            if mode == WalCheckpointMode::Passive {
                None
            } else {
                match busy_handler.as_mut() {
                    Some(h) => Some(&mut **h),
                    None => None,
                }
            },
        ) {
            Ok(pair) => pair,
            Err(e) if e.code == ErrorCode::Busy => {
                // `WAL_CKPT_LOCK` contended by another checkpointer: return `Busy` immediately!
                return Ok((
                    WalCheckpointResult {
                        busy: true,
                        n_log: -1,
                        n_ckpt: -1,
                    },
                    false,
                ));
            }
            Err(other) => return Err(other),
        };

        let effective_mode = if downgraded_busy {
            WalCheckpointMode::Passive
        } else {
            mode
        };
        if effective_mode == WalCheckpointMode::Passive {
            busy_handler = None;
        }

        let mut is_changed = false;
        let run_res = self.run_checkpoint_under_guard(
            db_file,
            &mut ckpt_guard,
            effective_mode,
            downgraded_busy,
            busy_handler,
            sync_flags,
            interrupt_flag,
            &mut is_changed,
            after_read_hdr_hook,
        );
        let release_res = ckpt_guard.release();

        if is_changed {
            self.hdr_valid = false;
        }

        let out = run_res?;
        release_res?;
        Ok((out, is_changed))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_checkpoint_under_guard(
        &mut self,
        db_file: &dyn VfsFile,
        ckpt_guard: &mut sqliters_vfs::CheckpointTxnGuard<'_>,
        effective_mode: WalCheckpointMode,
        downgraded_busy: bool,
        mut x_busy2: Option<&mut dyn FnMut(u32) -> bool>,
        sync_flags: Option<SyncFlags>,
        interrupt_flag: Option<&AtomicBool>,
        is_changed: &mut bool,
        after_read_hdr_hook: Option<&mut dyn FnMut()>,
    ) -> Result<WalCheckpointResult> {
        // Step 1: Read/recover `WalIndexHdr` (`walIndexReadHdr`, `wal.c:4459`).
        match ckpt_guard.try_read_header_and_ckpt_info()? {
            Some((live_hdr, _)) => {
                if !self.hdr_valid || live_hdr != self.hdr {
                    self.apply_loaded_header(live_hdr);
                    *is_changed = true;
                }
            }
            None => {
                let acquired_temp_write = !ckpt_guard.holds_write_lock();
                if acquired_temp_write {
                    match ckpt_guard.acquire_temp_write_lock() {
                        Ok(()) => {}
                        Err(e) if e.code == ErrorCode::Busy => {
                            return Ok(WalCheckpointResult {
                                busy: true,
                                n_log: -1,
                                n_ckpt: -1,
                            });
                        }
                        Err(other) => return Err(other),
                    }
                }

                let inner_res: Result<()> = match ckpt_guard.extend_and_try_read_header()? {
                    Some((live_hdr, _)) => {
                        if !self.hdr_valid || live_hdr != self.hdr {
                            self.apply_loaded_header(live_hdr);
                            *is_changed = true;
                        }
                        Ok(())
                    }
                    None => {
                        let rec = self.recover_into_shm_from_checkpoint(ckpt_guard)?;
                        self.apply_loaded_header(rec);
                        *is_changed = true;
                        Ok(())
                    }
                };

                if acquired_temp_write {
                    let rel = ckpt_guard.release_temp_write_lock();
                    inner_res?;
                    rel?;
                } else {
                    inner_res?;
                }
            }
        }

        if let Some(hook) = after_read_hdr_hook {
            hook();
        }

        // Step 2: Run `walCheckpoint` (`wal.c:2281-2477`).
        let mut hit_busy = downgraded_busy;

        if self.hdr.mx_frame > 0 {
            let mx_safe_frame = ckpt_guard.claim_safe_backfill_frame_with_busy(
                self.hdr.mx_frame,
                match x_busy2.as_mut() {
                    Some(h) => Some(&mut **h),
                    None => None,
                },
            )?;
            if mx_safe_frame < self.hdr.mx_frame {
                // At least one reader slot was busy and clamped `mx_safe_frame`; disable `xBusy2` (`wal.c:2330`).
                x_busy2 = None;
            }

            let pre_info = ckpt_guard.read_ckpt_info()?;
            if pre_info.n_backfill < mx_safe_frame {
                let lock_r0 = ckpt_guard.lock_read0_for_backfill(match x_busy2.as_mut() {
                    Some(h) => Some(&mut **h),
                    None => None,
                });
                match lock_r0 {
                    Ok(()) => {
                        let backfill_res = self.backfill_frames_under_read0(
                            db_file,
                            ckpt_guard,
                            pre_info.n_backfill,
                            mx_safe_frame,
                            sync_flags,
                            interrupt_flag,
                        );
                        let unlock_r0 = ckpt_guard.unlock_read0_after_backfill();
                        backfill_res?;
                        unlock_r0?;
                    }
                    Err(e) if e.code == ErrorCode::Busy => {
                        // Per `wal.c:2428-2432`: `PASSIVE` clears `Busy` from `READ(0)` (reporting
                        // partial counts cleanly), while `Full`/`Restart`/`Truncate` report `Busy`!
                        if effective_mode != WalCheckpointMode::Passive {
                            hit_busy = true;
                        }
                    }
                    Err(other) => return Err(other),
                }
            }
        }

        let mut post_info = ckpt_guard.read_ckpt_info()?;

        if effective_mode != WalCheckpointMode::Passive {
            if post_info.n_backfill < self.hdr.mx_frame {
                hit_busy = true;
            } else if matches!(
                effective_mode,
                WalCheckpointMode::Restart | WalCheckpointMode::Truncate
            ) {
                let lock_readers =
                    ckpt_guard.lock_all_positive_readers_with_busy(match x_busy2.as_mut() {
                        Some(h) => Some(&mut **h),
                        None => None,
                    });
                match lock_readers {
                    Ok(()) => {
                        if effective_mode == WalCheckpointMode::Truncate {
                            let mut rand4 = [0u8; 4];
                            self.vfs.randomness(&mut rand4);
                            let salt1 = u32::from_be_bytes(rand4);

                            // STRICT ORDER (`wal.c:2464-2466`):
                            // 1) Reset WAL-index header & marks (`walRestartHdr`) FIRST.
                            // 2) Truncate `-wal` to 0 bytes SECOND.
                            // 3) Unlock `WAL_READ_LOCK(1..=4)` THIRD.
                            let res = (|| -> Result<()> {
                                let restarted =
                                    ckpt_guard.restart_header_for_checkpoint(&self.hdr, salt1)?;
                                self.n_ckpt = self.n_ckpt.wrapping_add(1);
                                self.hdr = restarted;
                                if let Some(ref wal_fd) = self.wal_file {
                                    wal_fd.truncate(0)?;
                                }
                                Ok(())
                            })();
                            let unlock_res = ckpt_guard.unlock_all_positive_readers();
                            res?;
                            unlock_res?;
                            post_info = ckpt_guard.read_ckpt_info()?;
                        } else {
                            // `RESTART` simply verifies all `READ(1..=4)` locks were obtainable so
                            // the next writer's `walRestartLog` can wrap from `READ(0)` (`wal.c:2456-2467`).
                            ckpt_guard.unlock_all_positive_readers()?;
                        }
                    }
                    Err(e) if e.code == ErrorCode::Busy => {
                        hit_busy = true;
                    }
                    Err(other) => return Err(other),
                }
            }
        }

        let n_log = i32::try_from(self.hdr.mx_frame).unwrap_or(i32::MAX);
        let n_ckpt = i32::try_from(post_info.n_backfill).unwrap_or(i32::MAX);
        Ok(WalCheckpointResult {
            busy: hit_busy,
            n_log,
            n_ckpt,
        })
    }

    fn backfill_frames_under_read0(
        &self,
        db_file: &dyn VfsFile,
        ckpt_guard: &mut sqliters_vfs::CheckpointTxnGuard<'_>,
        start_backfill: u32,
        mx_safe_frame: u32,
        sync_flags: Option<SyncFlags>,
        interrupt_flag: Option<&AtomicBool>,
    ) -> Result<()> {
        // March 2026 WAL-reset salt guard (`wal.c:2354-2355`):
        // verify live `WalIndexHdr.salt` in `-shm` still matches `self.hdr.salt` AFTER acquiring
        // `EXCLUSIVE WAL_READ_LOCK(0)`! If a writer wrapped the WAL in the meantime, abort without
        // touching `nBackfillAttempted`, `nBackfill`, or `.db`!
        let live_salt = ckpt_guard.read_live_header_salt()?;
        if live_salt != self.hdr.salt {
            return Ok(());
        }

        let n_page = self.hdr.n_page;
        let sz_page = if self.sz_page >= 512 {
            self.sz_page
        } else {
            self.hdr.page_size
        };
        let wal_fd = self
            .wal_file
            .as_ref()
            .ok_or_else(|| SqliteError::corrupt("missing -wal file during checkpoint backfill"))?;

        ckpt_guard.set_backfill_attempted(mx_safe_frame)?;
        if let Some(sflags) = sync_flags {
            wal_fd.sync(sflags)?;
        }

        // Database growth sanity check (`wal.c:2365-2376`): if the final database size `n_req`
        // exceeds the current `.db` file size plus all WAL frames plus the maximum pending-byte
        // page gap (`65_536` bytes), the WAL header is corrupt.
        let cur_db_size = db_file.file_size()?;
        let n_req = u64::from(n_page) * u64::from(sz_page);
        if cur_db_size < n_req
            && cur_db_size
                .saturating_add(65_536)
                .saturating_add(u64::from(self.hdr.mx_frame) * u64::from(sz_page))
                < n_req
        {
            return Err(SqliteError::corrupt(
                "WAL header database size exceeds current database plus WAL frames",
            ));
        }

        // Build deduplicated `(pgno, latest_frame)` list over `(start_backfill, mx_safe_frame]`
        // sorted in ascending `pgno` order (`walIteratorInit` / `walIteratorNext`, `wal.c:1833-2099`)!
        let ordered_pages: Vec<(Pgno, u32)> = if n_page <= 16_384 {
            let mut dense = vec![0u32; (n_page as usize) + 1];
            ckpt_guard.for_each_frame_pgno(
                start_backfill + 1,
                mx_safe_frame,
                &mut |fno, pgno| {
                    if pgno > 0 && pgno <= n_page {
                        dense[pgno as usize] = fno;
                    }
                },
            )?;
            let mut out = Vec::with_capacity((mx_safe_frame - start_backfill).min(n_page) as usize);
            for (idx, &fno) in dense.iter().enumerate().skip(1) {
                if fno != 0 {
                    out.push((idx as Pgno, fno));
                }
            }
            out
        } else {
            let mut latest_by_pgno: HashMap<Pgno, u32> = HashMap::new();
            ckpt_guard.for_each_frame_pgno(
                start_backfill + 1,
                mx_safe_frame,
                &mut |fno, pgno| {
                    if pgno > 0 && pgno <= n_page {
                        latest_by_pgno.insert(pgno, fno);
                    }
                },
            )?;
            let mut out: Vec<(Pgno, u32)> = latest_by_pgno.into_iter().collect();
            out.sort_unstable_by_key(|&(pgno, _)| pgno);
            out
        };

        self.backfill_ordered_pages_coalesced(db_file, &ordered_pages, sz_page, interrupt_flag)?;

        // Full vs Partial backfill (`wal.c:2409-2421`):
        // ONLY when `mx_safe_frame == live_hdr.mx_frame` (full backfill) do we truncate & sync `.db`!
        let live_mx_frame = ckpt_guard
            .try_read_header_and_ckpt_info()?
            .map(|(h, _)| h.mx_frame)
            .unwrap_or(self.hdr.mx_frame);

        if mx_safe_frame == live_mx_frame {
            let target_db_bytes = u64::from(n_page) * u64::from(sz_page);
            db_file.truncate(target_db_bytes)?;
            if let Some(sflags) = sync_flags {
                db_file.sync(sflags)?;
            }
        }

        ckpt_guard.set_backfill_completed(mx_safe_frame)?;
        Ok(())
    }

    fn backfill_ordered_pages_coalesced(
        &self,
        db_file: &dyn VfsFile,
        ordered_pages: &[(Pgno, u32)],
        sz_page: u32,
        interrupt_flag: Option<&AtomicBool>,
    ) -> Result<()> {
        if ordered_pages.is_empty() {
            return Ok(());
        }
        let ps = sz_page as usize;
        let max_run_pages = (65_536 / ps).max(1).min(ordered_pages.len());
        let mut run_buf = vec![0u8; ps * max_run_pages];
        let mut idx = 0usize;
        while idx < ordered_pages.len() {
            let (first_pgno, first_fno) = ordered_pages[idx];
            if let Some(flag) = interrupt_flag
                && flag.load(Ordering::SeqCst)
            {
                return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
            }
            self.read_frame(first_fno, &mut run_buf[..ps])?;
            let mut run_pages = 1usize;
            while run_pages < max_run_pages
                && idx + run_pages < ordered_pages.len()
                && ordered_pages[idx + run_pages].0 == first_pgno + (run_pages as u32)
            {
                if let Some(flag) = interrupt_flag
                    && flag.load(Ordering::SeqCst)
                {
                    return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
                }
                let fno = ordered_pages[idx + run_pages].1;
                let dst_start = run_pages * ps;
                self.read_frame(fno, &mut run_buf[dst_start..dst_start + ps])?;
                run_pages += 1;
            }
            let db_off = u64::from(first_pgno - 1) * u64::from(sz_page);
            db_file.write_at(&run_buf[..run_pages * ps], db_off)?;
            idx += run_pages;
        }
        Ok(())
    }

    fn checkpoint_heap_memory(
        &mut self,
        db_file: &dyn VfsFile,
        mode: WalCheckpointMode,
        sync_flags: Option<SyncFlags>,
        interrupt_flag: Option<&AtomicBool>,
    ) -> Result<(WalCheckpointResult, bool)> {
        let mut is_changed = false;
        if self.heap_index.hdr.is_none() {
            self.recover_into_heap_index()?;
            if let Some(h) = self.heap_index.hdr {
                self.apply_loaded_header(h);
            }
            is_changed = true;
        }
        if mode == WalCheckpointMode::Noop {
            return Ok((
                WalCheckpointResult {
                    busy: false,
                    n_log: i32::try_from(self.hdr.mx_frame).unwrap_or(i32::MAX),
                    n_ckpt: i32::try_from(self.heap_index.ckpt.n_backfill).unwrap_or(i32::MAX),
                },
                is_changed,
            ));
        }

        let mx_frame = self.hdr.mx_frame;
        let start_backfill = self.heap_index.ckpt.n_backfill;
        if start_backfill < mx_frame {
            let n_page = self.hdr.n_page;
            let sz_page = if self.sz_page >= 512 {
                self.sz_page
            } else {
                self.hdr.page_size
            };
            self.heap_index.ckpt.n_backfill_attempted = mx_frame;
            if let (Some(sflags), Some(wal_fd)) = (sync_flags, self.wal_file.as_ref()) {
                wal_fd.sync(sflags)?;
            }

            let cur_db_size = db_file.file_size()?;
            let n_req = u64::from(n_page) * u64::from(sz_page);
            if cur_db_size < n_req
                && cur_db_size
                    .saturating_add(65_536)
                    .saturating_add(u64::from(mx_frame) * u64::from(sz_page))
                    < n_req
            {
                return Err(SqliteError::corrupt(
                    "WAL header database size exceeds current database plus WAL frames",
                ));
            }

            let mut latest_by_pgno: HashMap<Pgno, u32> = HashMap::new();
            for fno in (start_backfill + 1)..=mx_frame {
                let pgno = self.heap_index.read_frame_pgno(fno)?;
                if pgno > 0 && pgno <= n_page {
                    latest_by_pgno.insert(pgno, fno);
                }
            }
            let mut ordered: Vec<(Pgno, u32)> = latest_by_pgno.into_iter().collect();
            ordered.sort_unstable_by_key(|&(pgno, _)| pgno);

            self.backfill_ordered_pages_coalesced(db_file, &ordered, sz_page, interrupt_flag)?;

            db_file.truncate(u64::from(n_page) * u64::from(sz_page))?;
            if let Some(sflags) = sync_flags {
                db_file.sync(sflags)?;
            }
            self.heap_index.ckpt.n_backfill = mx_frame;
        }

        if mode == WalCheckpointMode::Truncate {
            let mut rand4 = [0u8; 4];
            self.vfs.randomness(&mut rand4);
            let salt1 = u32::from_be_bytes(rand4);
            self.n_ckpt = self.n_ckpt.wrapping_add(1);
            self.hdr.mx_frame = 0;
            self.hdr.salt[0] = self.hdr.salt[0].wrapping_add(1);
            self.hdr.salt[1] = salt1;
            let (_, normalized) = self.hdr.encode_for_publish()?;
            self.hdr = normalized;
            self.heap_index.hdr = Some(normalized);
            self.heap_index.ckpt.n_backfill = 0;
            self.heap_index.ckpt.n_backfill_attempted = 0;
            self.heap_index.ckpt.read_marks = [
                0,
                0,
                READMARK_NOT_USED,
                READMARK_NOT_USED,
                READMARK_NOT_USED,
            ];
            self.heap_index.reset_empty();
            if let Some(ref wal_fd) = self.wal_file {
                wal_fd.truncate(0)?;
            }
        }

        Ok((
            WalCheckpointResult {
                busy: false,
                n_log: i32::try_from(self.hdr.mx_frame).unwrap_or(i32::MAX),
                n_ckpt: i32::try_from(self.heap_index.ckpt.n_backfill).unwrap_or(i32::MAX),
            },
            is_changed,
        ))
    }

    // ========================================================================
    // Last-Connection Close (`sqlite3WalClose`, `wal.c:2589-2653`)
    // ========================================================================

    /// Tears down an unestablished `Wal` handle after an initial open/recovery failure without
    /// checkpointing or deleting `<db>-wal`.
    pub fn abort_open(mut self) {
        let _ = self.end_read_transaction();
        if let Some(shm) = self.shm.take() {
            let _ = shm.close_shared(false);
        }
        if let Some(mut wal_fd) = self.wal_file.take() {
            let _ = wal_fd.close();
        }
    }

    /// Closes this WAL session (`sqlite3WalClose`, `wal.c:2589-2653`):
    /// - Releases any held read/write guards.
    /// - When `!readonly` and `db_file` can acquire (or already holds) `LockLevel::Exclusive`,
    ///   runs a `PASSIVE` checkpoint, closes `-shm` (`delete_if_last = !persist_wal`), closes `-wal`,
    ///   and deletes `-wal` unless `persist_wal` is `true`!
    /// - When `LockLevel::Exclusive` returns `Busy` (other connections remain open), cleanly closes
    ///   this connection's `-shm` (`delete_if_last = false`) and `-wal` without disturbing others!
    pub fn close(
        mut self,
        db_file: &dyn VfsFile,
        sync_flags: Option<SyncFlags>,
        persist_wal: bool,
    ) -> Result<()> {
        let _ = self.end_read_transaction();

        let mut is_delete = false;
        let mut first_err: Option<SqliteError> = None;

        if self.read_only_flags == WAL_RDWR && !self.immutable {
            let prev_lock = db_file.lock_level();
            let got_exclusive = if prev_lock >= sqliters_vfs::LockLevel::Exclusive {
                true
            } else {
                let lock_res = if prev_lock == sqliters_vfs::LockLevel::NoLock {
                    db_file
                        .lock(sqliters_vfs::LockLevel::Shared)
                        .and_then(|()| db_file.lock(sqliters_vfs::LockLevel::Exclusive))
                } else {
                    db_file.lock(sqliters_vfs::LockLevel::Exclusive)
                };
                if lock_res.is_ok() {
                    true
                } else {
                    let _ = db_file.unlock(prev_lock);
                    false
                }
            };

            if got_exclusive {
                if self.exclusive_mode == WalExclusiveMode::Normal {
                    self.exclusive_mode = WalExclusiveMode::Exclusive;
                }
                match self.checkpoint(db_file, WalCheckpointMode::Passive, None, sync_flags, None) {
                    Ok((res, _)) if !res.busy => {
                        if !persist_wal {
                            is_delete = true;
                        } else if self.mx_wal_size >= 0
                            && let Some(ref wal_fd) = self.wal_file
                            && let Err(e) = wal_fd.truncate(0)
                            && first_err.is_none()
                        {
                            first_err = Some(e);
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        first_err = Some(e);
                    }
                }
            }
        }

        if let Some(shm) = self.shm.take()
            && let Err(e) = shm.close_shared(is_delete)
            && first_err.is_none()
        {
            first_err = Some(e);
        }

        if let Some(mut wal_fd) = self.wal_file.take()
            && let Err(e) = wal_fd.close()
            && first_err.is_none()
        {
            first_err = Some(e);
        }

        if is_delete
            && let Err(e) = self.vfs.delete(&self.wal_path, false)
            && first_err.is_none()
        {
            first_err = Some(e);
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
