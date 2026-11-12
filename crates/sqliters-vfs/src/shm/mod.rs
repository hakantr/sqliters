//! Protocol-oriented Safe Rust WAL-index (`-shm`) subsystem (`sqlite/src/wal.c` 3.54.1).
//!
//! No raw pointers, `&[u8]`, `&mut [u8]`, or unguarded shared-memory field mutators are exposed.
//! Header decoding/encoding operates strictly on owned validated structs (`WalIndexHeaderData`,
//! `WalCkptInfoSnapshot`), `aLock[0..8]` (`120..128`) is completely sealed from memory reads and
//! writes, `nBackfillAttempted` (`128..132`) remains a normal mapped 32-bit WAL-index field despite
//! the OS Deadman Switch lock on byte `128`, and mutating operations require owned state-machine
//! lock guards (`WriterTxnGuard`, `CheckpointTxnGuard`, `RecoveryTxnGuard`).
#![forbid(unsafe_code)]

pub mod locks;
pub mod mapping;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sqliters_types::{
    ErrorCode, Result, SqliteError, decode_shm_page_size, encode_shm_page_size, extended,
};

pub use locks::{
    CheckpointMode, CheckpointTxnGuard, CkptRecoveryGuard, InitRecoveryGuard, OwnedInitWriteGuard,
    OwnedWalReadGuard, OwnedWriterTxnGuard, ReadonlyHeaderProbe, RecoveryTxnGuard, ShmLockKind,
    WAL_CKPT_LOCK, WAL_READ_LOCK_BASE, WAL_RECOVER_LOCK, WAL_SHM_DMS_OFFSET, WAL_SHM_NLOCK,
    WAL_WRITE_LOCK, WalLockMode, WalLockState, WalReadGuard, WriterTxnGuard, wal_lock_byte_offset,
    wal_read_lock_slot,
};
pub use mapping::{
    MapView, SHM_CHUNK_SIZE, ShmChunk, WAL_LOCK_LEN, WAL_LOCK_OFFSET, checked_region_byte_range,
    shm_barrier,
};

/// Type alias matching the design contract in `implementation_plan.md` line 110.
pub type ShmError = SqliteError;
/// Type alias matching `implementation_plan.md` line 113.
pub type WalWriteGuard<'a> = WriterTxnGuard<'a>;
/// Type alias matching `implementation_plan.md` line 114.
pub type WalCkptGuard<'a> = CheckpointTxnGuard<'a>;

/// Maximum supported WAL-index format version (`WALINDEX_MAX_VERSION` in `sqlite/src/wal.c:278`).
pub const WALINDEX_MAX_VERSION: u32 = 3_007_000;

/// Byte offset of the first 48-byte `WalIndexHdr` copy (`0..48`).
pub const WALINDEX_HDR_COPY1_OFFSET: usize = 0;
/// Byte offset of the second 48-byte `WalIndexHdr` copy (`48..96`).
pub const WALINDEX_HDR_COPY2_OFFSET: usize = 48;
/// Byte size of a single `WalIndexHdr` copy (`48`).
pub const WALINDEX_HDR_COPY_LEN: usize = 48;

/// Byte offset of `WalCkptInfo` (`96..136`).
pub const WAL_CKPT_INFO_OFFSET: usize = 96;
/// Byte offset of `WalCkptInfo.nBackfill` (`96..100`).
pub const WAL_CKPT_NBACKFILL_OFFSET: usize = 96;
/// Byte offset of `WalCkptInfo.aReadMark[0..5]` (`100..120`).
pub const WAL_CKPT_READMARK_OFFSET: usize = 100;
/// Byte offset of `WalCkptInfo.aLock[0..8]` (`120..128`, OS lock bytes — never accessed as data).
pub const WAL_CKPT_LOCK_ARRAY_OFFSET: usize = 120;
/// Byte offset of `WalCkptInfo.nBackfillAttempted` (`128..132`, normal mapped WAL-index field).
pub const WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET: usize = 128;
/// Byte offset of `WalCkptInfo.notUsed0` reserved padding (`132..136`).
pub const WAL_CKPT_RESERVED_OFFSET: usize = 132;
/// Total byte size of the two `WalIndexHdr` copies plus `WalCkptInfo` (`136`).
pub const WALINDEX_HDR_TOTAL_SIZE: usize = 136;

/// Number of reader mark / lock slots (`WAL_NREADER = 5`, indices `0..=4`).
pub const WAL_NREADER: usize = 5;
/// Unused reader mark sentinel (`READMARK_NOT_USED = 0xffffffff` in `sqlite/src/wal.c:401`).
pub const READMARK_NOT_USED: u32 = 0xffff_ffff;

/// Frame capacity of WAL-index blocks `>= 1` (`HASHTABLE_NPAGE = 4096`).
pub const HASHTABLE_NPAGE: u32 = 4096;
/// Frame capacity of WAL-index block `0` (`HASHTABLE_NPAGE_ONE = 4096 - 136/4 = 4062`).
pub const HASHTABLE_NPAGE_ONE: u32 = 4062;
/// Number of `u16` hash slots per 32 KiB WAL-index block (`HASHTABLE_NSLOT = 8192`).
pub const HASHTABLE_NSLOT: usize = 8192;
/// Multiplicative hash constant (`HASHTABLE_HASH_1 = 383` in `sqlite/src/wal.c:648`).
pub const HASHTABLE_HASH_PRIME: u32 = 383;

const HASH_TABLE_BYTE_OFFSET: usize = 16_384;

/// Word byte-order mode for WAL / SHM checksum calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WalChecksumEndian {
    /// Interpret each 4-byte word in host native byte order (`walIndexWriteHdr` / `walIndexTryHdr`).
    Native,
    /// Interpret each 4-byte word as big-endian (`WAL_MAGIC | 1`).
    Big,
    /// Interpret each 4-byte word as little-endian (`WAL_MAGIC`).
    Little,
}

/// Exact port of `walChecksumBytes` (`sqlite/src/wal.c:888-944`).
///
/// `data.len()` must be a positive multiple of 8 bytes. When `native_endian` is `true`, words are
/// decoded in native byte order; when `false`, each native 32-bit word is byte-swapped
/// (`BYTESWAP32`).
pub fn wal_checksum_bytes(
    native_endian: bool,
    data: &[u8],
    initial: Option<[u32; 2]>,
) -> Result<[u32; 2]> {
    let mode = if native_endian {
        WalChecksumEndian::Native
    } else if cfg!(target_endian = "little") {
        WalChecksumEndian::Big
    } else {
        WalChecksumEndian::Little
    };
    wal_checksum_with_endian(mode, data, initial)
}

/// Computes or extends the 8-byte SQLite WAL/SHM cumulative checksum using `endian`.
pub fn wal_checksum_with_endian(
    endian: WalChecksumEndian,
    data: &[u8],
    initial: Option<[u32; 2]>,
) -> Result<[u32; 2]> {
    if data.is_empty() || !data.len().is_multiple_of(8) {
        return Err(SqliteError::misuse(
            "walChecksumBytes requires a non-empty multiple of 8 bytes",
        ));
    }
    let [s1, s2] = initial.unwrap_or([0, 0]);
    // `data.len()` is a multiple of 8, so it splits exactly into 4-byte words and word pairs.
    let words = data.as_chunks::<4>().0;
    Ok(match endian {
        WalChecksumEndian::Native => wal_checksum_words(words, s1, s2, u32::from_ne_bytes),
        WalChecksumEndian::Big => wal_checksum_words(words, s1, s2, u32::from_be_bytes),
        WalChecksumEndian::Little => wal_checksum_words(words, s1, s2, u32::from_le_bytes),
    })
}

/// Checksum kernel over whole 4-byte words, monomorphized per byte order so each variant is a
/// tight loop of two word loads and four additions per 8 input bytes (the loop-carried
/// dependency is the two `s2 -> s1 -> s2` additions).
#[inline(always)]
fn wal_checksum_words(
    words: &[[u8; 4]],
    mut s1: u32,
    mut s2: u32,
    decode: impl Fn([u8; 4]) -> u32,
) -> [u32; 2] {
    for pair in words.as_chunks::<2>().0 {
        s1 = s1.wrapping_add(decode(pair[0])).wrapping_add(s2);
        s2 = s2.wrapping_add(decode(pair[1])).wrapping_add(s1);
    }
    [s1, s2]
}

/// Returns the 0-based 32 KiB WAL-index region containing 1-based `frame_no`
/// (`walFramePage` in `sqlite/src/wal.c:1235`).
#[inline]
pub const fn wal_frame_page(frame_no: u32) -> u32 {
    if frame_no <= HASHTABLE_NPAGE_ONE {
        0
    } else {
        (frame_no - HASHTABLE_NPAGE_ONE - 1) / HASHTABLE_NPAGE + 1
    }
}

/// Computes the initial hash slot index `0..8192` for 1-based `pgno` (`walHash`, `wal.c:1170`).
#[inline]
pub const fn wal_hash_slot(pgno: u32) -> usize {
    (pgno.wrapping_mul(HASHTABLE_HASH_PRIME) as usize) & (HASHTABLE_NSLOT - 1)
}

#[inline]
const fn wal_next_hash_slot(slot: usize) -> usize {
    (slot + 1) & (HASHTABLE_NSLOT - 1)
}

/// Owned, validated representation of the 48-byte `WalIndexHdr` (`sqlite/src/wal.c:321-333`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalIndexHeaderData {
    /// `iVersion` (`@0..4`, `3_007_000`).
    pub version: u32,
    /// `unused` padding (`@4..8`).
    pub unused: u32,
    /// `iChange` transaction counter (`@8..12`).
    pub change_counter: u32,
    /// `isInit` (`@12`, `1` when initialized).
    pub is_init: u8,
    /// `bigEndCksum` (`@13`, `1` if `.wal` frame checksums use big-endian words).
    pub big_end_cksum: u8,
    /// Validated database page size in bytes (`512..=65536`; encoded at `@14..16` with `65536 -> 1`).
    pub page_size: u32,
    /// `mxFrame` index of last valid committed frame in `.wal` (`@16..20`).
    pub mx_frame: u32,
    /// `nPage` database image size in pages (`@20..24`).
    pub n_page: u32,
    /// `aFrameCksum[0..2]` cumulative checksum of last valid `.wal` frame (`@24..32`).
    pub frame_cksum: [u32; 2],
    /// `aSalt[0..2]` copied from `.wal` header `16..24` (`@32..40`, big-endian 32-bit integers).
    pub salt: [u32; 2],
    /// `aCksum[0..2]` native-endian checksum over header bytes `0..40` (`@40..48`).
    pub cksum: [u32; 2],
}

impl WalIndexHeaderData {
    /// Creates an initialized `WalIndexHeaderData` template with validated `page_size` and
    /// `version = WALINDEX_MAX_VERSION`.
    pub fn new(
        page_size: u32,
        mx_frame: u32,
        n_page: u32,
        change_counter: u32,
        big_end_cksum: bool,
        frame_cksum: [u32; 2],
        salt: [u32; 2],
    ) -> Result<Self> {
        if !(mx_frame == 0 && page_size == 0) {
            let _ = encode_shm_page_size(page_size)?;
        }
        let mut hdr = Self {
            version: WALINDEX_MAX_VERSION,
            unused: 0,
            change_counter,
            is_init: 1,
            big_end_cksum: u8::from(big_end_cksum),
            page_size,
            mx_frame,
            n_page,
            frame_cksum,
            salt,
            cksum: [0, 0],
        };
        let (_, finalized) = hdr.encode_for_publish()?;
        hdr.cksum = finalized.cksum;
        Ok(hdr)
    }

    /// Returns the raw 8-byte wire representation of `aSalt` (`@32..40`, matching `.wal` `@16..24`).
    #[inline]
    pub const fn salt_wire_bytes(&self) -> [u8; 8] {
        let s0 = self.salt[0].to_be_bytes();
        let s1 = self.salt[1].to_be_bytes();
        [s0[0], s0[1], s0[2], s0[3], s1[0], s1[1], s1[2], s1[3]]
    }

    /// Encodes a 48-byte `WalIndexHdr` following `walIndexWriteHdr` (`wal.c:979-982`), forcing
    /// `is_init = 1` and `version = WALINDEX_MAX_VERSION` and computing `aCksum` over `0..40`.
    pub fn encode_for_publish(&self) -> Result<([u8; WALINDEX_HDR_COPY_LEN], Self)> {
        let encoded_sz_page = if self.mx_frame == 0 && self.page_size == 0 {
            0u16
        } else {
            encode_shm_page_size(self.page_size)?
        };
        let mut out = [0u8; WALINDEX_HDR_COPY_LEN];
        let mut normalized = *self;
        normalized.version = WALINDEX_MAX_VERSION;
        normalized.is_init = 1;

        out[0..4].copy_from_slice(&normalized.version.to_ne_bytes());
        out[4..8].copy_from_slice(&normalized.unused.to_ne_bytes());
        out[8..12].copy_from_slice(&normalized.change_counter.to_ne_bytes());
        out[12] = normalized.is_init;
        out[13] = normalized.big_end_cksum;
        out[14..16].copy_from_slice(&encoded_sz_page.to_ne_bytes());
        out[16..20].copy_from_slice(&normalized.mx_frame.to_ne_bytes());
        out[20..24].copy_from_slice(&normalized.n_page.to_ne_bytes());
        out[24..28].copy_from_slice(&normalized.frame_cksum[0].to_ne_bytes());
        out[28..32].copy_from_slice(&normalized.frame_cksum[1].to_ne_bytes());
        out[32..40].copy_from_slice(&normalized.salt_wire_bytes());

        let cksum = wal_checksum_bytes(true, &out[0..40], None)?;
        normalized.cksum = cksum;
        out[40..44].copy_from_slice(&cksum[0].to_ne_bytes());
        out[44..48].copy_from_slice(&cksum[1].to_ne_bytes());
        Ok((out, normalized))
    }

    /// Verifies and decodes two 48-byte copies of `WalIndexHdr` (`walIndexTryHdr` + version check).
    pub fn decode_verified_pair(
        copy1: &[u8; WALINDEX_HDR_COPY_LEN],
        copy2: &[u8; WALINDEX_HDR_COPY_LEN],
    ) -> Result<Self> {
        if copy1 != copy2 {
            return Err(SqliteError::corrupt(
                "WAL-index header copies 0..48 and 48..96 do not match",
            ));
        }
        let is_init = copy1[12];
        if is_init != 1 {
            return Err(SqliteError::corrupt(format!(
                "WAL-index header is not initialized (isInit={is_init})"
            )));
        }
        let expected_cksum = wal_checksum_bytes(true, &copy1[0..40], None)?;
        let cksum0 = u32::from_ne_bytes([copy1[40], copy1[41], copy1[42], copy1[43]]);
        let cksum1 = u32::from_ne_bytes([copy1[44], copy1[45], copy1[46], copy1[47]]);
        if expected_cksum != [cksum0, cksum1] {
            return Err(SqliteError::corrupt("WAL-index header checksum mismatch"));
        }

        let version = u32::from_ne_bytes([copy1[0], copy1[1], copy1[2], copy1[3]]);
        if version != WALINDEX_MAX_VERSION {
            // Matches `wal.c:2828-2829`: valid checksum with unsupported version -> SQLITE_CANTOPEN!
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                format!("unsupported WAL-index version: {version}"),
            ));
        }

        let mx_frame = u32::from_ne_bytes([copy1[16], copy1[17], copy1[18], copy1[19]]);
        let raw_sz_page = u16::from_ne_bytes([copy1[14], copy1[15]]);
        // In C SQLite (`wal.c:1446-1453, 1604`), `walIndexRecover` on an empty or uncommitted
        // `-wal` (`mxFrame == 0`) leaves `pWal->hdr.szPage == 0` while setting `isInit = 1`.
        let page_size = if mx_frame == 0 && raw_sz_page == 0 {
            0
        } else {
            decode_shm_page_size(raw_sz_page)?
        };
        let unused = u32::from_ne_bytes([copy1[4], copy1[5], copy1[6], copy1[7]]);
        let change_counter = u32::from_ne_bytes([copy1[8], copy1[9], copy1[10], copy1[11]]);
        let big_end_cksum = copy1[13];
        let n_page = u32::from_ne_bytes([copy1[20], copy1[21], copy1[22], copy1[23]]);
        let fc0 = u32::from_ne_bytes([copy1[24], copy1[25], copy1[26], copy1[27]]);
        let fc1 = u32::from_ne_bytes([copy1[28], copy1[29], copy1[30], copy1[31]]);
        let salt0 = u32::from_be_bytes([copy1[32], copy1[33], copy1[34], copy1[35]]);
        let salt1 = u32::from_be_bytes([copy1[36], copy1[37], copy1[38], copy1[39]]);

        Ok(Self {
            version,
            unused,
            change_counter,
            is_init,
            big_end_cksum,
            page_size,
            mx_frame,
            n_page,
            frame_cksum: [fc0, fc1],
            salt: [salt0, salt1],
            cksum: [cksum0, cksum1],
        })
    }
}

/// Owned snapshot of `WalCkptInfo` (`96..136`) excluding `aLock[0..8]` (`120..128`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalCkptInfoSnapshot {
    /// `nBackfill` (`@96..100`).
    pub n_backfill: u32,
    /// `aReadMark[0..5]` (`@100..120`).
    pub read_marks: [u32; WAL_NREADER],
    /// `nBackfillAttempted` (`@128..132`, normal mapped field).
    pub n_backfill_attempted: u32,
}

/// Options controlling how [`crate::VfsFile::open_wal_shm_with_options`] attaches to `-shm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShmOpenOptions {
    /// Forces `-shm` to be opened read-only (`readonly_shm=1` URI behavior in `os_unix.c:5032`).
    pub readonly_shm: bool,
    /// If `true`, retains the `WalShm` session when DMS reports `SQLITE_READONLY_CANTINIT` so
    /// `WAL_READ_LOCK(0)` can still be acquired (`walBeginShmUnreliable` in `wal.c:2874`).
    /// When `false` (default for `open_wal_shm`), `SQLITE_READONLY_CANTINIT` is returned as `Err`.
    pub allow_unreliable_readonly: bool,
}

/// Deadman Switch initialization state observed on `-shm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShmDmsState {
    /// DMS byte `128` is held in shared mode after initialization or joining an active writer/reader.
    ActiveShared,
    /// `-shm` is read-only and no writer holds DMS (`SQLITE_READONLY_CANTINIT`).
    ReadonlyCantInit,
}

/// Public protocol-safe WAL-index operations trait (`implementation_plan.md` lines 110-115).
pub trait WalShmOps {
    /// Reads copy 1 (`0..48`), executes `shm_barrier()`, reads copy 2 (`48..96`), and verifies
    /// byte-for-byte equality, `isInit == 1`, `iVersion == 3007000`, `szPage`, and `aCksum`.
    fn read_header_verified(&self) -> Result<WalIndexHeaderData>;

    /// Acquires `SHARED WAL_READ_LOCK(slot)` (`slot` in `0..=4`) and returns an owned [`WalReadGuard`].
    fn acquire_read_slot(&self, slot: u8) -> Result<WalReadGuard<'_>>;

    /// Acquires `EXCLUSIVE WAL_WRITE_LOCK` (`byte 120`) and returns an owned [`WriterTxnGuard`].
    fn acquire_writer(&self) -> Result<WriterTxnGuard<'_>>;

    /// Acquires `EXCLUSIVE WAL_CKPT_LOCK` (`byte 121`, `CheckpointMode::Passive`) and returns an
    /// owned [`CheckpointTxnGuard`].
    fn acquire_checkpointer(&self) -> Result<CheckpointTxnGuard<'_>>;
}

/// Session-level WAL-index backend.
///
/// Invariants every implementation upholds for as long as a session stays open (i.e. until its
/// `close_session`), which [`WalShm`] relies on to cache its mapping of region 0:
/// - once `ensure_dms_ready()` has succeeded it keeps succeeding (the shared Deadman Switch is
///   held until the last session on the node closes), and
/// - once `map_region(r, _)` has returned `Some(chunk)`, region `r` stays backed by that same
///   mapping (regions are appended, never relocated or replaced, until the node is torn down).
pub(crate) trait ShmBackend: Send + Sync {
    fn is_readonly(&self) -> bool;
    fn dms_state(&self) -> ShmDmsState;
    fn ensure_dms_ready(&self) -> Result<()>;
    fn lock_slot(&self, connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()>;
    fn unlock_slot(&self, connection_id: u64, lock_idx: u8, kind: ShmLockKind) -> Result<()>;
    fn map_region(&self, region: u32, extend: bool) -> Result<Option<ShmChunk>>;
    fn mapped_region_count(&self) -> Result<u32>;
    fn close_session(&self, connection_id: u64, delete_on_last: bool) -> Result<()>;
}

/// Safe per-connection WAL-index handle bound to an open `.db` file's verified identity.
pub struct WalShm {
    backend: Arc<dyn ShmBackend>,
    state: Mutex<WalLockState>,
    closed: Mutex<bool>,
    /// Immutable copy of `state.connection_id()` (fixed at construction) so hot paths that only
    /// need the id do not have to take the `state` mutex.
    conn_id: u64,
    /// Lock-free summary of "`closed` is set OR `state` is poisoned". It is raised (never
    /// lowered) while the corresponding mutex is still held, so a `false` reading proves the
    /// handle was open and healthy at that instant and the per-operation health check can skip
    /// both mutexes; a `true` reading falls back to the fully itemized check.
    closed_or_poisoned: AtomicBool,
    /// This connection's mapping of WAL-index region 0 (header + `WalCkptInfo`), cached after the
    /// first successful `ensure_dms_ready()` + `map_region(0, false)` so the per-transaction
    /// header/read-mark reads do not repeat the backend round trip. Valid until close by the
    /// [`ShmBackend`] invariants; emptied in `close_shared` BEFORE the backend session is closed
    /// so this handle never keeps a mapping alive past `xShmUnmap`.
    region0: Mutex<Option<ShmChunk>>,
}

impl std::fmt::Debug for WalShm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalShm")
            .field("readonly", &self.backend.is_readonly())
            .field("dms_state", &self.backend.dms_state())
            .finish_non_exhaustive()
    }
}

impl WalShm {
    pub(crate) fn new(connection_id: u64, backend: Arc<dyn ShmBackend>) -> Self {
        Self {
            backend,
            state: Mutex::new(WalLockState::new(connection_id)),
            closed: Mutex::new(false),
            conn_id: connection_id,
            closed_or_poisoned: AtomicBool::new(false),
            region0: Mutex::new(None),
        }
    }

    /// Returns a cloned snapshot of this connection's [`WalLockState`].
    pub fn lock_state(&self) -> Result<WalLockState> {
        let guard = self
            .state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WalLockState mutex"))?;
        Ok(guard.clone())
    }

    /// Returns whether the underlying `-shm` handle was opened read-only.
    #[inline]
    pub fn is_readonly(&self) -> bool {
        self.backend.is_readonly()
    }

    /// Returns the current Deadman Switch state (`ActiveShared` vs `ReadonlyCantInit`).
    #[inline]
    pub fn dms_state(&self) -> ShmDmsState {
        self.backend.dms_state()
    }

    /// Returns an owned [`ShmChunk`] for `region` if already allocated on disk/memory without
    /// extending `-shm` (read-only inspection for tests & diagnostics).
    pub fn inspect_mapped_chunk(&self, region: u32) -> Result<Option<ShmChunk>> {
        self.ensure_open_and_healthy()?;
        self.backend.ensure_dms_ready()?;
        self.backend.map_region(region, false)
    }

    /// Acquires a reader snapshot on `slot` (`0..=4`), optionally claiming `slot` (`1..=4`) under
    /// a brief `EXCLUSIVE WAL_READ_LOCK(slot)` first to update `aReadMark[slot] = claim_mark`
    /// (`walTryBeginRead` in `sqlite/src/wal.c:3275-3295`).
    pub fn acquire_read_slot_with_mark(
        &self,
        slot: u8,
        claim_mark: Option<u32>,
    ) -> Result<WalReadGuard<'_>> {
        let lock_idx =
            wal_read_lock_slot(slot).ok_or_else(|| SqliteError::misuse("reader slot > 4"))?;
        let conn_id = self.begin_idle_transition()?;

        // Reader slots 1..=4 read WAL frames via the shared wal-index and therefore require
        // DMS initialization (`ActiveShared`); only slot 0 (`WAL_READ_LOCK(0)`) may be taken
        // under `ReadonlyCantInit` (`walBeginShmUnreliable` fallback reading `.db` directly).
        if slot > 0 {
            self.backend.ensure_dms_ready()?;
        }

        if let Some(mark) = claim_mark {
            if slot == 0 {
                return Err(SqliteError::misuse(
                    "aReadMark[0] cannot be updated via acquire_read_slot_with_mark",
                ));
            }
            let chunk0 = self.backend.map_region(0, false)?.ok_or_else(|| {
                SqliteError::corrupt("WAL-index region 0 missing while updating aReadMark")
            })?;
            self.backend
                .lock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)?;
            let store_res = chunk0.store_u32(
                WAL_CKPT_READMARK_OFFSET + (slot as usize) * 4,
                mark,
                Ordering::Release,
            );
            let unlock_res = self
                .backend
                .unlock_slot(conn_id, lock_idx, ShmLockKind::Exclusive);
            if let Err(e) = store_res {
                if unlock_res.is_err() {
                    self.poison_connection("failed to unlock exclusive read slot after store err");
                }
                return Err(e);
            }
            unlock_res?;
        }

        self.backend
            .lock_slot(conn_id, lock_idx, ShmLockKind::Shared)?;
        let epoch = {
            let mut st = self.lock_state_guard()?;
            st.record_lock(lock_idx, ShmLockKind::Shared);
            st.set_mode(WalLockMode::Reading { slot });
            st.epoch()
        };
        Ok(WalReadGuard::new(self, slot, epoch))
    }

    /// Acquires a [`CheckpointTxnGuard`] for `mode` (`Passive`, `Full`, `Restart`, or `Truncate`).
    ///
    /// Enforces `CKPT -> WRITE` acquisition order and rolls back `CKPT` if `WRITE` fails!
    pub fn acquire_checkpoint(&self, mode: CheckpointMode) -> Result<CheckpointTxnGuard<'_>> {
        let conn_id = self.begin_idle_transition()?;
        self.backend.ensure_dms_ready()?;

        let mut stack = Vec::with_capacity(7);
        self.backend
            .lock_slot(conn_id, WAL_CKPT_LOCK, ShmLockKind::Exclusive)?;
        stack.push(WAL_CKPT_LOCK);

        if mode != CheckpointMode::Passive
            && let Err(err) =
                self.backend
                    .lock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Exclusive)
        {
            // Roll back `CKPT` in reverse order on partial acquisition failure!
            if let Err(unlock_err) =
                self.backend
                    .unlock_slot(conn_id, WAL_CKPT_LOCK, ShmLockKind::Exclusive)
            {
                self.poison_connection(format!(
                    "failed to roll back WAL_CKPT_LOCK after WRITE failure: {unlock_err}"
                ));
            }
            return Err(err);
        } else if mode != CheckpointMode::Passive {
            stack.push(WAL_WRITE_LOCK);
        }

        let epoch = {
            let mut st = self.lock_state_guard()?;
            for &idx in &stack {
                st.record_lock(idx, ShmLockKind::Exclusive);
            }
            st.set_mode(WalLockMode::Checkpointing { mode });
            st.epoch()
        };
        Ok(CheckpointTxnGuard::new(self, mode, stack, epoch))
    }

    /// Acquires a standalone [`RecoveryTxnGuard`] (`WRITE -> CKPT -> RECOVER`, never `READ(0)`),
    /// rolling back partially acquired locks in reverse order on `BUSY` or I/O error.
    pub fn acquire_recovery(&self) -> Result<RecoveryTxnGuard<'_>> {
        let conn_id = self.begin_idle_transition()?;
        self.backend.ensure_dms_ready()?;

        let order = [WAL_WRITE_LOCK, WAL_CKPT_LOCK, WAL_RECOVER_LOCK];
        let mut acquired: Vec<u8> = Vec::with_capacity(3);
        for &idx in &order {
            if let Err(err) = self.backend.lock_slot(conn_id, idx, ShmLockKind::Exclusive) {
                for &held_idx in acquired.iter().rev() {
                    if let Err(uerr) =
                        self.backend
                            .unlock_slot(conn_id, held_idx, ShmLockKind::Exclusive)
                    {
                        self.poison_connection(format!(
                            "failed to roll back recovery lock {held_idx}: {uerr}"
                        ));
                    }
                }
                return Err(err);
            }
            acquired.push(idx);
        }

        let epoch = {
            let mut st = self.lock_state_guard()?;
            for &idx in &acquired {
                st.record_lock(idx, ShmLockKind::Exclusive);
            }
            st.set_mode(WalLockMode::Recovering { reader_slot: None });
            st.epoch()
        };
        Ok(RecoveryTxnGuard::new_standalone(self, epoch))
    }

    /// Explicitly closes this `WalShm` session (`xShmUnmap`), optionally deleting `-shm` if this
    /// is the final open connection on the node. Fails with `SQLITE_BUSY` if any guard lock is
    /// still active on this connection, and surfaces any recorded poison or OS close error.
    pub fn close(&mut self, delete_if_last: bool) -> Result<()> {
        self.close_shared(delete_if_last)
    }

    /// Explicitly closes this `WalShm` session via `&self` (allowing `Arc<WalShm>` teardown once
    /// all transaction guards on this connection have been dropped).
    pub fn close_shared(&self, delete_if_last: bool) -> Result<()> {
        let mut closed_guard = self
            .closed
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned closed mutex"))?;
        if *closed_guard {
            return Ok(());
        }

        let (conn_id, poisoned_err) = {
            let st = self.lock_state_guard()?;
            if st.held_mask() != 0 && !st.is_poisoned() {
                return Err(SqliteError::new(
                    ErrorCode::Busy,
                    "cannot close WalShm while lock guards are still held",
                ));
            }
            let perr = if st.is_poisoned() {
                Some(SqliteError::new(
                    ErrorCode::IoErr,
                    st.poison_message()
                        .unwrap_or("poisoned WAL-index lock state"),
                ))
            } else {
                None
            };
            (st.connection_id(), perr)
        };

        *closed_guard = true;
        self.closed_or_poisoned.store(true, Ordering::Release);
        drop(closed_guard);

        // Release the cached region-0 mapping before the backend tears the session down (the
        // closed flag is already visible, so `region0_for_read` cannot repopulate the slot).
        let cached_region0 = match self.region0.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        drop(cached_region0);

        let close_res = self.backend.close_session(conn_id, delete_if_last);
        if let Some(err) = poisoned_err {
            return Err(err);
        }
        close_res
    }

    /// Re-probes Deadman Switch readiness on the underlying `-shm` session (`walBeginShmUnreliable`,
    /// `wal.c:2923`): returns `Ok(ShmDmsState::ActiveShared)` if a writer has since initialized DMS,
    /// `Ok(ShmDmsState::ReadonlyCantInit)` if `-shm` remains uninitialized on a read-only handle,
    /// or `Err` on I/O or lock failure.
    pub fn reprobe_dms_ready(&self) -> Result<ShmDmsState> {
        let _ = self.ensure_open_and_healthy()?;
        match self.backend.ensure_dms_ready() {
            Ok(()) => Ok(ShmDmsState::ActiveShared),
            Err(e) if e.extended_code == extended::SQLITE_READONLY_CANTINIT => {
                Ok(ShmDmsState::ReadonlyCantInit)
            }
            Err(other) => Err(other),
        }
    }

    /// Attempts lock-free dual-copy `WalIndexHdr` + `WalCkptInfo` read (`walIndexTryHdr`,
    /// `wal.c:2690-2735`):
    /// - Propagates `SQLITE_READONLY_CANTINIT` if DMS cannot be initialized on a read-only `-shm`.
    /// - Propagates `ErrorCode::CantOpen` if header checksum is valid with an unsupported version.
    /// - Returns `Ok(None)` if region 0 is not yet allocated or header copies/checksum are invalid.
    /// - Returns `Ok(Some((hdr, ckpt)))` when both copies match with valid checksum and version.
    pub fn try_read_header_and_ckpt_info(
        &self,
    ) -> Result<Option<(WalIndexHeaderData, WalCkptInfoSnapshot)>> {
        self.try_read_header_and_ckpt_info_internal()
    }

    /// Probes `SHARED WAL_WRITE_LOCK` and (on `Busy`) `SHARED WAL_RECOVER_LOCK` from an `Idle`
    /// read-only `-shm` connection when `WalIndexHdr` is uninitialized (`wal.c:2788-2793, 3191-3196`).
    pub fn probe_readonly_bad_header(&self) -> Result<ReadonlyHeaderProbe> {
        let conn_id = self.begin_idle_transition()?;
        self.backend.ensure_dms_ready()?;
        match self
            .backend
            .lock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Shared)
        {
            Ok(()) => {
                self.backend
                    .unlock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Shared)?;
                Ok(ReadonlyHeaderProbe::ReadonlyRecovery)
            }
            Err(e) if e.code == ErrorCode::Busy => self.probe_recover_lock_internal(conn_id),
            Err(other) => Err(other),
        }
    }

    /// Probes `SHARED WAL_RECOVER_LOCK` when `WAL_WRITE_LOCK` returned `Busy` (`wal.c:3191-3196`),
    /// distinguishing an active ordinary writer (`WriterActiveRetry`) from active WAL recovery
    /// (`BusyRecovery`).
    pub fn probe_recover_lock(&self) -> Result<ReadonlyHeaderProbe> {
        let conn_id = self.ensure_open_and_healthy()?;
        self.probe_recover_lock_internal(conn_id)
    }

    /// Acquires an [`OwnedInitWriteGuard`] holding `EXCLUSIVE WAL_WRITE_LOCK` from `Idle` for
    /// `walIndexReadHdr` header initialization / `walIndexRecover` (`wal.c:2796-2820`).
    pub fn acquire_owned_init_write(shm: &Arc<Self>) -> Result<OwnedInitWriteGuard> {
        let _ = shm.acquire_writer_from_idle_internal()?;
        Ok(OwnedInitWriteGuard::new(Arc::clone(shm)))
    }

    /// Claims `aReadMark[slot] = mark` (`slot` in `1..=4`) under a brief `EXCLUSIVE WAL_READ_LOCK(slot)`
    /// without acquiring `SHARED WAL_READ_LOCK(slot)` (`wal.c:3275-3286`).
    ///
    /// - Returns `Ok(true)` if the slot was claimed and `aReadMark[slot]` updated to `mark`.
    /// - Returns `Ok(false)` if `EXCLUSIVE WAL_READ_LOCK(slot)` failed with `ErrorCode::Busy`.
    /// - Propagates any non-`Busy` error immediately.
    pub fn try_claim_read_mark(
        &self,
        slot: u8,
        mark: u32,
        exclusive_db_mode: bool,
    ) -> Result<bool> {
        if slot == 0 {
            return Err(SqliteError::misuse(
                "aReadMark[0] cannot be updated via try_claim_read_mark",
            ));
        }
        let lock_idx =
            wal_read_lock_slot(slot).ok_or_else(|| SqliteError::misuse("reader slot > 4"))?;
        let conn_id = self.begin_idle_transition()?;
        let region0 = self.region0_for_read()?;
        let chunk0 = region0.as_ref().ok_or_else(|| {
            SqliteError::corrupt("WAL-index region 0 missing while updating aReadMark")
        })?;

        if !exclusive_db_mode {
            match self
                .backend
                .lock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)
            {
                Ok(()) => {}
                Err(e) if e.code == ErrorCode::Busy => return Ok(false),
                Err(other) => return Err(other),
            }
        }

        let store_res = chunk0.store_u32(
            WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
            mark,
            Ordering::Release,
        );
        let unlock_res = if !exclusive_db_mode {
            self.backend
                .unlock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)
        } else {
            Ok(())
        };
        if let Err(e) = store_res {
            if unlock_res.is_err() {
                self.poison_connection("failed to unlock exclusive read slot after store err");
            }
            return Err(e);
        }
        unlock_res?;
        Ok(true)
    }

    /// Acquires an [`OwnedWalReadGuard`] on `slot` (`0..=4`), optionally claiming `aReadMark[slot]`
    /// under brief `EXCLUSIVE WAL_READ_LOCK(slot)` first (`walTryBeginRead`), and skipping OS
    /// `WAL_READ_LOCK` acquisition when `exclusive_db_mode` is `true` (`WAL_EXCLUSIVE_MODE` under
    /// `.db` `EXCLUSIVE` lock).
    pub fn acquire_owned_read_slot_with_mark(
        shm: &Arc<Self>,
        slot: u8,
        claim_mark: Option<u32>,
        exclusive_db_mode: bool,
    ) -> Result<OwnedWalReadGuard> {
        let lock_idx =
            wal_read_lock_slot(slot).ok_or_else(|| SqliteError::misuse("reader slot > 4"))?;

        if claim_mark.is_none() && !exclusive_db_mode {
            // Common case (plain `SHARED WAL_READ_LOCK(slot)`): identical steps to the general
            // path below, but the per-connection `state` mutex is taken once and held across
            // the idle check, the OS lock and the bookkeeping instead of twice.
            let mut st = shm.begin_idle_transition_locked()?;
            if slot > 0 {
                shm.backend.ensure_dms_ready()?;
            }
            shm.backend
                .lock_slot(st.connection_id(), lock_idx, ShmLockKind::Shared)?;
            st.record_lock(lock_idx, ShmLockKind::Shared);
            st.set_mode(WalLockMode::Reading { slot });
            let epoch = st.epoch();
            drop(st);
            return Ok(OwnedWalReadGuard::new(
                Arc::clone(shm),
                Some(slot),
                slot,
                epoch,
            ));
        }

        let conn_id = shm.begin_idle_transition()?;

        if slot > 0 {
            shm.backend.ensure_dms_ready()?;
        }

        if let Some(mark) = claim_mark {
            if slot == 0 {
                return Err(SqliteError::misuse(
                    "aReadMark[0] cannot be updated via acquire_owned_read_slot_with_mark",
                ));
            }
            let chunk0 = shm.backend.map_region(0, false)?.ok_or_else(|| {
                SqliteError::corrupt("WAL-index region 0 missing while updating aReadMark")
            })?;
            if !exclusive_db_mode {
                shm.backend
                    .lock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)?;
            }
            let store_res = chunk0.store_u32(
                WAL_CKPT_READMARK_OFFSET + (slot as usize) * 4,
                mark,
                Ordering::Release,
            );
            let unlock_res = if !exclusive_db_mode {
                shm.backend
                    .unlock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)
            } else {
                Ok(())
            };
            if let Err(e) = store_res {
                if unlock_res.is_err() {
                    shm.poison_connection("failed to unlock exclusive read slot after store err");
                }
                return Err(e);
            }
            unlock_res?;
        }

        if exclusive_db_mode {
            let epoch = {
                let st = shm.lock_state_guard()?;
                st.epoch()
            };
            Ok(OwnedWalReadGuard::new(Arc::clone(shm), None, slot, epoch))
        } else {
            shm.backend
                .lock_slot(conn_id, lock_idx, ShmLockKind::Shared)?;
            let epoch = {
                let mut st = shm.lock_state_guard()?;
                st.record_lock(lock_idx, ShmLockKind::Shared);
                st.set_mode(WalLockMode::Reading { slot });
                st.epoch()
            };
            Ok(OwnedWalReadGuard::new(
                Arc::clone(shm),
                Some(slot),
                slot,
                epoch,
            ))
        }
    }

    /// Acquires a [`CheckpointTxnGuard`] matching `sqlite3WalCheckpoint` (`wal.c:4428-4452`):
    /// - Acquires `EXCLUSIVE WAL_CKPT_LOCK` WITHOUT invoking `busy_handler` (`R-53820-33897`).
    /// - For `Full`, `Restart`, or `Truncate`, attempts `EXCLUSIVE WAL_WRITE_LOCK` via `busy_handler`:
    ///   if `WAL_WRITE_LOCK` remains `Busy`, downgrades the guard to `CheckpointMode::Passive` while
    ///   retaining `WAL_CKPT_LOCK` and returns `Ok((guard, /*downgraded_busy=*/true))`!
    pub fn acquire_checkpoint_with_busy(
        &self,
        mode: CheckpointMode,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<(CheckpointTxnGuard<'_>, bool)> {
        let conn_id = self.begin_idle_transition()?;
        self.backend.ensure_dms_ready()?;

        let mut stack = Vec::with_capacity(7);
        self.backend
            .lock_slot(conn_id, WAL_CKPT_LOCK, ShmLockKind::Exclusive)?;
        stack.push(WAL_CKPT_LOCK);

        let mut effective_mode = mode;
        let mut downgraded_busy = false;

        if mode != CheckpointMode::Passive {
            match self.lock_slot_with_busy_internal(
                conn_id,
                WAL_WRITE_LOCK,
                ShmLockKind::Exclusive,
                busy_handler,
            ) {
                Ok(()) => {
                    stack.push(WAL_WRITE_LOCK);
                }
                Err(e) if e.code == ErrorCode::Busy => {
                    effective_mode = CheckpointMode::Passive;
                    downgraded_busy = true;
                }
                Err(other) => {
                    if let Err(unlock_err) =
                        self.backend
                            .unlock_slot(conn_id, WAL_CKPT_LOCK, ShmLockKind::Exclusive)
                    {
                        self.poison_connection(format!(
                            "failed to roll back WAL_CKPT_LOCK after WRITE failure: {unlock_err}"
                        ));
                    }
                    return Err(other);
                }
            }
        }

        let epoch = {
            let mut st = self.lock_state_guard()?;
            for &idx in &stack {
                st.record_lock(idx, ShmLockKind::Exclusive);
            }
            st.set_mode(WalLockMode::Checkpointing {
                mode: effective_mode,
            });
            st.epoch()
        };
        Ok((
            CheckpointTxnGuard::new(self, effective_mode, stack, epoch),
            downgraded_busy,
        ))
    }

    // ========================================================================
    // Internal guard-verified state & shared-memory helpers
    // ========================================================================

    fn lock_state_guard(&self) -> Result<std::sync::MutexGuard<'_, WalLockState>> {
        self.state
            .lock()
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned WalLockState mutex"))
    }

    /// Locks the region-0 cache slot, populating it through the backend (`ensure_dms_ready()` then
    /// `map_region(0, false)`) when empty. The slot stays `None` while region 0 is not yet
    /// allocated/mapped. Callers must already have passed [`Self::ensure_open_and_healthy`] /
    /// [`Self::begin_idle_transition`] and must not re-enter this method while holding the guard.
    #[inline]
    fn region0_for_read(&self) -> Result<std::sync::MutexGuard<'_, Option<ShmChunk>>> {
        let mut slot = self.region0.lock().map_err(|_| {
            SqliteError::new(ErrorCode::Internal, "poisoned WalShm region-0 cache mutex")
        })?;
        if slot.is_none() {
            self.fill_region0_slot(&mut slot)?;
        }
        Ok(slot)
    }

    #[cold]
    #[inline(never)]
    fn fill_region0_slot(&self, slot: &mut Option<ShmChunk>) -> Result<()> {
        // A concurrent `close_shared` empties the slot only after raising the flag, so never
        // (re)populate once the handle is closed.
        if self.closed_or_poisoned.load(Ordering::Acquire) {
            let _ = self.ensure_open_and_healthy_checked()?;
        }
        self.backend.ensure_dms_ready()?;
        *slot = self.backend.map_region(0, false)?;
        Ok(())
    }

    #[inline]
    fn ensure_open_and_healthy(&self) -> Result<u64> {
        if !self.closed_or_poisoned.load(Ordering::Acquire) {
            return Ok(self.conn_id);
        }
        self.ensure_open_and_healthy_checked()
    }

    #[cold]
    #[inline(never)]
    fn ensure_open_and_healthy_checked(&self) -> Result<u64> {
        let is_closed = self
            .closed
            .lock()
            .map(|g| *g)
            .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned closed mutex"))?;
        if is_closed {
            return Err(SqliteError::misuse("WalShm handle already closed"));
        }
        let st = self.lock_state_guard()?;
        st.ensure_healthy()?;
        Ok(st.connection_id())
    }

    fn begin_idle_transition(&self) -> Result<u64> {
        Ok(self.begin_idle_transition_locked()?.connection_id())
    }

    /// [`Self::begin_idle_transition`] that hands back the still-locked `state` guard so the
    /// caller can record the lock it is about to take without re-locking the mutex.
    #[inline]
    fn begin_idle_transition_locked(&self) -> Result<std::sync::MutexGuard<'_, WalLockState>> {
        if self.closed_or_poisoned.load(Ordering::Acquire) {
            let is_closed = self
                .closed
                .lock()
                .map(|g| *g)
                .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned closed mutex"))?;
            if is_closed {
                return Err(SqliteError::misuse("WalShm handle already closed"));
            }
        }
        let st = self.lock_state_guard()?;
        st.ensure_healthy()?;
        if st.mode() != WalLockMode::Idle || st.held_mask() != 0 {
            return Err(SqliteError::misuse(
                "WalShm connection already holds an active transaction guard",
            ));
        }
        Ok(st)
    }

    pub(crate) fn poison_connection(&self, reason: impl Into<String>) {
        if let Ok(mut st) = self.state.lock() {
            st.mark_poisoned(reason);
            self.closed_or_poisoned.store(true, Ordering::Release);
        }
    }

    pub(crate) fn try_read_header_and_ckpt_info_internal(
        &self,
    ) -> Result<Option<(WalIndexHeaderData, WalCkptInfoSnapshot)>> {
        let _ = self.ensure_open_and_healthy()?;
        let region0 = self.region0_for_read()?;
        let Some(chunk0) = region0.as_ref() else {
            return Ok(None);
        };

        let Some(hdr) = Self::try_read_header_from_chunk(chunk0)? else {
            return Ok(None);
        };
        let ckpt = self.read_ckpt_info_from_chunk(chunk0)?;
        Ok(Some((hdr, ckpt)))
    }

    /// Dual-copy `WalIndexHdr` read from an already mapped region 0 (`walIndexTryHdr`): copy 1,
    /// `shm_barrier()`, copy 2, then byte equality + `isInit` + checksum + version verification.
    /// `Ok(None)` means "dirty/uninitialized header"; an unsupported version is `Err(CantOpen)`.
    #[inline]
    fn try_read_header_from_chunk(chunk0: &ShmChunk) -> Result<Option<WalIndexHeaderData>> {
        let copy1 = Self::load_raw_header_copy(chunk0, WALINDEX_HDR_COPY1_OFFSET)?;
        shm_barrier();
        let copy2 = Self::load_raw_header_copy(chunk0, WALINDEX_HDR_COPY2_OFFSET)?;
        match WalIndexHeaderData::decode_verified_pair(&copy1, &copy2) {
            Ok(h) => Ok(Some(h)),
            Err(e) if e.code == ErrorCode::CantOpen => Err(e),
            Err(_) => Ok(None),
        }
    }

    fn read_ckpt_info_from_chunk(&self, chunk0: &ShmChunk) -> Result<WalCkptInfoSnapshot> {
        let n_backfill = chunk0.load_u32(WAL_CKPT_NBACKFILL_OFFSET, Ordering::Acquire)?;
        let mut read_marks = [0u32; WAL_NREADER];
        for (i, mark) in read_marks.iter_mut().enumerate() {
            *mark = chunk0.load_u32(WAL_CKPT_READMARK_OFFSET + i * 4, Ordering::Acquire)?;
        }
        let n_backfill_attempted =
            chunk0.load_u32(WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET, Ordering::Acquire)?;
        Ok(WalCkptInfoSnapshot {
            n_backfill,
            read_marks,
            n_backfill_attempted,
        })
    }

    fn probe_recover_lock_internal(&self, conn_id: u64) -> Result<ReadonlyHeaderProbe> {
        match self
            .backend
            .lock_slot(conn_id, WAL_RECOVER_LOCK, ShmLockKind::Shared)
        {
            Ok(()) => {
                self.backend
                    .unlock_slot(conn_id, WAL_RECOVER_LOCK, ShmLockKind::Shared)?;
                Ok(ReadonlyHeaderProbe::WriterActiveRetry)
            }
            Err(e) if e.code == ErrorCode::Busy => Ok(ReadonlyHeaderProbe::BusyRecovery),
            Err(other) => Err(other),
        }
    }

    pub(crate) fn acquire_writer_from_idle_internal(&self) -> Result<u64> {
        let conn_id = self.begin_idle_transition()?;
        self.backend.ensure_dms_ready()?;
        self.backend
            .lock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Exclusive)?;
        let mut st = self.lock_state_guard()?;
        st.record_lock(WAL_WRITE_LOCK, ShmLockKind::Exclusive);
        st.set_mode(WalLockMode::Writing { reader_slot: None });
        Ok(st.epoch())
    }

    pub(crate) fn reacquire_reader_from_idle_internal(&self, slot: u8) -> Result<u64> {
        let lock_idx =
            wal_read_lock_slot(slot).ok_or_else(|| SqliteError::misuse("reader slot > 4"))?;
        let conn_id = self.begin_idle_transition()?;
        if slot > 0 {
            self.backend.ensure_dms_ready()?;
        }
        self.backend
            .lock_slot(conn_id, lock_idx, ShmLockKind::Shared)?;
        let mut st = self.lock_state_guard()?;
        st.record_lock(lock_idx, ShmLockKind::Shared);
        st.set_mode(WalLockMode::Reading { slot });
        Ok(st.epoch())
    }

    pub(crate) fn verify_post_read_lock_internal(
        &self,
        slot: u8,
        expected_mark: Option<u32>,
        expected_hdr: &WalIndexHeaderData,
    ) -> Result<bool> {
        shm_barrier();
        if let Some(exp_mark) = expected_mark {
            let live_mark = self.read_slot_mark_internal(slot)?;
            if live_mark != exp_mark {
                return Ok(false);
            }
        }
        let _ = self.ensure_open_and_healthy()?;
        let region0 = self.region0_for_read()?;
        let Some(chunk0) = region0.as_ref() else {
            return Ok(false);
        };
        let Some(live_hdr) = Self::try_read_header_from_chunk(chunk0)? else {
            return Ok(false);
        };
        Ok(live_hdr == *expected_hdr)
    }

    /// `walTryBeginRead` post-lock step for a positive reader slot (`wal.c:3308-3320`), performed
    /// against ONE mapping of region 0: read `nBackfill` (for `minFrame = nBackfill + 1`),
    /// `shm_barrier()`, re-check `aReadMark[slot] == expected_mark`, then re-read and fully
    /// re-verify the dual-copy `WalIndexHdr` against `expected_hdr`.
    ///
    /// Returns `(n_backfill, verified)`. Equivalent to `read_ckpt_info_internal()?.n_backfill`
    /// followed by `verify_post_read_lock_internal(slot, Some(expected_mark), expected_hdr)`,
    /// minus the repeated handle-health / DMS / region-lookup round trips between the reads and
    /// the `WalCkptInfo` words (`aReadMark[..]`, `nBackfillAttempted`) the caller never uses.
    pub(crate) fn read_backfill_and_verify_post_read_lock_internal(
        &self,
        slot: u8,
        expected_mark: u32,
        expected_hdr: &WalIndexHeaderData,
    ) -> Result<(u32, bool)> {
        let _ = self.ensure_open_and_healthy()?;
        if usize::from(slot) >= WAL_NREADER {
            return Err(SqliteError::misuse("reader slot out of range"));
        }
        let region0 = self.region0_for_read()?;
        let chunk0 = region0
            .as_ref()
            .ok_or_else(|| SqliteError::corrupt("WAL-index region 0 is not mapped"))?;
        let n_backfill = chunk0.load_u32(WAL_CKPT_NBACKFILL_OFFSET, Ordering::Acquire)?;

        shm_barrier();
        let live_mark = chunk0.load_u32(
            WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
            Ordering::Acquire,
        )?;
        if live_mark != expected_mark {
            return Ok((n_backfill, false));
        }
        let Some(live_hdr) = Self::try_read_header_from_chunk(chunk0)? else {
            return Ok((n_backfill, false));
        };
        Ok((n_backfill, live_hdr == *expected_hdr))
    }

    pub(crate) fn release_reader_under_write_internal(&self, slot: u8) -> Result<()> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        let lock_idx =
            wal_read_lock_slot(slot).ok_or_else(|| SqliteError::misuse("reader slot > 4"))?;
        self.backend
            .unlock_slot(conn_id, lock_idx, ShmLockKind::Shared)?;
        let mut st = self.lock_state_guard()?;
        st.record_unlock(lock_idx, ShmLockKind::Shared);
        st.set_mode(WalLockMode::Writing { reader_slot: None });
        Ok(())
    }

    pub(crate) fn try_claim_read_mark_under_write_internal(
        &self,
        slot: u8,
        mark: u32,
        lock_os_exclusive: bool,
    ) -> Result<bool> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        if slot == 0 {
            return Err(SqliteError::misuse(
                "aReadMark[0] cannot be updated via try_claim_read_mark_under_write_internal",
            ));
        }
        let lock_idx =
            wal_read_lock_slot(slot).ok_or_else(|| SqliteError::misuse("reader slot > 4"))?;
        let chunk0 = self.ensure_region_writable_internal(0)?;

        if lock_os_exclusive {
            match self
                .backend
                .lock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)
            {
                Ok(()) => {}
                Err(e) if e.code == ErrorCode::Busy => return Ok(false),
                Err(other) => return Err(other),
            }
        }

        let store_res = chunk0.store_u32(
            WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
            mark,
            Ordering::Release,
        );
        let unlock_res = if lock_os_exclusive {
            self.backend
                .unlock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)
        } else {
            Ok(())
        };
        if let Err(e) = store_res {
            if unlock_res.is_err() {
                self.poison_connection(
                    "failed to unlock exclusive read slot under write after store err",
                );
            }
            return Err(e);
        }
        unlock_res?;
        Ok(true)
    }

    pub(crate) fn acquire_reader_under_write_internal(
        &self,
        slot: u8,
        claim_mark: Option<u32>,
        lock_os_shared: bool,
    ) -> Result<()> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        let lock_idx =
            wal_read_lock_slot(slot).ok_or_else(|| SqliteError::misuse("reader slot > 4"))?;

        if let Some(mark) = claim_mark {
            if slot == 0 {
                return Err(SqliteError::misuse(
                    "aReadMark[0] cannot be updated via acquire_reader_under_write_internal",
                ));
            }
            let chunk0 = self.ensure_region_writable_internal(0)?;
            if lock_os_shared {
                self.backend
                    .lock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)?;
            }
            let store_res = chunk0.store_u32(
                WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
                mark,
                Ordering::Release,
            );
            let unlock_res = if lock_os_shared {
                self.backend
                    .unlock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)
            } else {
                Ok(())
            };
            if let Err(e) = store_res {
                if unlock_res.is_err() {
                    self.poison_connection(
                        "failed to unlock exclusive read slot under write after store err",
                    );
                }
                return Err(e);
            }
            unlock_res?;
        }

        if lock_os_shared {
            self.backend
                .lock_slot(conn_id, lock_idx, ShmLockKind::Shared)?;
            let mut st = self.lock_state_guard()?;
            st.record_lock(lock_idx, ShmLockKind::Shared);
            st.set_mode(WalLockMode::Writing {
                reader_slot: Some(slot),
            });
        }
        Ok(())
    }

    pub(crate) fn try_reset_header_under_write_internal(
        &self,
        base_header: &WalIndexHeaderData,
        new_salt1: u32,
    ) -> Result<Option<WalIndexHeaderData>> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        let mut locked_readers: Vec<u8> = Vec::with_capacity(4);
        for slot in 1..(WAL_NREADER as u8) {
            let idx = wal_read_lock_slot(slot).unwrap();
            match self.backend.lock_slot(conn_id, idx, ShmLockKind::Exclusive) {
                Ok(()) => locked_readers.push(idx),
                Err(err) => {
                    for &held_idx in locked_readers.iter().rev() {
                        let _ = self
                            .backend
                            .unlock_slot(conn_id, held_idx, ShmLockKind::Exclusive);
                    }
                    if err.code == ErrorCode::Busy {
                        return Ok(None);
                    }
                    return Err(err);
                }
            }
        }

        let write_res = self.write_restarted_header_and_marks_internal(base_header, new_salt1);

        let mut unlock_err: Option<SqliteError> = None;
        for &held_idx in locked_readers.iter().rev() {
            if let Err(e) = self
                .backend
                .unlock_slot(conn_id, held_idx, ShmLockKind::Exclusive)
                && unlock_err.is_none()
            {
                unlock_err = Some(e);
            }
        }
        if let Some(e) = unlock_err {
            self.poison_connection(format!(
                "failed to release WAL_READ_LOCK(1..=4) after try_reset_header: {e}"
            ));
            return Err(e);
        }
        write_res.map(Some)
    }

    pub(crate) fn lock_slot_with_busy_internal(
        &self,
        conn_id: u64,
        lock_idx: u8,
        kind: ShmLockKind,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<()> {
        let Some(handler) = busy_handler else {
            return self.backend.lock_slot(conn_id, lock_idx, kind);
        };
        let mut n_retry = 0u32;
        loop {
            match self.backend.lock_slot(conn_id, lock_idx, kind) {
                Ok(()) => return Ok(()),
                Err(err) if err.code == ErrorCode::Busy => {
                    if !handler(n_retry) {
                        return Err(err);
                    }
                    n_retry = n_retry.saturating_add(1);
                }
                Err(other) => return Err(other),
            }
        }
    }

    pub(crate) fn lock_additional_exclusive_with_busy_internal(
        &self,
        lock_idx: u8,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<()> {
        let conn_id = self.ensure_open_and_healthy()?;
        self.lock_slot_with_busy_internal(conn_id, lock_idx, ShmLockKind::Exclusive, busy_handler)?;
        let mut st = self.lock_state_guard()?;
        st.record_lock(lock_idx, ShmLockKind::Exclusive);
        Ok(())
    }

    pub(crate) fn unlock_single_exclusive_internal(&self, lock_idx: u8) -> Result<()> {
        let conn_id = self.ensure_open_and_healthy()?;
        self.backend
            .unlock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)?;
        let mut st = self.lock_state_guard()?;
        st.record_unlock(lock_idx, ShmLockKind::Exclusive);
        Ok(())
    }

    pub(crate) fn read_live_header_salt_internal(&self) -> Result<[u32; 2]> {
        let _ = self.verify_holds_exclusive(WAL_CKPT_LOCK)?;
        let chunk0 = self
            .backend
            .map_region(0, false)?
            .ok_or_else(|| SqliteError::corrupt("WAL-index region 0 is not mapped"))?;
        let w0 = chunk0.load_u32(32, Ordering::Acquire)?;
        let w1 = chunk0.load_u32(36, Ordering::Acquire)?;
        let b0 = w0.to_ne_bytes();
        let b1 = w1.to_ne_bytes();
        Ok([u32::from_be_bytes(b0), u32::from_be_bytes(b1)])
    }

    pub(crate) fn read_frame_pgno_internal(&self, frame_no: u32) -> Result<u32> {
        let _ = self.ensure_open_and_healthy()?;
        if frame_no == 0 {
            return Err(SqliteError::misuse("WAL frame_no must be >= 1"));
        }
        self.backend.ensure_dms_ready()?;
        let region = wal_frame_page(frame_no);
        let chunk = self.backend.map_region(region, false)?.ok_or_else(|| {
            SqliteError::corrupt(format!(
                "WAL-index missing mapped region {region} for frame {frame_no}"
            ))
        })?;
        let (i_zero, pgno_base, capacity) = Self::region_hash_params(region)?;
        let idx = frame_no - i_zero;
        if idx == 0 || idx > capacity {
            return Err(SqliteError::misuse("WAL frame index out of block capacity"));
        }
        let pgno_rel = pgno_base + ((idx - 1) as usize) * 4;
        chunk.load_u32(pgno_rel, Ordering::Acquire)
    }

    pub(crate) fn reset_region_for_recovery_internal(&self, region: u32) -> Result<()> {
        self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        let chunk = self.ensure_region_writable_internal(region)?;
        let zero_from = if region == 0 {
            WALINDEX_HDR_TOTAL_SIZE
        } else {
            0
        };
        chunk.zero_aligned_u32_range(zero_from, SHM_CHUNK_SIZE)
    }

    pub(crate) fn read_header_verified_internal(&self) -> Result<WalIndexHeaderData> {
        let _ = self.ensure_open_and_healthy()?;
        let region0 = self.region0_for_read()?;
        let Some(chunk0) = region0.as_ref() else {
            return Err(SqliteError::corrupt(
                "WAL-index region 0 is not yet initialized",
            ));
        };

        let copy1 = Self::load_raw_header_copy(chunk0, WALINDEX_HDR_COPY1_OFFSET)?;
        shm_barrier();
        let copy2 = Self::load_raw_header_copy(chunk0, WALINDEX_HDR_COPY2_OFFSET)?;
        WalIndexHeaderData::decode_verified_pair(&copy1, &copy2)
    }

    fn load_raw_header_copy(
        chunk0: &ShmChunk,
        base_offset: usize,
    ) -> Result<[u8; WALINDEX_HDR_COPY_LEN]> {
        let mut raw = [0u8; WALINDEX_HDR_COPY_LEN];
        for i in 0..(WALINDEX_HDR_COPY_LEN / 4) {
            let word = chunk0.load_u32(base_offset + i * 4, Ordering::Acquire)?;
            raw[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }
        Ok(raw)
    }

    fn store_raw_header_copy(
        chunk0: &ShmChunk,
        base_offset: usize,
        raw: &[u8; WALINDEX_HDR_COPY_LEN],
    ) -> Result<()> {
        for i in 0..(WALINDEX_HDR_COPY_LEN / 4) {
            let word =
                u32::from_ne_bytes([raw[i * 4], raw[i * 4 + 1], raw[i * 4 + 2], raw[i * 4 + 3]]);
            chunk0.store_u32(base_offset + i * 4, word, Ordering::Release)?;
        }
        Ok(())
    }

    pub(crate) fn publish_header_internal(
        &self,
        header: &WalIndexHeaderData,
    ) -> Result<WalIndexHeaderData> {
        self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        let chunk0 = self.ensure_region_writable_internal(0)?;
        let (encoded, normalized) = header.encode_for_publish()?;

        // Binding publish contract (`walIndexWriteHdr`, `wal.c:983-985`):
        // write second copy (`48..96`) -> `shm_barrier()` -> write first copy (`0..48`).
        Self::store_raw_header_copy(&chunk0, WALINDEX_HDR_COPY2_OFFSET, &encoded)?;
        shm_barrier();
        Self::store_raw_header_copy(&chunk0, WALINDEX_HDR_COPY1_OFFSET, &encoded)?;
        shm_barrier();
        Ok(normalized)
    }

    pub(crate) fn read_slot_mark_internal(&self, slot: u8) -> Result<u32> {
        let _ = self.ensure_open_and_healthy()?;
        if usize::from(slot) >= WAL_NREADER {
            return Err(SqliteError::misuse("reader slot out of range"));
        }
        let region0 = self.region0_for_read()?;
        let chunk0 = region0
            .as_ref()
            .ok_or_else(|| SqliteError::corrupt("WAL-index region 0 is not mapped"))?;
        chunk0.load_u32(
            WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
            Ordering::Acquire,
        )
    }

    pub(crate) fn read_ckpt_info_internal(&self) -> Result<WalCkptInfoSnapshot> {
        let _ = self.ensure_open_and_healthy()?;
        let region0 = self.region0_for_read()?;
        let chunk0 = region0
            .as_ref()
            .ok_or_else(|| SqliteError::corrupt("WAL-index region 0 is not mapped"))?;
        self.read_ckpt_info_from_chunk(chunk0)
    }

    pub(crate) fn ensure_region_writable_internal(&self, region: u32) -> Result<ShmChunk> {
        self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        self.backend.ensure_dms_ready()?;
        self.backend
            .map_region(region, true)?
            .ok_or_else(|| SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "map failed"))
    }

    fn verify_holds_exclusive(&self, lock_idx: u8) -> Result<u64> {
        let conn_id = self.ensure_open_and_healthy()?;
        let st = self.lock_state_guard()?;
        if !st.holds_exclusive(lock_idx) {
            return Err(SqliteError::misuse(format!(
                "missing required exclusive WAL lock {lock_idx}"
            )));
        }
        Ok(conn_id)
    }

    #[inline]
    fn region_hash_params(region: u32) -> Result<(u32, usize, u32)> {
        if region == 0 {
            Ok((0, WALINDEX_HDR_TOTAL_SIZE, HASHTABLE_NPAGE_ONE))
        } else {
            let prev = region - 1;
            let offset_frames = prev
                .checked_mul(HASHTABLE_NPAGE)
                .and_then(|v| v.checked_add(HASHTABLE_NPAGE_ONE))
                .ok_or_else(|| SqliteError::misuse("WAL frame index overflow"))?;
            Ok((offset_frames, 0, HASHTABLE_NPAGE))
        }
    }

    pub(crate) fn record_frame_internal(
        &self,
        frame_no: u32,
        pgno: u32,
        committed_mx_frame: u32,
    ) -> Result<()> {
        if frame_no == 0 || pgno == 0 {
            return Err(SqliteError::misuse("WAL frame_no and pgno must be >= 1"));
        }
        let region = wal_frame_page(frame_no);
        let chunk = self.ensure_region_writable_internal(region)?;
        let (i_zero, pgno_base, capacity) = Self::region_hash_params(region)?;
        let idx = frame_no - i_zero;
        if idx == 0 || idx > capacity {
            return Err(SqliteError::misuse("WAL frame index out of block capacity"));
        }

        // First entry in this 32 KiB hash block zeroes the entire `aPgno` + `aHash` region (`wal.c:1353`).
        if idx == 1 {
            chunk.zero_aligned_u32_range(pgno_base, SHM_CHUNK_SIZE)?;
        }

        let pgno_rel = pgno_base + ((idx - 1) as usize) * 4;
        if chunk.load_u32(pgno_rel, Ordering::Relaxed)? != 0 {
            // Clean remnants of any aborted prior write transaction (`wal.c:1365`).
            self.truncate_frames_above_internal(committed_mx_frame)?;
            if chunk.load_u32(pgno_rel, Ordering::Relaxed)? != 0 {
                return Err(SqliteError::misuse(
                    "cannot overwrite already committed frame slot without truncating first",
                ));
            }
        }

        let mut key = wal_hash_slot(pgno);
        let mut collisions_left = idx;
        loop {
            let slot_rel = HASH_TABLE_BYTE_OFFSET + key * 2;
            let cur = chunk.load_u16(slot_rel, Ordering::Relaxed)?;
            if cur == 0 {
                // Store `aPgno` BEFORE publishing `aHash[key]` with `Release` ordering (`wal.c:1375-1376`).
                chunk.store_u32(pgno_rel, pgno, Ordering::Release)?;
                chunk.store_u16(slot_rel, idx as u16, Ordering::Release)?;
                return Ok(());
            }
            if collisions_left == 0 {
                return Err(SqliteError::corrupt(
                    "WAL-index hash table full / corrupt collision chain",
                ));
            }
            collisions_left -= 1;
            key = wal_next_hash_slot(key);
        }
    }

    pub(crate) fn truncate_frames_above_internal(&self, max_frame: u32) -> Result<()> {
        self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        let mapped_count = self.backend.mapped_region_count()?;
        if mapped_count == 0 {
            return Ok(());
        }

        if max_frame == 0 {
            if let Some(chunk0) = self.backend.map_region(0, false)? {
                chunk0.zero_aligned_u32_range(WALINDEX_HDR_TOTAL_SIZE, SHM_CHUNK_SIZE)?;
            }
            for region in 1..mapped_count {
                if let Some(chunk) = self.backend.map_region(region, false)? {
                    chunk.zero_aligned_u32_range(0, SHM_CHUNK_SIZE)?;
                }
            }
            return Ok(());
        }

        let target_region = wal_frame_page(max_frame);
        if let Some(chunk) = self.backend.map_region(target_region, false)? {
            let (i_zero, pgno_base, capacity) = Self::region_hash_params(target_region)?;
            let limit = (max_frame - i_zero) as u16;
            for slot in 0..HASHTABLE_NSLOT {
                let slot_rel = HASH_TABLE_BYTE_OFFSET + slot * 2;
                if chunk.load_u16(slot_rel, Ordering::Relaxed)? > limit {
                    chunk.store_u16(slot_rel, 0, Ordering::Release)?;
                }
            }
            let zero_from = pgno_base + usize::from(limit) * 4;
            let zero_to = pgno_base + (capacity as usize) * 4;
            if zero_from < zero_to {
                chunk.zero_aligned_u32_range(zero_from, zero_to)?;
            }
        }

        for region in (target_region + 1)..mapped_count {
            if let Some(chunk) = self.backend.map_region(region, false)? {
                chunk.zero_aligned_u32_range(0, SHM_CHUNK_SIZE)?;
            }
        }
        Ok(())
    }

    pub(crate) fn lookup_page_internal(
        &self,
        pgno: u32,
        min_frame: u32,
        max_frame: u32,
    ) -> Result<Option<u32>> {
        let _ = self.ensure_open_and_healthy()?;
        if pgno == 0 || max_frame == 0 || min_frame > max_frame {
            return Ok(None);
        }
        self.backend.ensure_dms_ready()?;

        let min_clamped = min_frame.max(1);
        let min_region = wal_frame_page(min_clamped);
        let max_region = wal_frame_page(max_frame);

        let mut region = max_region;
        loop {
            let chunk = self.backend.map_region(region, false)?.ok_or_else(|| {
                SqliteError::corrupt(format!(
                    "WAL-index missing mapped region {region} for snapshot max_frame={max_frame}"
                ))
            })?;
            let (i_zero, pgno_base, capacity) = Self::region_hash_params(region)?;

            let mut key = wal_hash_slot(pgno);
            let mut collisions_left = HASHTABLE_NSLOT;
            let mut match_frame = None;
            loop {
                let slot_val =
                    chunk.load_u16(HASH_TABLE_BYTE_OFFSET + key * 2, Ordering::Acquire)?;
                if slot_val == 0 {
                    break;
                }
                if u32::from(slot_val) > capacity {
                    return Err(SqliteError::corrupt(
                        "WAL-index hash slot exceeds block capacity",
                    ));
                }
                let frame = i_zero + u32::from(slot_val);
                if frame >= min_clamped && frame <= max_frame {
                    let pgno_rel = pgno_base + (usize::from(slot_val) - 1) * 4;
                    if chunk.load_u32(pgno_rel, Ordering::Acquire)? == pgno {
                        match_frame = Some(frame);
                    }
                }
                if collisions_left == 0 {
                    return Err(SqliteError::corrupt(
                        "WAL-index hash lookup exceeded HASHTABLE_NSLOT without empty slot",
                    ));
                }
                collisions_left -= 1;
                key = wal_next_hash_slot(key);
            }

            if match_frame.is_some() {
                return Ok(match_frame);
            }
            if region == min_region {
                break;
            }
            region -= 1;
        }
        Ok(None)
    }

    pub(crate) fn upgrade_reader_to_writer_internal(&self, slot: u8) -> Result<u64> {
        let conn_id = self.ensure_open_and_healthy()?;
        // One `state` critical section spanning the mode check, the OS lock and the bookkeeping.
        let mut st = self.lock_state_guard()?;
        if st.mode() != (WalLockMode::Reading { slot }) {
            return Err(SqliteError::misuse(
                "WalShm is not in matching Reading mode for writer upgrade",
            ));
        }
        self.backend
            .lock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Exclusive)?;
        st.record_lock(WAL_WRITE_LOCK, ShmLockKind::Exclusive);
        st.set_mode(WalLockMode::Writing {
            reader_slot: Some(slot),
        });
        Ok(st.epoch())
    }

    pub(crate) fn downgrade_writer_to_reader_internal(
        &self,
        reader_slot: Option<u8>,
    ) -> Result<u64> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        self.backend
            .unlock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Exclusive)?;
        let mut st = self.lock_state_guard()?;
        st.record_unlock(WAL_WRITE_LOCK, ShmLockKind::Exclusive);
        match reader_slot {
            Some(slot) => st.set_mode(WalLockMode::Reading { slot }),
            None => st.set_mode(WalLockMode::Idle),
        }
        Ok(st.epoch())
    }

    pub(crate) fn release_reader_internal(&self, slot: u8, from_drop: bool) -> Result<()> {
        let Some(lock_idx) = wal_read_lock_slot(slot) else {
            return Err(SqliteError::misuse("invalid reader slot"));
        };
        let conn_id = self.conn_id;
        let res = self
            .backend
            .unlock_slot(conn_id, lock_idx, ShmLockKind::Shared);
        if let Ok(mut st) = self.state.lock() {
            if res.is_ok() {
                st.record_unlock(lock_idx, ShmLockKind::Shared);
                st.set_mode(WalLockMode::Idle);
            } else if from_drop && let Err(ref e) = res {
                st.mark_poisoned(format!("failed to unlock WAL_READ_LOCK({slot}): {e}"));
                self.closed_or_poisoned.store(true, Ordering::Release);
            }
        }
        res
    }

    pub(crate) fn release_writer_internal(
        &self,
        reader_slot: Option<u8>,
        from_drop: bool,
    ) -> Result<()> {
        let conn_id = self.conn_id;
        let mut first_err: Option<SqliteError> = None;

        // Release in reverse order: `WRITE` first, then `READ(slot)` if co-held.
        match self
            .backend
            .unlock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Exclusive)
        {
            Ok(()) => {
                if let Ok(mut st) = self.state.lock() {
                    st.record_unlock(WAL_WRITE_LOCK, ShmLockKind::Exclusive);
                }
            }
            Err(e) => first_err = Some(e),
        }

        if let Some(slot) = reader_slot
            && let Some(read_idx) = wal_read_lock_slot(slot)
        {
            match self
                .backend
                .unlock_slot(conn_id, read_idx, ShmLockKind::Shared)
            {
                Ok(()) => {
                    if let Ok(mut st) = self.state.lock() {
                        st.record_unlock(read_idx, ShmLockKind::Shared);
                    }
                }
                Err(e) if first_err.is_none() => first_err = Some(e),
                Err(_) => {}
            }
        }

        if let Ok(mut st) = self.state.lock() {
            if let Some(ref e) = first_err {
                if from_drop {
                    st.mark_poisoned(format!("failed to release WriterTxnGuard: {e}"));
                    self.closed_or_poisoned.store(true, Ordering::Release);
                }
            } else {
                st.set_mode(WalLockMode::Idle);
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub(crate) fn restart_header_under_write_internal(
        &self,
        reader_slot: &mut Option<u8>,
        base_header: &WalIndexHeaderData,
        new_salt1: u32,
    ) -> Result<WalIndexHeaderData> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;

        // If this connection currently holds `SHARED WAL_READ_LOCK(1..=4)`, migrate it to
        // `WAL_READ_LOCK(0)` first just like `walRestartLog` (`wal.c:4091-4102`).
        if let Some(slot) = *reader_slot
            && slot > 0
        {
            let old_idx = wal_read_lock_slot(slot).unwrap();
            let read0_idx = wal_read_lock_slot(0).unwrap();
            self.backend
                .unlock_slot(conn_id, old_idx, ShmLockKind::Shared)?;
            {
                let mut st = self.lock_state_guard()?;
                st.record_unlock(old_idx, ShmLockKind::Shared);
            }
            if let Err(err) = self
                .backend
                .lock_slot(conn_id, read0_idx, ShmLockKind::Shared)
            {
                *reader_slot = None;
                let mut st = self.lock_state_guard()?;
                st.set_mode(WalLockMode::Writing { reader_slot: None });
                return Err(err);
            }
            *reader_slot = Some(0);
            let mut st = self.lock_state_guard()?;
            st.record_lock(read0_idx, ShmLockKind::Shared);
            st.set_mode(WalLockMode::Writing {
                reader_slot: Some(0),
            });
        }

        // Acquire `EXCLUSIVE WAL_READ_LOCK(1..=4)` to ensure no active WAL readers remain.
        let mut locked_readers: Vec<u8> = Vec::with_capacity(4);
        for slot in 1..(WAL_NREADER as u8) {
            let idx = wal_read_lock_slot(slot).unwrap();
            if let Err(err) = self.backend.lock_slot(conn_id, idx, ShmLockKind::Exclusive) {
                for &held_idx in locked_readers.iter().rev() {
                    let _ = self
                        .backend
                        .unlock_slot(conn_id, held_idx, ShmLockKind::Exclusive);
                }
                return Err(err);
            }
            locked_readers.push(idx);
        }

        let write_res = self.write_restarted_header_and_marks_internal(base_header, new_salt1);

        let mut unlock_err: Option<SqliteError> = None;
        for &held_idx in locked_readers.iter().rev() {
            if let Err(e) = self
                .backend
                .unlock_slot(conn_id, held_idx, ShmLockKind::Exclusive)
                && unlock_err.is_none()
            {
                unlock_err = Some(e);
            }
        }
        if let Some(e) = unlock_err {
            self.poison_connection(format!(
                "failed to release WAL_READ_LOCK(1..=4) after restart_header: {e}"
            ));
            return Err(e);
        }
        write_res
    }

    pub(crate) fn write_restarted_header_and_marks_internal(
        &self,
        base_header: &WalIndexHeaderData,
        new_salt1: u32,
    ) -> Result<WalIndexHeaderData> {
        let chunk0 = self.ensure_region_writable_internal(0)?;
        let mut next = *base_header;
        // Per `walRestartHdr` (`wal.c:2238-2247`): `mxFrame = 0`, `aSalt[0]++`, `aSalt[1] = salt1`;
        // `iChange` (`change_counter`) is NOT incremented here (only on `isCommit` in `walFrames`).
        next.mx_frame = 0;
        next.salt[0] = next.salt[0].wrapping_add(1);
        next.salt[1] = new_salt1;

        let published = self.publish_header_internal(&next)?;
        chunk0.store_u32(WAL_CKPT_NBACKFILL_OFFSET, 0, Ordering::Release)?;
        chunk0.store_u32(WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET, 0, Ordering::Release)?;
        chunk0.store_u32(WAL_CKPT_READMARK_OFFSET + 4, 0, Ordering::Release)?;
        for slot in 2..WAL_NREADER {
            chunk0.store_u32(
                WAL_CKPT_READMARK_OFFSET + slot * 4,
                READMARK_NOT_USED,
                Ordering::Release,
            )?;
        }
        self.truncate_frames_above_internal(0)?;
        Ok(published)
    }

    pub(crate) fn lock_additional_exclusive_internal(&self, lock_idx: u8) -> Result<()> {
        let conn_id = self.ensure_open_and_healthy()?;
        self.backend
            .lock_slot(conn_id, lock_idx, ShmLockKind::Exclusive)?;
        let mut st = self.lock_state_guard()?;
        st.record_lock(lock_idx, ShmLockKind::Exclusive);
        Ok(())
    }

    pub(crate) fn claim_safe_backfill_frame_internal(&self, mx_frame: u32) -> Result<u32> {
        self.claim_safe_backfill_frame_with_busy_internal(mx_frame, None)
    }

    pub(crate) fn claim_safe_backfill_frame_with_busy_internal(
        &self,
        mx_frame: u32,
        mut busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<u32> {
        let conn_id = self.verify_holds_exclusive(WAL_CKPT_LOCK)?;
        let chunk0 = self
            .backend
            .map_region(0, false)?
            .ok_or_else(|| SqliteError::corrupt("WAL-index region 0 is not mapped"))?;

        let mut mx_safe = mx_frame;
        for slot in 1..(WAL_NREADER as u8) {
            let y = chunk0.load_u32(
                WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
                Ordering::Acquire,
            )?;
            if mx_safe > y {
                let idx = wal_read_lock_slot(slot).unwrap();
                let already_held = {
                    let st = self.lock_state_guard()?;
                    st.holds_exclusive(idx)
                };
                if already_held {
                    let mark = if slot == 1 {
                        mx_safe
                    } else {
                        READMARK_NOT_USED
                    };
                    chunk0.store_u32(
                        WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
                        mark,
                        Ordering::Release,
                    )?;
                    continue;
                }
                match self.lock_slot_with_busy_internal(
                    conn_id,
                    idx,
                    ShmLockKind::Exclusive,
                    match busy_handler.as_mut() {
                        Some(h) => Some(&mut **h),
                        None => None,
                    },
                ) {
                    Ok(()) => {
                        let mark = if slot == 1 {
                            mx_safe
                        } else {
                            READMARK_NOT_USED
                        };
                        let store_res = chunk0.store_u32(
                            WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
                            mark,
                            Ordering::Release,
                        );
                        let unlock_res =
                            self.backend
                                .unlock_slot(conn_id, idx, ShmLockKind::Exclusive);
                        store_res?;
                        unlock_res?;
                    }
                    Err(e) if e.code == ErrorCode::Busy => {
                        mx_safe = y;
                        // Per `wal.c:2330`: `xBusy2 = 0` after the first busy reader slot!
                        busy_handler = None;
                    }
                    Err(other) => return Err(other),
                }
            }
        }
        Ok(mx_safe)
    }

    pub(crate) fn write_backfill_attempted_internal(&self, frame: u32) -> Result<()> {
        let _ = self.verify_holds_exclusive(WAL_CKPT_LOCK)?;
        let chunk0 = self
            .backend
            .map_region(0, false)?
            .ok_or_else(|| SqliteError::corrupt("WAL-index region 0 is not mapped"))?;
        chunk0.store_u32(WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET, frame, Ordering::Release)
    }

    pub(crate) fn write_backfill_completed_internal(&self, frame: u32) -> Result<()> {
        let _ = self.verify_holds_exclusive(WAL_CKPT_LOCK)?;
        let chunk0 = self
            .backend
            .map_region(0, false)?
            .ok_or_else(|| SqliteError::corrupt("WAL-index region 0 is not mapped"))?;
        chunk0.store_u32(WAL_CKPT_NBACKFILL_OFFSET, frame, Ordering::Release)
    }

    pub(crate) fn release_exclusive_stack_internal(
        &self,
        stack: &[u8],
        from_drop: bool,
    ) -> Result<()> {
        let conn_id = {
            let st = self.lock_state_guard()?;
            st.connection_id()
        };
        let mut first_err: Option<SqliteError> = None;
        for &idx in stack.iter().rev() {
            match self
                .backend
                .unlock_slot(conn_id, idx, ShmLockKind::Exclusive)
            {
                Ok(()) => {
                    if let Ok(mut st) = self.state.lock() {
                        st.record_unlock(idx, ShmLockKind::Exclusive);
                    }
                }
                Err(e) if first_err.is_none() => first_err = Some(e),
                Err(_) => {}
            }
        }
        if let Ok(mut st) = self.state.lock() {
            if let Some(ref e) = first_err {
                if from_drop {
                    st.mark_poisoned(format!("failed to release CheckpointTxnGuard: {e}"));
                    self.closed_or_poisoned.store(true, Ordering::Release);
                }
            } else {
                st.set_mode(WalLockMode::Idle);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub(crate) fn enter_recovery_from_writer_internal(
        &self,
        reader_slot: Option<u8>,
    ) -> Result<u64> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        // Acquire `CKPT` then `RECOVER` WITHOUT locking `READ(0)` (`walIndexRecover`, `wal.c:1439`).
        self.backend
            .lock_slot(conn_id, WAL_CKPT_LOCK, ShmLockKind::Exclusive)?;
        if let Err(err) = self
            .backend
            .lock_slot(conn_id, WAL_RECOVER_LOCK, ShmLockKind::Exclusive)
        {
            if let Err(uerr) =
                self.backend
                    .unlock_slot(conn_id, WAL_CKPT_LOCK, ShmLockKind::Exclusive)
            {
                self.poison_connection(format!(
                    "failed to roll back WAL_CKPT_LOCK on recovery entry failure: {uerr}"
                ));
            }
            return Err(err);
        }

        let mut st = self.lock_state_guard()?;
        st.record_lock(WAL_CKPT_LOCK, ShmLockKind::Exclusive);
        st.record_lock(WAL_RECOVER_LOCK, ShmLockKind::Exclusive);
        st.set_mode(WalLockMode::Recovering { reader_slot });
        Ok(st.epoch())
    }

    pub(crate) fn finish_recovery_header_and_marks_internal(
        &self,
        header: &WalIndexHeaderData,
    ) -> Result<(WalIndexHeaderData, [bool; WAL_NREADER])> {
        let conn_id = self.verify_holds_exclusive(WAL_WRITE_LOCK)?;
        let _ = self.verify_holds_exclusive(WAL_CKPT_LOCK)?;
        let _ = self.verify_holds_exclusive(WAL_RECOVER_LOCK)?;

        let published = self.publish_header_internal(header)?;
        let chunk0 = self.ensure_region_writable_internal(0)?;

        // Under `WRITE + CKPT + RECOVER` (without `READ(0)`):
        // `nBackfill = 0`, `nBackfillAttempted = mxFrame`, `aReadMark[0] = 0` (`wal.c:1611-1613`).
        chunk0.store_u32(WAL_CKPT_NBACKFILL_OFFSET, 0, Ordering::Release)?;
        chunk0.store_u32(
            WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET,
            published.mx_frame,
            Ordering::Release,
        )?;
        chunk0.store_u32(WAL_CKPT_READMARK_OFFSET, 0, Ordering::Release)?;

        let mut updated = [false; WAL_NREADER];
        updated[0] = true;

        for slot in 1..(WAL_NREADER as u8) {
            let idx = wal_read_lock_slot(slot).unwrap();
            match self.backend.lock_slot(conn_id, idx, ShmLockKind::Exclusive) {
                Ok(()) => {
                    let mark = if slot == 1 && published.mx_frame > 0 {
                        published.mx_frame
                    } else {
                        READMARK_NOT_USED
                    };
                    let store_res = chunk0.store_u32(
                        WAL_CKPT_READMARK_OFFSET + usize::from(slot) * 4,
                        mark,
                        Ordering::Release,
                    );
                    let unlock_res = self
                        .backend
                        .unlock_slot(conn_id, idx, ShmLockKind::Exclusive);
                    store_res?;
                    unlock_res?;
                    updated[usize::from(slot)] = true;
                }
                Err(e) if e.code == ErrorCode::Busy => {
                    // Leave `aReadMark[slot]` untouched when an existing reader holds `READ(slot)`!
                    updated[usize::from(slot)] = false;
                }
                Err(other) => return Err(other),
            }
        }
        Ok((published, updated))
    }

    pub(crate) fn exit_recovery_to_writer_internal(&self, reader_slot: Option<u8>) -> Result<u64> {
        let conn_id = self.verify_holds_exclusive(WAL_RECOVER_LOCK)?;
        let mut first_err: Option<SqliteError> = None;

        for idx in [WAL_RECOVER_LOCK, WAL_CKPT_LOCK] {
            match self
                .backend
                .unlock_slot(conn_id, idx, ShmLockKind::Exclusive)
            {
                Ok(()) => {
                    if let Ok(mut st) = self.state.lock() {
                        st.record_unlock(idx, ShmLockKind::Exclusive);
                    }
                }
                Err(e) if first_err.is_none() => first_err = Some(e),
                Err(_) => {}
            }
        }
        if let Some(err) = first_err {
            self.poison_connection(format!("failed exiting recovery locks: {err}"));
            return Err(err);
        }

        let mut st = self.lock_state_guard()?;
        st.set_mode(WalLockMode::Writing { reader_slot });
        Ok(st.epoch())
    }

    pub(crate) fn release_recovery_all_internal(
        &self,
        reader_slot: Option<u8>,
        from_drop: bool,
    ) -> Result<()> {
        let conn_id = {
            let st = self.lock_state_guard()?;
            st.connection_id()
        };
        let mut first_err: Option<SqliteError> = None;

        // Strict reverse order: `RECOVER -> CKPT -> WRITE -> READ(slot)`.
        for idx in [WAL_RECOVER_LOCK, WAL_CKPT_LOCK, WAL_WRITE_LOCK] {
            match self
                .backend
                .unlock_slot(conn_id, idx, ShmLockKind::Exclusive)
            {
                Ok(()) => {
                    if let Ok(mut st) = self.state.lock() {
                        st.record_unlock(idx, ShmLockKind::Exclusive);
                    }
                }
                Err(e) if first_err.is_none() => first_err = Some(e),
                Err(_) => {}
            }
        }

        if let Some(slot) = reader_slot
            && let Some(read_idx) = wal_read_lock_slot(slot)
        {
            match self
                .backend
                .unlock_slot(conn_id, read_idx, ShmLockKind::Shared)
            {
                Ok(()) => {
                    if let Ok(mut st) = self.state.lock() {
                        st.record_unlock(read_idx, ShmLockKind::Shared);
                    }
                }
                Err(e) if first_err.is_none() => first_err = Some(e),
                Err(_) => {}
            }
        }

        if let Ok(mut st) = self.state.lock() {
            if let Some(ref e) = first_err {
                if from_drop {
                    st.mark_poisoned(format!("failed to release RecoveryTxnGuard: {e}"));
                    self.closed_or_poisoned.store(true, Ordering::Release);
                }
            } else {
                st.set_mode(WalLockMode::Idle);
            }
        }

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl WalShmOps for WalShm {
    fn read_header_verified(&self) -> Result<WalIndexHeaderData> {
        self.read_header_verified_internal()
    }

    fn acquire_read_slot(&self, slot: u8) -> Result<WalReadGuard<'_>> {
        self.acquire_read_slot_with_mark(slot, None)
    }

    fn acquire_writer(&self) -> Result<WriterTxnGuard<'_>> {
        let conn_id = self.begin_idle_transition()?;
        self.backend.ensure_dms_ready()?;
        self.backend
            .lock_slot(conn_id, WAL_WRITE_LOCK, ShmLockKind::Exclusive)?;
        let epoch = {
            let mut st = self.lock_state_guard()?;
            st.record_lock(WAL_WRITE_LOCK, ShmLockKind::Exclusive);
            st.set_mode(WalLockMode::Writing { reader_slot: None });
            st.epoch()
        };
        Ok(WriterTxnGuard::new(self, None, epoch))
    }

    fn acquire_checkpointer(&self) -> Result<CheckpointTxnGuard<'_>> {
        self.acquire_checkpoint(CheckpointMode::Passive)
    }
}

impl Drop for WalShm {
    fn drop(&mut self) {
        let _ = self.close(false);
    }
}
