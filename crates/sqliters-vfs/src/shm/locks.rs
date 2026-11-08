//! RAII WAL-index lock guards and per-connection binding lock state machine (`WalLockState`).
//!
//! Enforces SQLite 3.54.1 non-linear WAL lock transitions (`sqlite/src/wal.c`):
//! - Reader snapshot: `SHARED` on `WAL_READ_LOCK(0..=4)` (`bytes 123..=127`).
//! - Writer transaction: `Reading(slot)` -> `EXCLUSIVE WAL_WRITE_LOCK` (`byte 120`), or standalone
//!   `EXCLUSIVE WAL_WRITE_LOCK`.
//! - Checkpoint: `EXCLUSIVE WAL_CKPT_LOCK` (`byte 121`) -> (`FULL`/`RESTART`/`TRUNCATE`) `EXCLUSIVE`
//!   `WAL_WRITE_LOCK` (`byte 120`) -> optional `EXCLUSIVE WAL_READ_LOCK(0)` / `WAL_READ_LOCK(1..=4)`.
//!   `PASSIVE` never waits or invokes a busy handler.
//! - Recovery: held `EXCLUSIVE WAL_WRITE_LOCK` -> `EXCLUSIVE WAL_CKPT_LOCK` + `WAL_RECOVER_LOCK`
//!   (`bytes 121..=122`) WITHOUT locking `WAL_READ_LOCK(0)`, followed by nonblocking per-slot
//!   `WAL_READ_LOCK(1..=4)` read-mark updates that skip `BUSY` slots untouched.
//!
//! Partial lock acquisition failures roll back only newly acquired locks in reverse order. Failed
//! unlock during `Drop` never panics; instead it transitions the connection into `poisoned` state.
#![forbid(unsafe_code)]

use sqliters_types::{ErrorCode, Result, SqliteError};

use super::{
    READMARK_NOT_USED, ShmChunk, WAL_NREADER, WalCkptInfoSnapshot, WalIndexHeaderData, WalShm,
};

/// `WAL_WRITE_LOCK` index (`0` -> byte `120` in `-shm`).
pub const WAL_WRITE_LOCK: u8 = 0;

/// `WAL_CKPT_LOCK` index (`1` -> byte `121` in `-shm`).
pub const WAL_CKPT_LOCK: u8 = 1;

/// `WAL_RECOVER_LOCK` index (`2` -> byte `122` in `-shm`).
pub const WAL_RECOVER_LOCK: u8 = 2;

/// First reader lock index (`WAL_READ_LOCK(0) = 3` -> byte `123`; `0..=4` -> `123..=127`).
pub const WAL_READ_LOCK_BASE: u8 = 3;

/// Total number of WAL lock bytes (`SQLITE_SHM_NLOCK = 8`, bytes `120..128`).
pub const WAL_SHM_NLOCK: u8 = 8;

/// Deadman Switch byte offset in `-shm` (`UNIX_SHM_DMS = WIN_SHM_DMS = 128`).
pub const WAL_SHM_DMS_OFFSET: u64 = 128;

/// Maps reader slot `0..=4` to lock index `3..=7`.
#[inline]
pub const fn wal_read_lock_slot(slot: u8) -> Option<u8> {
    if slot < (WAL_NREADER as u8) {
        Some(WAL_READ_LOCK_BASE + slot)
    } else {
        None
    }
}

/// Maps lock index `0..8` to its physical byte offset `120..128` in `-shm`.
#[inline]
pub const fn wal_lock_byte_offset(lock_idx: u8) -> Option<u64> {
    if lock_idx < WAL_SHM_NLOCK {
        Some(120 + (lock_idx as u64))
    } else {
        None
    }
}

/// Shared vs exclusive mode for an individual WAL lock byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShmLockKind {
    Shared,
    Exclusive,
}

/// SQLite WAL checkpoint modes (`SQLITE_CHECKPOINT_PASSIVE..TRUNCATE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckpointMode {
    Passive,
    Full,
    Restart,
    Truncate,
}

/// High-level protocol mode of a single [`WalShm`] connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WalLockMode {
    Idle,
    Reading { slot: u8 },
    Writing { reader_slot: Option<u8> },
    Checkpointing { mode: CheckpointMode },
    Recovering { reader_slot: Option<u8> },
}

/// Per-connection WAL lock state (`implementation_plan.md` line 119).
///
/// Bits `0..8` of `held_mask` track shared locks on `0..8`; bits `8..16` track exclusive locks
/// on `0..8` (matching `pWal->lockMask` in `sqlite/src/wal.c`).
#[derive(Debug, Clone)]
pub struct WalLockState {
    connection_id: u64,
    held_mask: u16,
    mode: WalLockMode,
    epoch: u64,
    poisoned: bool,
    poison_message: Option<String>,
}

impl WalLockState {
    pub(crate) fn new(connection_id: u64) -> Self {
        Self {
            connection_id,
            held_mask: 0,
            mode: WalLockMode::Idle,
            epoch: 0,
            poisoned: false,
            poison_message: None,
        }
    }

    #[inline]
    pub const fn connection_id(&self) -> u64 {
        self.connection_id
    }

    #[inline]
    pub const fn held_mask(&self) -> u16 {
        self.held_mask
    }

    #[inline]
    pub const fn mode(&self) -> WalLockMode {
        self.mode
    }

    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    #[inline]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    #[inline]
    pub fn poison_message(&self) -> Option<&str> {
        self.poison_message.as_deref()
    }

    #[inline]
    pub const fn shared_bit(lock_idx: u8) -> u16 {
        1u16 << lock_idx
    }

    #[inline]
    pub const fn exclusive_bit(lock_idx: u8) -> u16 {
        1u16 << (WAL_SHM_NLOCK + lock_idx)
    }

    #[inline]
    pub const fn holds_shared(&self, lock_idx: u8) -> bool {
        lock_idx < WAL_SHM_NLOCK && (self.held_mask & Self::shared_bit(lock_idx)) != 0
    }

    #[inline]
    pub const fn holds_exclusive(&self, lock_idx: u8) -> bool {
        lock_idx < WAL_SHM_NLOCK && (self.held_mask & Self::exclusive_bit(lock_idx)) != 0
    }

    #[inline]
    pub(crate) fn ensure_healthy(&self) -> Result<()> {
        if self.poisoned {
            let msg = self
                .poison_message
                .as_deref()
                .unwrap_or("WAL-index connection lock state is poisoned");
            return Err(SqliteError::new(ErrorCode::IoErr, msg));
        }
        Ok(())
    }

    pub(crate) fn record_lock(&mut self, lock_idx: u8, kind: ShmLockKind) {
        let bit = match kind {
            ShmLockKind::Shared => Self::shared_bit(lock_idx),
            ShmLockKind::Exclusive => Self::exclusive_bit(lock_idx),
        };
        self.held_mask |= bit;
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub(crate) fn record_unlock(&mut self, lock_idx: u8, kind: ShmLockKind) {
        let bit = match kind {
            ShmLockKind::Shared => Self::shared_bit(lock_idx),
            ShmLockKind::Exclusive => Self::exclusive_bit(lock_idx),
        };
        self.held_mask &= !bit;
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub(crate) fn set_mode(&mut self, mode: WalLockMode) {
        self.mode = mode;
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub(crate) fn mark_poisoned(&mut self, reason: impl Into<String>) {
        self.poisoned = true;
        if self.poison_message.is_none() {
            self.poison_message = Some(reason.into());
        }
        self.epoch = self.epoch.wrapping_add(1);
    }
}

/// RAII reader snapshot guard holding `SHARED` on `WAL_READ_LOCK(slot)` (`slot` in `0..=4`).
#[derive(Debug)]
pub struct WalReadGuard<'a> {
    shm: &'a WalShm,
    slot: u8,
    epoch_at_entry: u64,
    active: bool,
}

impl<'a> WalReadGuard<'a> {
    pub(crate) fn new(shm: &'a WalShm, slot: u8, epoch_at_entry: u64) -> Self {
        Self {
            shm,
            slot,
            epoch_at_entry,
            active: true,
        }
    }

    /// Held reader slot index (`0..=4`).
    #[inline]
    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Lock-state epoch observed when this guard entered `Reading` mode.
    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch_at_entry
    }

    /// Reads `WalCkptInfo.aReadMark[slot]` (`100 + slot * 4`) atomically.
    pub fn read_mark(&self) -> Result<u32> {
        self.shm.read_slot_mark_internal(self.slot)
    }

    /// Reads an owned snapshot of `WalCkptInfo` (`nBackfill`, `aReadMark[0..5]`, `nBackfillAttempted`).
    pub fn read_ckpt_info(&self) -> Result<WalCkptInfoSnapshot> {
        self.shm.read_ckpt_info_internal()
    }

    /// Reads and verifies both copies + checksum of `WalIndexHdr`.
    pub fn read_header_verified(&self) -> Result<WalIndexHeaderData> {
        self.shm.read_header_verified_internal()
    }

    /// Searches WAL-index hash tables for the newest frame of `pgno` in `[min_frame, max_frame]`.
    ///
    /// Following `sqlite3WalFindFrame` (`wal.c:3626`), returns `Ok(None)` immediately when
    /// holding `WAL_READ_LOCK(0)` because slot-0 readers read exclusively from `.db`.
    pub fn lookup_page_in_range(
        &self,
        pgno: u32,
        min_frame: u32,
        max_frame: u32,
    ) -> Result<Option<u32>> {
        if self.slot == 0 {
            return Ok(None);
        }
        self.shm.lookup_page_internal(pgno, min_frame, max_frame)
    }

    /// Attempts to upgrade this reader snapshot into a [`WriterTxnGuard`] by acquiring
    /// `EXCLUSIVE WAL_WRITE_LOCK` (`byte 120`).
    ///
    /// On failure (such as `SQLITE_BUSY`), returns `Err((self, err))` with this reader guard
    /// still intact and holding its `SHARED WAL_READ_LOCK(slot)`.
    pub fn try_upgrade_to_writer(
        self,
    ) -> std::result::Result<WriterTxnGuard<'a>, (Self, SqliteError)> {
        match self.shm.upgrade_reader_to_writer_internal(self.slot) {
            Ok(epoch) => {
                let shm = self.shm;
                let slot = self.slot;
                // Disarm `self` without unlocking `WAL_READ_LOCK(slot)` since ownership moved to `WriterTxnGuard`.
                let mut this = self;
                this.active = false;
                Ok(WriterTxnGuard::new(shm, Some(slot), epoch))
            }
            Err(err) => Err((self, err)),
        }
    }

    /// Upgrades to a [`WriterTxnGuard`], releasing the reader lock if upgrading fails.
    pub fn upgrade_to_writer(self) -> Result<WriterTxnGuard<'a>> {
        match self.try_upgrade_to_writer() {
            Ok(writer) => Ok(writer),
            Err((guard, err)) => {
                let _ = guard.release();
                Err(err)
            }
        }
    }

    /// Explicitly releases `SHARED WAL_READ_LOCK(slot)` and surfaces any unlock error.
    pub fn release(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.shm.release_reader_internal(self.slot, false)
    }
}

impl Drop for WalReadGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            let _ = self.shm.release_reader_internal(self.slot, true);
        }
    }
}

/// RAII writer transaction guard holding `EXCLUSIVE WAL_WRITE_LOCK` (`byte 120`) and an optional
/// `SHARED WAL_READ_LOCK(slot)`.
#[derive(Debug)]
pub struct WriterTxnGuard<'a> {
    shm: &'a WalShm,
    reader_slot: Option<u8>,
    epoch_at_entry: u64,
    active: bool,
}

impl<'a> WriterTxnGuard<'a> {
    pub(crate) fn new(shm: &'a WalShm, reader_slot: Option<u8>, epoch_at_entry: u64) -> Self {
        Self {
            shm,
            reader_slot,
            epoch_at_entry,
            active: true,
        }
    }

    /// Reader slot co-held by this writer transaction, if upgraded from [`WalReadGuard`].
    #[inline]
    pub const fn reader_slot(&self) -> Option<u8> {
        self.reader_slot
    }

    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch_at_entry
    }

    /// Ensures logical 32 KiB WAL-index `region` is allocated and mapped, extending `-shm` safely
    /// under `WAL_WRITE_LOCK`.
    pub fn ensure_region(&mut self, region: u32) -> Result<ShmChunk> {
        self.shm.ensure_region_writable_internal(region)
    }

    /// Reads and verifies `WalIndexHdr` (`copy1 -> shm_barrier -> copy2` + checksum).
    pub fn read_header_verified(&self) -> Result<WalIndexHeaderData> {
        self.shm.read_header_verified_internal()
    }

    /// Reads an owned snapshot of `WalCkptInfo`.
    pub fn read_ckpt_info(&self) -> Result<WalCkptInfoSnapshot> {
        self.shm.read_ckpt_info_internal()
    }

    /// Appends `(frame_no -> pgno)` into the WAL-index `aPgno` + `aHash` tables (`walIndexAppend`),
    /// automatically cleaning uncommitted remnants beyond `committed_mx_frame` if present.
    pub fn record_frame(
        &mut self,
        frame_no: u32,
        pgno: u32,
        committed_mx_frame: u32,
    ) -> Result<()> {
        self.shm
            .record_frame_internal(frame_no, pgno, committed_mx_frame)
    }

    /// Searches WAL-index hash tables for the latest frame of `pgno` in `[min_frame, max_frame]`.
    pub fn lookup_page_in_range(
        &self,
        pgno: u32,
        min_frame: u32,
        max_frame: u32,
    ) -> Result<Option<u32>> {
        self.shm.lookup_page_internal(pgno, min_frame, max_frame)
    }

    /// Removes hash-table slots and `aPgno` entries with frame numbers strictly above `max_frame`
    /// (`walCleanupHash` for rollback / savepoint rollback).
    pub fn truncate_frames_above(&mut self, max_frame: u32) -> Result<()> {
        self.shm.truncate_frames_above_internal(max_frame)
    }

    /// Publishes `header` following `walIndexWriteHdr` (`wal.c:974-986`):
    /// sets `is_init = 1` and `version = 3_007_000`, computes checksum over `0..40`, writes copy 2
    /// (`48..96`), executes `shm_barrier()`, then writes copy 1 (`0..48`).
    pub fn publish_header(&mut self, header: &WalIndexHeaderData) -> Result<WalIndexHeaderData> {
        self.shm.publish_header_internal(header)
    }

    /// Resets the WAL-index header for log restart (`walRestartLog` + `walRestartHdr`, `wal.c:2234`):
    /// acquires `EXCLUSIVE WAL_READ_LOCK(1..=4)` to verify no active WAL readers exist (temporarily
    /// releasing this connection's own `reader_slot` onto `READ(0)` if `reader_slot` was in `1..=4`),
    /// increments big-endian `salt[0]`, sets `salt[1] = new_salt1` and `mx_frame = 0`, publishes
    /// the new header, resets `nBackfill = 0`, `nBackfillAttempted = 0`, `aReadMark[1] = 0`, and
    /// `aReadMark[2..=4] = READMARK_NOT_USED`, then releases `WAL_READ_LOCK(1..=4)`.
    pub fn restart_header(
        &mut self,
        base_header: &WalIndexHeaderData,
        new_salt1: u32,
    ) -> Result<WalIndexHeaderData> {
        self.shm
            .restart_header_under_write_internal(&mut self.reader_slot, base_header, new_salt1)
    }

    /// Enters WAL recovery (`walIndexRecover`, `wal.c:1422`): while continuing to hold
    /// `WAL_WRITE_LOCK`, acquires `EXCLUSIVE WAL_CKPT_LOCK` + `WAL_RECOVER_LOCK` WITHOUT locking
    /// `WAL_READ_LOCK(0)`.
    ///
    /// On partial failure (e.g. `CKPT` acquired, `RECOVER` returns `BUSY`), rolls back `CKPT` in
    /// reverse order and returns `Err((self, err))` with `WAL_WRITE_LOCK` still held.
    pub fn enter_recovery(self) -> std::result::Result<RecoveryTxnGuard<'a>, (Self, SqliteError)> {
        match self
            .shm
            .enter_recovery_from_writer_internal(self.reader_slot)
        {
            Ok(epoch) => {
                let shm = self.shm;
                let reader_slot = self.reader_slot;
                let mut this = self;
                this.active = false;
                Ok(RecoveryTxnGuard::new(shm, reader_slot, epoch))
            }
            Err(err) => Err((self, err)),
        }
    }

    /// Releases `WAL_WRITE_LOCK` while retaining `WalReadGuard` if this writer upgraded from a reader.
    pub fn downgrade_to_reader(mut self) -> Result<Option<WalReadGuard<'a>>> {
        if !self.active {
            return Ok(None);
        }
        self.active = false;
        let epoch = self
            .shm
            .downgrade_writer_to_reader_internal(self.reader_slot)?;
        Ok(self
            .reader_slot
            .map(|slot| WalReadGuard::new(self.shm, slot, epoch)))
    }

    /// Explicitly releases `WAL_WRITE_LOCK` and any co-held `reader_slot` in reverse order.
    pub fn release(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.shm.release_writer_internal(self.reader_slot, false)
    }
}

impl Drop for WriterTxnGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            let _ = self.shm.release_writer_internal(self.reader_slot, true);
        }
    }
}

/// RAII checkpoint transaction guard (`CKPT -> WRITE -> READ(0) / READ(1..=4)`).
#[derive(Debug)]
pub struct CheckpointTxnGuard<'a> {
    shm: &'a WalShm,
    mode: CheckpointMode,
    acquired_stack: Vec<u8>,
    epoch_at_entry: u64,
    active: bool,
}

impl<'a> CheckpointTxnGuard<'a> {
    pub(crate) fn new(
        shm: &'a WalShm,
        mode: CheckpointMode,
        acquired_stack: Vec<u8>,
        epoch_at_entry: u64,
    ) -> Self {
        Self {
            shm,
            mode,
            acquired_stack,
            epoch_at_entry,
            active: true,
        }
    }

    #[inline]
    pub const fn mode(&self) -> CheckpointMode {
        self.mode
    }

    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch_at_entry
    }

    #[inline]
    pub fn holds_write_lock(&self) -> bool {
        self.acquired_stack.contains(&WAL_WRITE_LOCK)
    }

    #[inline]
    pub fn holds_read_lock_exclusive(&self, slot: u8) -> bool {
        wal_read_lock_slot(slot).is_some_and(|idx| self.acquired_stack.contains(&idx))
    }

    /// Reads and verifies `WalIndexHdr`.
    pub fn read_header_verified(&self) -> Result<WalIndexHeaderData> {
        self.shm.read_header_verified_internal()
    }

    /// Reads an owned snapshot of `WalCkptInfo`.
    pub fn read_ckpt_info(&self) -> Result<WalCkptInfoSnapshot> {
        self.shm.read_ckpt_info_internal()
    }

    /// Acquires `EXCLUSIVE WAL_READ_LOCK(slot)` (`slot` in `0..=4`) during `FULL`/`RESTART`/`TRUNCATE`
    /// checkpoints, strictly enforcing that `WAL_WRITE_LOCK` is already held (`CKPT -> WRITE -> READ`).
    pub fn lock_read_slot_exclusive(&mut self, slot: u8) -> Result<()> {
        if self.mode == CheckpointMode::Passive || !self.holds_write_lock() {
            return Err(SqliteError::misuse(
                "CheckpointTxnGuard must hold CKPT and WRITE before locking READ slots exclusively",
            ));
        }
        let lock_idx = wal_read_lock_slot(slot)
            .ok_or_else(|| SqliteError::misuse("invalid WAL reader slot"))?;
        if self.acquired_stack.contains(&lock_idx) {
            return Ok(());
        }
        self.shm.lock_additional_exclusive_internal(lock_idx)?;
        self.acquired_stack.push(lock_idx);
        Ok(())
    }

    /// Computes `mxSafeFrame` across `aReadMark[1..=4]` (`walCheckpoint`, `wal.c:2315-2333`):
    /// whenever `mx_safe_frame > aReadMark[i]`, attempts a nonblocking `EXCLUSIVE WAL_READ_LOCK(i)`;
    /// if acquired, updates `aReadMark[i]` to `mx_safe_frame` (for `i == 1`) or `READMARK_NOT_USED`
    /// (for `i in 2..=4`) and unlocks `WAL_READ_LOCK(i)`; if `BUSY`, clamps `mx_safe_frame = y`.
    pub fn claim_safe_backfill_frame(&mut self, mx_frame: u32) -> Result<u32> {
        self.shm.claim_safe_backfill_frame_internal(mx_frame)
    }

    /// Updates `WalCkptInfo.nBackfillAttempted` (`@128..132`) prior to backfilling pages into `.db`.
    pub fn set_backfill_attempted(&mut self, frame: u32) -> Result<()> {
        self.shm.write_backfill_attempted_internal(frame)
    }

    /// Publishes `WalCkptInfo.nBackfill` (`@96..100`) after backfilled pages have been written.
    pub fn set_backfill_completed(&mut self, frame: u32) -> Result<()> {
        self.shm.write_backfill_completed_internal(frame)
    }

    /// Resets header and checkpoint fields during `RESTART`/`TRUNCATE` once `CKPT`, `WRITE`, and
    /// `READ(1..=4)` exclusive locks are held.
    pub fn restart_header_for_checkpoint(
        &mut self,
        base_header: &WalIndexHeaderData,
        new_salt1: u32,
    ) -> Result<WalIndexHeaderData> {
        if !self.holds_write_lock() {
            return Err(SqliteError::misuse(
                "checkpoint header restart requires WAL_WRITE_LOCK",
            ));
        }
        for slot in 1..(WAL_NREADER as u8) {
            if !self.holds_read_lock_exclusive(slot) {
                return Err(SqliteError::misuse(
                    "checkpoint header restart requires exclusive WAL_READ_LOCK(1..=4)",
                ));
            }
        }
        self.shm
            .write_restarted_header_and_marks_internal(base_header, new_salt1)
    }

    /// Explicitly releases all held checkpoint locks in strict reverse acquisition order.
    pub fn release(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let stack = std::mem::take(&mut self.acquired_stack);
        self.shm.release_exclusive_stack_internal(&stack, false)
    }
}

impl Drop for CheckpointTxnGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            let stack = std::mem::take(&mut self.acquired_stack);
            let _ = self.shm.release_exclusive_stack_internal(&stack, true);
        }
    }
}

/// RAII WAL recovery transaction guard (`WRITE -> CKPT + RECOVER`, NEVER locking `READ(0)`).
#[derive(Debug)]
pub struct RecoveryTxnGuard<'a> {
    shm: &'a WalShm,
    reader_slot: Option<u8>,
    owns_write_lock: bool,
    epoch_at_entry: u64,
    active: bool,
}

impl<'a> RecoveryTxnGuard<'a> {
    pub(crate) fn new(shm: &'a WalShm, reader_slot: Option<u8>, epoch_at_entry: u64) -> Self {
        Self {
            shm,
            reader_slot,
            owns_write_lock: false,
            epoch_at_entry,
            active: true,
        }
    }

    pub(crate) fn new_standalone(shm: &'a WalShm, epoch_at_entry: u64) -> Self {
        Self {
            shm,
            reader_slot: None,
            owns_write_lock: true,
            epoch_at_entry,
            active: true,
        }
    }

    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch_at_entry
    }

    /// Ensures logical 32 KiB WAL-index `region` is allocated and mapped during WAL recovery.
    pub fn ensure_region(&mut self, region: u32) -> Result<ShmChunk> {
        self.shm.ensure_region_writable_internal(region)
    }

    /// Appends `(frame_no -> pgno)` into the WAL-index hash table during recovery.
    pub fn record_frame(
        &mut self,
        frame_no: u32,
        pgno: u32,
        committed_mx_frame: u32,
    ) -> Result<()> {
        self.shm
            .record_frame_internal(frame_no, pgno, committed_mx_frame)
    }

    /// Truncates hash entries strictly above `max_frame` (e.g. if trailing frames in `.wal` had
    /// invalid checksums or lacked a commit record!).
    pub fn truncate_frames_above(&mut self, max_frame: u32) -> Result<()> {
        self.shm.truncate_frames_above_internal(max_frame)
    }

    /// Searches WAL-index hash tables during recovery verification.
    pub fn lookup_page_in_range(
        &self,
        pgno: u32,
        min_frame: u32,
        max_frame: u32,
    ) -> Result<Option<u32>> {
        self.shm.lookup_page_internal(pgno, min_frame, max_frame)
    }

    /// Publishes the recovered `WalIndexHeaderData` and resets `WalCkptInfo` exactly matching
    /// `walIndexRecover` (`sqlite/src/wal.c:1604-1627`):
    /// 1. Publishes `header` (`copy2 -> shm_barrier -> copy1`).
    /// 2. Writes `nBackfill = 0`, `nBackfillAttempted = mx_frame`, and `aReadMark[0] = 0` under
    ///    `WRITE + CKPT + RECOVER` WITHOUT acquiring `WAL_READ_LOCK(0)`.
    /// 3. Attempts nonblocking `EXCLUSIVE WAL_READ_LOCK(slot)` for each `slot` in `1..=4`:
    ///    - If acquired: sets `aReadMark[1] = mx_frame` iff `mx_frame > 0` else `READMARK_NOT_USED`,
    ///      sets `aReadMark[2..=4] = READMARK_NOT_USED`, and immediately releases `WAL_READ_LOCK(slot)`.
    ///    - If `SQLITE_BUSY`: leaves `aReadMark[slot]` untouched and continues!
    ///
    /// Returns `(published_header, slot_updated_flags)`.
    pub fn finish_recovery_header_and_marks(
        &mut self,
        header: &WalIndexHeaderData,
    ) -> Result<(WalIndexHeaderData, [bool; WAL_NREADER])> {
        let _ = READMARK_NOT_USED;
        self.shm.finish_recovery_header_and_marks_internal(header)
    }

    /// Releases `WAL_RECOVER_LOCK` then `WAL_CKPT_LOCK` in reverse order and returns a
    /// [`WriterTxnGuard`] still holding `WAL_WRITE_LOCK`.
    pub fn finish_to_writer(mut self) -> Result<WriterTxnGuard<'a>> {
        self.active = false;
        let epoch = self
            .shm
            .exit_recovery_to_writer_internal(self.reader_slot)?;
        Ok(WriterTxnGuard::new(self.shm, self.reader_slot, epoch))
    }

    /// Explicitly releases `RECOVER`, `CKPT`, `WRITE`, and any `reader_slot` in reverse order.
    pub fn release(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        let _ = self.owns_write_lock;
        self.shm
            .release_recovery_all_internal(self.reader_slot, false)
    }
}

impl Drop for RecoveryTxnGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            let _ = self
                .shm
                .release_recovery_all_internal(self.reader_slot, true);
        }
    }
}
