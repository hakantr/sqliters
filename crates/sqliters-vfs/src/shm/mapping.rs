//! Two-level shared-memory mapping lifetimes (`MapView` + `ShmChunk`) in 100% Safe Rust.
//!
//! - [`MapView`] represents one physical OS mapping (`mmap` / `MapViewOfFile`, aligned to OS page
//!   size or Windows `dwAllocationGranularity`) or an in-memory atomic region (`MemoryVfs`).
//! - [`ShmChunk`] represents one logical `32,768`-byte WAL-index region holding an `Arc<MapView>`
//!   plus a validated `subrange_offset`. Adjacent 32 KiB chunks share a single `Arc<MapView>` when
//!   mapping granularity exceeds 32 KiB (e.g. 64 KiB OS pages), existing chunks never relocate when
//!   new regions are appended, and `munmap`/`UnmapViewOfFile` executes only when the last
//!   `Arc<MapView>` drops.
#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering, fence};

use sqliters_types::{Result, SqliteError, extended};

#[cfg(unix)]
use crate::sys::unix as sys_os;
#[cfg(windows)]
use crate::sys::windows as sys_os;

/// Logical WAL-index region size in bytes (`WALINDEX_PGSZ = 32768` in `sqlite/src/wal.c:659`).
pub const SHM_CHUNK_SIZE: usize = 32_768;

/// Byte offset of `WalCkptInfo.aLock[0..8]` in region 0 (`WALINDEX_LOCK_OFFSET = 120`).
pub const WAL_LOCK_OFFSET: usize = 120;

/// Byte length of `WalCkptInfo.aLock[0..8]` (`SQLITE_SHM_NLOCK = 8`, `120..128`).
pub const WAL_LOCK_LEN: usize = 8;

static NEXT_MEM_VIEW_ID: AtomicUsize = AtomicUsize::new(1);

/// Computes `(file_byte_offset, byte_length)` for `chunk_count` contiguous 32 KiB regions starting
/// at `start_region` using checked multiplication and addition (`implementation_plan.md` line 99).
pub fn checked_region_byte_range(start_region: u32, chunk_count: usize) -> Result<(u64, usize)> {
    if chunk_count == 0 {
        return Err(SqliteError::misuse("chunk_count must be non-zero"));
    }
    let start_idx = usize::try_from(start_region).map_err(|_| {
        SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "region index overflow")
    })?;
    let start_bytes = start_idx.checked_mul(SHM_CHUNK_SIZE).ok_or_else(|| {
        SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "region offset overflow")
    })?;
    let len_bytes = chunk_count.checked_mul(SHM_CHUNK_SIZE).ok_or_else(|| {
        SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "region length overflow")
    })?;
    let _end_bytes = start_bytes.checked_add(len_bytes).ok_or_else(|| {
        SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "region end overflow")
    })?;
    let off_u64 = u64::try_from(start_bytes).map_err(|_| {
        SqliteError::with_extended(extended::SQLITE_IOERR_SHMMAP, "region u64 offset overflow")
    })?;
    Ok((off_u64, len_bytes))
}

/// Shared atomic buffer backing an in-memory `MapView` (`MemoryVfs` and private-heap WAL-index).
pub(crate) struct MemRegionWords {
    id: usize,
    words: Box<[AtomicU32]>,
}

impl MemRegionWords {
    pub(crate) fn new_zeroed(byte_len: usize) -> Result<Self> {
        if byte_len == 0 || !byte_len.is_multiple_of(4) {
            return Err(SqliteError::misuse(
                "in-memory WAL-index view byte_len must be a non-zero multiple of 4",
            ));
        }
        let word_count = byte_len / 4;
        let mut vec = Vec::with_capacity(word_count);
        vec.resize_with(word_count, || AtomicU32::new(0));
        Ok(Self {
            id: NEXT_MEM_VIEW_ID.fetch_add(1, Ordering::Relaxed),
            words: vec.into_boxed_slice(),
        })
    }

    #[inline]
    pub(crate) fn byte_len(&self) -> usize {
        self.words.len() * 4
    }
}

pub(crate) struct MemMapView {
    storage: Arc<MemRegionWords>,
    writable: bool,
}

impl MemMapView {
    pub(crate) fn new(storage: Arc<MemRegionWords>, writable: bool) -> Self {
        Self { storage, writable }
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
                "attempt to write to a read-only in-memory WAL-index mapping",
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
        if rel_end > SHM_CHUNK_SIZE {
            return Err(SqliteError::misuse(
                "WAL-index relative offset exceeds 32 KiB region",
            ));
        }
        if region_index == 0
            && rel_offset < (WAL_LOCK_OFFSET + WAL_LOCK_LEN)
            && rel_end > WAL_LOCK_OFFSET
        {
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
        if abs_end > self.storage.byte_len() {
            return Err(SqliteError::misuse(
                "in-memory WAL-index access out of MapView bounds",
            ));
        }
        Ok(abs_offset)
    }

    #[inline]
    fn load_u32(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        ordering: Ordering,
    ) -> Result<u32> {
        let abs = self.validate_offset(subrange_offset, region_index, rel_offset, 4, false)?;
        Ok(self.storage.words[abs / 4].load(ordering))
    }

    #[inline]
    fn store_u32(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        value: u32,
        ordering: Ordering,
    ) -> Result<()> {
        let abs = self.validate_offset(subrange_offset, region_index, rel_offset, 4, true)?;
        self.storage.words[abs / 4].store(value, ordering);
        Ok(())
    }

    #[inline]
    fn load_u16(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        ordering: Ordering,
    ) -> Result<u16> {
        let abs = self.validate_offset(subrange_offset, region_index, rel_offset, 2, false)?;
        let word = self.storage.words[abs / 4].load(ordering);
        let bytes = word.to_ne_bytes();
        let lo = abs & 2;
        Ok(u16::from_ne_bytes([bytes[lo], bytes[lo + 1]]))
    }

    #[inline]
    fn store_u16(
        &self,
        subrange_offset: usize,
        region_index: u32,
        rel_offset: usize,
        value: u16,
        ordering: Ordering,
    ) -> Result<()> {
        let abs = self.validate_offset(subrange_offset, region_index, rel_offset, 2, true)?;
        let lo = abs & 2;
        let hb = value.to_ne_bytes();
        let (set_order, fetch_order) = match ordering {
            Ordering::Release => (Ordering::Release, Ordering::Relaxed),
            Ordering::AcqRel => (Ordering::AcqRel, Ordering::Acquire),
            Ordering::SeqCst => (Ordering::SeqCst, Ordering::SeqCst),
            _ => (Ordering::Relaxed, Ordering::Relaxed),
        };
        let word = &self.storage.words[abs / 4];
        let mut cur = word.load(fetch_order);
        loop {
            let mut b = cur.to_ne_bytes();
            b[lo] = hb[0];
            b[lo + 1] = hb[1];
            let next = u32::from_ne_bytes(b);
            match word.compare_exchange_weak(cur, next, set_order, fetch_order) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
        Ok(())
    }

    fn zero_aligned_u32_range(
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

pub(crate) enum MapViewInner {
    Os(sys_os::SysMapView),
    Memory(MemMapView),
}

/// Physical OS or in-memory mapping view over one or more 32 KiB WAL-index regions.
pub struct MapView {
    inner: MapViewInner,
}

impl std::fmt::Debug for MapView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MapView")
            .field("len", &self.len())
            .field("writable", &self.is_writable())
            .field("view_id", &self.debug_view_id())
            .finish()
    }
}

impl MapView {
    pub(crate) fn from_os(view: sys_os::SysMapView) -> Self {
        Self {
            inner: MapViewInner::Os(view),
        }
    }

    pub(crate) fn from_memory(storage: Arc<MemRegionWords>, writable: bool) -> Self {
        Self {
            inner: MapViewInner::Memory(MemMapView::new(storage, writable)),
        }
    }

    /// Total mapped length in bytes (always a multiple of [`SHM_CHUNK_SIZE`] or OS granularity).
    #[inline]
    pub fn len(&self) -> usize {
        match &self.inner {
            MapViewInner::Os(v) => v.len(),
            MapViewInner::Memory(m) => m.storage.byte_len(),
        }
    }

    /// Returns `true` if `len() == 0` (never true for valid `MapView`s).
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns whether this mapping permits writes.
    #[inline]
    pub fn is_writable(&self) -> bool {
        match &self.inner {
            MapViewInner::Os(v) => v.is_writable(),
            MapViewInner::Memory(m) => m.writable,
        }
    }

    /// Opaque view identity used in tests to verify chunk grouping and non-relocation.
    #[inline]
    pub fn debug_view_id(&self) -> usize {
        match &self.inner {
            MapViewInner::Os(v) => v.debug_view_id(),
            MapViewInner::Memory(m) => m.storage.id,
        }
    }
}

/// Logical `32,768`-byte WAL-index region backed by a reference-counted [`MapView`].
#[derive(Clone, Debug)]
pub struct ShmChunk {
    view: Arc<MapView>,
    subrange_offset: usize,
    region_index: u32,
}

impl ShmChunk {
    /// Creates a validated 32 KiB [`ShmChunk`] referencing `[subrange_offset..subrange_offset + 32768)`
    /// inside `view`.
    pub fn try_new(view: Arc<MapView>, subrange_offset: usize, region_index: u32) -> Result<Self> {
        let _ = checked_region_byte_range(region_index, 1)?;
        if !subrange_offset.is_multiple_of(4) {
            return Err(SqliteError::misuse(
                "ShmChunk subrange_offset must be 4-byte aligned",
            ));
        }
        let end = subrange_offset.checked_add(SHM_CHUNK_SIZE).ok_or_else(|| {
            SqliteError::with_extended(
                extended::SQLITE_IOERR_SHMMAP,
                "ShmChunk subrange_offset overflow",
            )
        })?;
        if end > view.len() {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_SHMMAP,
                "ShmChunk subrange exceeds MapView length",
            ));
        }
        Ok(Self {
            view,
            subrange_offset,
            region_index,
        })
    }

    /// 0-based logical WAL-index region index.
    #[inline]
    pub fn region_index(&self) -> u32 {
        self.region_index
    }

    /// Validated byte offset of this 32 KiB chunk within its parent [`MapView`].
    #[inline]
    pub fn subrange_offset(&self) -> usize {
        self.subrange_offset
    }

    /// Logical byte size of this chunk (always `32,768`).
    #[inline]
    pub const fn byte_len(&self) -> usize {
        SHM_CHUNK_SIZE
    }

    /// Reference-counted parent [`MapView`] keeping the OS mapping alive.
    #[inline]
    pub fn view(&self) -> &Arc<MapView> {
        &self.view
    }

    /// Returns `true` iff `self` and `other` share the exact same [`MapView`] instance.
    #[inline]
    pub fn shares_view_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.view, &other.view)
    }

    #[inline]
    pub(crate) fn load_u32(&self, rel_offset: usize, ordering: Ordering) -> Result<u32> {
        match &self.view.inner {
            MapViewInner::Os(v) => v.load_u32(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                ordering,
            ),
            MapViewInner::Memory(m) => m.load_u32(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                ordering,
            ),
        }
    }

    #[inline]
    pub(crate) fn store_u32(
        &self,
        rel_offset: usize,
        value: u32,
        ordering: Ordering,
    ) -> Result<()> {
        match &self.view.inner {
            MapViewInner::Os(v) => v.store_u32(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                value,
                ordering,
            ),
            MapViewInner::Memory(m) => m.store_u32(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                value,
                ordering,
            ),
        }
    }

    #[inline]
    pub(crate) fn load_u16(&self, rel_offset: usize, ordering: Ordering) -> Result<u16> {
        match &self.view.inner {
            MapViewInner::Os(v) => v.load_u16(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                ordering,
            ),
            MapViewInner::Memory(m) => m.load_u16(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                ordering,
            ),
        }
    }

    #[inline]
    pub(crate) fn store_u16(
        &self,
        rel_offset: usize,
        value: u16,
        ordering: Ordering,
    ) -> Result<()> {
        match &self.view.inner {
            MapViewInner::Os(v) => v.store_u16(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                value,
                ordering,
            ),
            MapViewInner::Memory(m) => m.store_u16(
                self.subrange_offset,
                self.region_index,
                rel_offset,
                value,
                ordering,
            ),
        }
    }

    pub(crate) fn zero_aligned_u32_range(&self, start_rel: usize, end_rel: usize) -> Result<()> {
        match &self.view.inner {
            MapViewInner::Os(v) => v.zero_aligned_u32_range(
                self.subrange_offset,
                self.region_index,
                start_rel,
                end_rel,
            ),
            MapViewInner::Memory(m) => m.zero_aligned_u32_range(
                self.subrange_offset,
                self.region_index,
                start_rel,
                end_rel,
            ),
        }
    }
}

/// Executes `xShmBarrier` across OS and in-memory backends.
#[inline]
pub fn shm_barrier() {
    #[cfg(any(unix, windows))]
    {
        sys_os::shm_barrier();
    }
    #[cfg(not(any(unix, windows)))]
    {
        fence(Ordering::SeqCst);
    }
    let _ = fence;
}
