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

use std::sync::Arc;

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

    /// Reads and verifies `(WalIndexHdr, WalCkptInfo)` if region 0 is mapped and valid, returning
    /// `Ok(None)` when region 0 is unallocated or header copies/checksum are uninitialized/mismatch,
    /// and propagating `ErrorCode::CantOpen` on unsupported format version.
    pub fn try_read_header_and_ckpt_info(
        &self,
    ) -> Result<Option<(WalIndexHeaderData, WalCkptInfoSnapshot)>> {
        self.shm.try_read_header_and_ckpt_info_internal()
    }

    /// Ensures region 0 is allocated (`extend = true`) while `WAL_WRITE_LOCK` is held and attempts
    /// to read `(WalIndexHdr, WalCkptInfo)`.
    pub fn extend_and_try_read_header(
        &self,
    ) -> Result<Option<(WalIndexHeaderData, WalCkptInfoSnapshot)>> {
        let _ = self.shm.ensure_region_writable_internal(0)?;
        self.shm.try_read_header_and_ckpt_info_internal()
    }

    /// Temporarily acquires `EXCLUSIVE WAL_WRITE_LOCK` during `Passive` checkpoint header recovery
    /// (`walIndexReadHdr`, `wal.c:2796`).
    pub fn acquire_temp_write_lock(&mut self) -> Result<()> {
        if self.holds_write_lock() {
            return Ok(());
        }
        self.shm
            .lock_additional_exclusive_internal(WAL_WRITE_LOCK)?;
        self.acquired_stack.push(WAL_WRITE_LOCK);
        Ok(())
    }

    /// Releases a temporary `WAL_WRITE_LOCK` acquired via [`Self::acquire_temp_write_lock`].
    pub fn release_temp_write_lock(&mut self) -> Result<()> {
        if self.acquired_stack.last() != Some(&WAL_WRITE_LOCK)
            || self.mode != CheckpointMode::Passive
        {
            return Err(SqliteError::misuse(
                "release_temp_write_lock is only valid for temporary WRITE on Passive checkpoint",
            ));
        }
        self.acquired_stack.pop();
        self.shm.unlock_single_exclusive_internal(WAL_WRITE_LOCK)
    }

    /// Enters `walIndexRecover` from an active checkpoint that already holds `WAL_CKPT_LOCK` and
    /// `WAL_WRITE_LOCK`, acquiring ONLY `WAL_RECOVER_LOCK` (`wal.c:1436`, never `WAL_READ_LOCK(0)`).
    pub fn begin_recovery_under_checkpoint(&mut self) -> Result<CkptRecoveryGuard<'_, 'a>> {
        if !self.holds_write_lock() {
            return Err(SqliteError::misuse(
                "checkpoint recovery requires WAL_CKPT_LOCK and WAL_WRITE_LOCK",
            ));
        }
        self.shm
            .lock_additional_exclusive_internal(WAL_RECOVER_LOCK)?;
        Ok(CkptRecoveryGuard {
            ckpt: self,
            active: true,
        })
    }

    /// Computes `mxSafeFrame` across `aReadMark[1..=4]` (`walCheckpoint`, `wal.c:2315-2333`),
    /// invoking `busy_handler` when `Some` and disabling further waits after the first `Busy` slot.
    pub fn claim_safe_backfill_frame_with_busy(
        &mut self,
        mx_frame: u32,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<u32> {
        self.shm
            .claim_safe_backfill_frame_with_busy_internal(mx_frame, busy_handler)
    }

    /// Acquires `EXCLUSIVE WAL_READ_LOCK(0)` prior to backfilling pages into `.db` (`wal.c:2342`),
    /// valid in both `Passive` (`busy_handler = None`) and `Full`/`Restart`/`Truncate`.
    pub fn lock_read0_for_backfill(
        &mut self,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<()> {
        let lock_idx = wal_read_lock_slot(0).unwrap();
        if self.acquired_stack.contains(&lock_idx) {
            return Ok(());
        }
        self.shm
            .lock_additional_exclusive_with_busy_internal(lock_idx, busy_handler)?;
        self.acquired_stack.push(lock_idx);
        Ok(())
    }

    /// Releases `EXCLUSIVE WAL_READ_LOCK(0)` after backfill (`wal.c:2426`).
    pub fn unlock_read0_after_backfill(&mut self) -> Result<()> {
        let lock_idx = wal_read_lock_slot(0).unwrap();
        let Some(pos) = self.acquired_stack.iter().rposition(|&x| x == lock_idx) else {
            return Ok(());
        };
        self.acquired_stack.remove(pos);
        self.shm.unlock_single_exclusive_internal(lock_idx)
    }

    /// Reads the live 8-byte `aSalt` (`[u32; 2]`, decoded from big-endian `@32..40` of copy 1) while
    /// `WAL_READ_LOCK(0)` is held (`wal.c:2354` March 2026 WAL-reset salt guard).
    pub fn read_live_header_salt(&self) -> Result<[u32; 2]> {
        self.shm.read_live_header_salt_internal()
    }

    /// Reads `(frame_no -> pgno)` from the WAL-index `aPgno` array (`1..=max_frame`) for building
    /// the checkpoint iterator (`walIteratorInit`, `wal.c:1866`).
    pub fn read_frame_pgno(&self, frame_no: u32) -> Result<u32> {
        self.shm.read_frame_pgno_internal(frame_no)
    }

    /// Scans `(frame_no, pgno)` across `start_frame..=end_frame` in the WAL-index `aPgno` array,
    /// reusing each 32 KiB region mapping for the entire block.
    pub fn for_each_frame_pgno(
        &self,
        start_frame: u32,
        end_frame: u32,
        f: &mut dyn FnMut(u32, u32),
    ) -> Result<()> {
        self.shm
            .for_each_frame_pgno_internal(start_frame, end_frame, f)
    }

    /// Acquires `EXCLUSIVE WAL_READ_LOCK(1..=4)` for `Restart` or `Truncate` (`wal.c:2456`),
    /// rolling back any acquired subset if a slot fails or returns `Busy`.
    pub fn lock_all_positive_readers_with_busy(
        &mut self,
        mut busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<()> {
        if !self.holds_write_lock() {
            return Err(SqliteError::misuse(
                "locking WAL_READ_LOCK(1..=4) in checkpoint requires WAL_WRITE_LOCK",
            ));
        }
        let mut newly_locked = Vec::with_capacity(4);
        for slot in 1..(WAL_NREADER as u8) {
            let idx = wal_read_lock_slot(slot).unwrap();
            if self.acquired_stack.contains(&idx) {
                continue;
            }
            let res = self.shm.lock_additional_exclusive_with_busy_internal(
                idx,
                match busy_handler.as_mut() {
                    Some(h) => Some(&mut **h),
                    None => None,
                },
            );
            match res {
                Ok(()) => {
                    self.acquired_stack.push(idx);
                    newly_locked.push(idx);
                }
                Err(err) => {
                    for &held_idx in newly_locked.iter().rev() {
                        if let Some(p) = self.acquired_stack.iter().rposition(|&x| x == held_idx) {
                            self.acquired_stack.remove(p);
                        }
                        let _ = self.shm.unlock_single_exclusive_internal(held_idx);
                    }
                    return Err(err);
                }
            }
        }
        Ok(())
    }

    /// Releases `EXCLUSIVE WAL_READ_LOCK(1..=4)` in reverse order after `Restart`/`Truncate` (`wal.c:2467`).
    pub fn unlock_all_positive_readers(&mut self) -> Result<()> {
        let mut first_err: Option<SqliteError> = None;
        for slot in (1..(WAL_NREADER as u8)).rev() {
            let idx = wal_read_lock_slot(slot).unwrap();
            if let Some(pos) = self.acquired_stack.iter().rposition(|&x| x == idx) {
                self.acquired_stack.remove(pos);
                if let Err(e) = self.shm.unlock_single_exclusive_internal(idx)
                    && first_err.is_none()
                {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
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

    /// Maps `region` (`extend = true`) and zeroes its `aPgno` + `aHash` payload prior to scanning
    /// recovery frames (`walIndexRecover`, `wal.c:1528-1595`).
    pub fn reset_region_for_recovery(&mut self, region: u32) -> Result<()> {
        self.shm.reset_region_for_recovery_internal(region)
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

/// RAII sub-guard for `walIndexRecover` invoked while `CheckpointTxnGuard` already holds
/// `WAL_CKPT_LOCK` and `WAL_WRITE_LOCK` (`wal.c:1436`), owning ONLY `WAL_RECOVER_LOCK`.
#[derive(Debug)]
pub struct CkptRecoveryGuard<'g, 'a> {
    ckpt: &'g mut CheckpointTxnGuard<'a>,
    active: bool,
}

impl CkptRecoveryGuard<'_, '_> {
    /// Maps `region` (`extend = true`) and zeroes its `aPgno` + `aHash` payload prior to scanning.
    pub fn reset_region_for_recovery(&mut self, region: u32) -> Result<()> {
        self.ckpt.shm.reset_region_for_recovery_internal(region)
    }

    /// Appends `(frame_no -> pgno)` into the WAL-index hash table during recovery.
    pub fn record_frame(
        &mut self,
        frame_no: u32,
        pgno: u32,
        committed_mx_frame: u32,
    ) -> Result<()> {
        self.ckpt
            .shm
            .record_frame_internal(frame_no, pgno, committed_mx_frame)
    }

    /// Truncates hash entries strictly above `max_frame`.
    pub fn truncate_frames_above(&mut self, max_frame: u32) -> Result<()> {
        self.ckpt.shm.truncate_frames_above_internal(max_frame)
    }

    /// Publishes the recovered header and updates `WalCkptInfo` + nonblocking `aReadMark[1..=4]`.
    pub fn finish_recovery_header_and_marks(
        &mut self,
        header: &WalIndexHeaderData,
    ) -> Result<(WalIndexHeaderData, [bool; WAL_NREADER])> {
        self.ckpt
            .shm
            .finish_recovery_header_and_marks_internal(header)
    }

    /// Explicitly releases `WAL_RECOVER_LOCK` while preserving the parent checkpoint's locks.
    pub fn finish(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.ckpt
            .shm
            .unlock_single_exclusive_internal(WAL_RECOVER_LOCK)
    }
}

impl Drop for CkptRecoveryGuard<'_, '_> {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            if let Err(e) = self
                .ckpt
                .shm
                .unlock_single_exclusive_internal(WAL_RECOVER_LOCK)
            {
                self.ckpt
                    .shm
                    .poison_connection(format!("failed to release WAL_RECOVER_LOCK: {e}"));
            }
        }
    }
}

/// Outcome of probing `WAL_WRITE_LOCK` and `WAL_RECOVER_LOCK` on a read-only `-shm` connection
/// when `WalIndexHdr` is uninitialized or mismatched (`wal.c:2788-2793, 3191-3196`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadonlyHeaderProbe {
    /// `SHARED WAL_WRITE_LOCK` succeeded (immediately released): no writer/recovery is active,
    /// so read-only caller must fail with `SQLITE_READONLY_RECOVERY`.
    ReadonlyRecovery,
    /// `WAL_WRITE_LOCK` was busy, `SHARED WAL_RECOVER_LOCK` succeeded (immediately released):
    /// an ordinary writer is active, so caller should retry (`WAL_RETRY`).
    WriterActiveRetry,
    /// Both `WAL_WRITE_LOCK` and `WAL_RECOVER_LOCK` were busy: another process is actively
    /// recovering the WAL, so caller must fail with `SQLITE_BUSY_RECOVERY`.
    BusyRecovery,
}

/// Owned RAII guard holding standalone `EXCLUSIVE WAL_WRITE_LOCK` on an `Arc<WalShm>` for
/// `walIndexReadHdr` header initialization / `walIndexRecover` (`wal.c:2796-2820`).
#[derive(Debug)]
pub struct OwnedInitWriteGuard {
    shm: Arc<WalShm>,
    active: bool,
}

impl OwnedInitWriteGuard {
    pub(crate) fn new(shm: Arc<WalShm>) -> Self {
        Self { shm, active: true }
    }

    /// Maps region 0 (`extend = true`) and attempts to read `(WalIndexHdr, WalCkptInfo)`.
    pub fn extend_and_try_read_header(
        &self,
    ) -> Result<Option<(WalIndexHeaderData, WalCkptInfoSnapshot)>> {
        let _ = self.shm.ensure_region_writable_internal(0)?;
        self.shm.try_read_header_and_ckpt_info_internal()
    }

    /// Enters `walIndexRecover` (`WAL_CKPT_LOCK` + `WAL_RECOVER_LOCK`, never `WAL_READ_LOCK(0)`).
    pub fn begin_recovery(&mut self) -> Result<InitRecoveryGuard<'_>> {
        let _ = self.shm.enter_recovery_from_writer_internal(None)?;
        Ok(InitRecoveryGuard {
            init: self,
            active: true,
        })
    }

    /// Releases `EXCLUSIVE WAL_WRITE_LOCK` and returns the connection to `Idle`.
    pub fn release(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.shm.release_writer_internal(None, false)
    }
}

/// RAII sub-guard for `walIndexRecover` invoked from [`OwnedInitWriteGuard`], owning ONLY
/// `WAL_CKPT_LOCK` and `WAL_RECOVER_LOCK` while the parent [`OwnedInitWriteGuard`] retains
/// `WAL_WRITE_LOCK`.
#[derive(Debug)]
pub struct InitRecoveryGuard<'g> {
    init: &'g mut OwnedInitWriteGuard,
    active: bool,
}

impl InitRecoveryGuard<'_> {
    /// Maps `region` (`extend = true`) and zeroes its `aPgno` + `aHash` payload prior to scanning.
    pub fn reset_region_for_recovery(&mut self, region: u32) -> Result<()> {
        self.init.shm.reset_region_for_recovery_internal(region)
    }

    /// Appends `(frame_no -> pgno)` into the WAL-index hash table during recovery.
    pub fn record_frame(
        &mut self,
        frame_no: u32,
        pgno: u32,
        committed_mx_frame: u32,
    ) -> Result<()> {
        self.init
            .shm
            .record_frame_internal(frame_no, pgno, committed_mx_frame)
    }

    /// Truncates hash entries strictly above `max_frame`.
    pub fn truncate_frames_above(&mut self, max_frame: u32) -> Result<()> {
        self.init.shm.truncate_frames_above_internal(max_frame)
    }

    /// Publishes the recovered header and updates `WalCkptInfo` + nonblocking `aReadMark[1..=4]`.
    pub fn finish_recovery_header_and_marks(
        &mut self,
        header: &WalIndexHeaderData,
    ) -> Result<(WalIndexHeaderData, [bool; WAL_NREADER])> {
        self.init
            .shm
            .finish_recovery_header_and_marks_internal(header)
    }

    /// Releases `WAL_RECOVER_LOCK` and `WAL_CKPT_LOCK` in reverse order, leaving `WAL_WRITE_LOCK`
    /// owned by the parent [`OwnedInitWriteGuard`].
    pub fn finish(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.init
            .shm
            .exit_recovery_to_writer_internal(None)
            .map(|_| ())
    }
}

impl Drop for InitRecoveryGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            let _ = self.init.shm.exit_recovery_to_writer_internal(None);
        }
    }
}

impl Drop for OwnedInitWriteGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            let _ = self.shm.release_writer_internal(None, true);
        }
    }
}

/// Owned `Arc<WalShm>`-backed reader snapshot guard (`Send + Sync`, zero lifetimes).
#[derive(Debug)]
pub struct OwnedWalReadGuard {
    shm: Arc<WalShm>,
    os_slot: Option<u8>,
    logical_slot: u8,
    epoch_at_entry: u64,
    active: bool,
}

impl OwnedWalReadGuard {
    pub(crate) fn new(
        shm: Arc<WalShm>,
        os_slot: Option<u8>,
        logical_slot: u8,
        epoch_at_entry: u64,
    ) -> Self {
        Self {
            shm,
            os_slot,
            logical_slot,
            epoch_at_entry,
            active: true,
        }
    }

    #[inline]
    pub const fn slot(&self) -> u8 {
        self.logical_slot
    }

    #[inline]
    pub const fn holds_os_read_lock(&self) -> bool {
        self.os_slot.is_some()
    }

    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch_at_entry
    }

    /// Reads and verifies `WalIndexHdr`.
    pub fn read_header_verified(&self) -> Result<WalIndexHeaderData> {
        self.shm.read_header_verified_internal()
    }

    /// Reads `aReadMark[slot]`.
    pub fn read_mark(&self) -> Result<u32> {
        self.shm.read_slot_mark_internal(self.logical_slot)
    }

    /// Reads `WalCkptInfo` (`nBackfill`, `aReadMark[0..5]`, `nBackfillAttempted`).
    pub fn read_ckpt_info(&self) -> Result<WalCkptInfoSnapshot> {
        self.shm.read_ckpt_info_internal()
    }

    /// Post-lock verification (`wal.c:3250, 3315-3320`): executes `shm_barrier()`, verifies
    /// `aReadMark[slot] == expected_mark` (when `Some`), and compares live `WalIndexHdr` against
    /// `expected_hdr`.
    pub fn verify_post_lock(
        &self,
        expected_mark: Option<u32>,
        expected_hdr: &WalIndexHeaderData,
    ) -> Result<bool> {
        self.shm
            .verify_post_read_lock_internal(self.logical_slot, expected_mark, expected_hdr)
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

    /// Releases the OS `SHARED WAL_READ_LOCK(slot)` upon entering `WAL_EXCLUSIVE_MODE` under a
    /// `.db` `EXCLUSIVE` lock (`sqlite3WalExclusiveMode(pWal, 1)`, `wal.c:4585-4590`).
    pub fn enter_exclusive_mode(&mut self) -> Result<()> {
        if let Some(slot) = self.os_slot.take() {
            self.shm.release_reader_internal(slot, false)?;
        }
        Ok(())
    }

    /// Re-acquires the OS `SHARED WAL_READ_LOCK(slot)` upon leaving `WAL_EXCLUSIVE_MODE` back to
    /// `WAL_NORMAL_MODE` (`sqlite3WalExclusiveMode(pWal, 0)`, `wal.c:4574-4580`).
    pub fn exit_exclusive_mode(&mut self) -> Result<()> {
        if self.os_slot.is_none() {
            let epoch = self
                .shm
                .reacquire_reader_from_idle_internal(self.logical_slot)?;
            self.os_slot = Some(self.logical_slot);
            self.epoch_at_entry = epoch;
        }
        Ok(())
    }

    /// Upgrades this reader snapshot into an [`OwnedWriterTxnGuard`] by acquiring
    /// `EXCLUSIVE WAL_WRITE_LOCK` (`sqlite3WalBeginWriteTransaction`).
    ///
    /// If acquiring `WAL_WRITE_LOCK` fails (e.g. `SQLITE_BUSY`), returns `Err((self, err))` with
    /// this reader guard completely intact!
    pub fn upgrade_to_writer(
        mut self,
    ) -> std::result::Result<OwnedWriterTxnGuard, (Self, SqliteError)> {
        let res = match self.os_slot {
            Some(slot) => self.shm.upgrade_reader_to_writer_internal(slot),
            None => self.shm.acquire_writer_from_idle_internal(),
        };
        match res {
            Ok(epoch) => {
                self.active = false;
                Ok(OwnedWriterTxnGuard {
                    shm: Arc::clone(&self.shm),
                    os_reader_slot: self.os_slot,
                    logical_slot: Some(self.logical_slot),
                    exclusive_db_mode: self.os_slot.is_none(),
                    epoch_at_entry: epoch,
                    active: true,
                })
            }
            Err(err) => Err((self, err)),
        }
    }

    /// Explicitly releases the reader lock and surfaces any unlock error.
    pub fn unlock(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        if let Some(slot) = self.os_slot.take() {
            self.shm.release_reader_internal(slot, false)
        } else {
            Ok(())
        }
    }
}

impl Drop for OwnedWalReadGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            if let Some(slot) = self.os_slot.take() {
                let _ = self.shm.release_reader_internal(slot, true);
            }
        }
    }
}

/// Owned `Arc<WalShm>`-backed writer transaction guard (`Send + Sync`, zero lifetimes).
#[derive(Debug)]
pub struct OwnedWriterTxnGuard {
    shm: Arc<WalShm>,
    os_reader_slot: Option<u8>,
    logical_slot: Option<u8>,
    exclusive_db_mode: bool,
    epoch_at_entry: u64,
    active: bool,
}

impl OwnedWriterTxnGuard {
    #[inline]
    pub const fn reader_slot(&self) -> Option<u8> {
        self.logical_slot
    }

    #[inline]
    pub const fn holds_os_read_lock(&self) -> bool {
        self.os_reader_slot.is_some()
    }

    #[inline]
    pub const fn exclusive_db_mode(&self) -> bool {
        self.exclusive_db_mode
    }

    #[inline]
    pub const fn epoch(&self) -> u64 {
        self.epoch_at_entry
    }

    /// Reads and verifies live `WalIndexHdr`.
    pub fn read_header_verified(&self) -> Result<WalIndexHeaderData> {
        self.shm.read_header_verified_internal()
    }

    /// Reads live `WalCkptInfo` (`nBackfill`, `aReadMark[0..5]`, `nBackfillAttempted`).
    pub fn read_ckpt_info(&self) -> Result<WalCkptInfoSnapshot> {
        self.shm.read_ckpt_info_internal()
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

    /// Appends `(frame_no -> pgno)` into the WAL-index `aPgno` + `aHash` tables (`walIndexAppend`).
    pub fn record_frame(
        &mut self,
        frame_no: u32,
        pgno: u32,
        committed_mx_frame: u32,
    ) -> Result<()> {
        self.shm
            .record_frame_internal(frame_no, pgno, committed_mx_frame)
    }

    /// Appends a contiguous slice of `pgnos` starting at `first_frame_no` into the WAL-index
    /// `aPgno` + `aHash` tables (`walIndexAppend`), resolving each 32 KiB region once per block.
    pub fn record_frames_batch(&mut self, first_frame_no: u32, pgnos: &[u32]) -> Result<()> {
        self.shm.record_frames_batch_internal(first_frame_no, pgnos)
    }

    /// Removes hash-table slots and `aPgno` entries strictly above `max_frame` (`walCleanupHash`).
    pub fn truncate_frames_above(&mut self, max_frame: u32) -> Result<()> {
        self.shm.truncate_frames_above_internal(max_frame)
    }

    /// Publishes `header` (`copy2 -> shm_barrier -> copy1`).
    pub fn publish_header(&mut self, header: &WalIndexHeaderData) -> Result<WalIndexHeaderData> {
        self.shm.publish_header_internal(header)
    }

    /// Attempts `walRestartHdr` during `walRestartLog` (`wal.c:3972-3981`) when `reader_slot == Some(0)`
    /// and `nBackfill > 0`: tries nonblocking `EXCLUSIVE WAL_READ_LOCK(1..=4)` across all 4 positive
    /// slots (rolling back any acquired subset on `Busy`/error).
    ///
    /// - Returns `Ok(Some(restarted_hdr))` if all 4 slots were free and header + marks + hash were reset.
    /// - Returns `Ok(None)` if any slot returned `ErrorCode::Busy` (skipping reset!).
    /// - Propagates any non-`Busy` error immediately.
    pub fn try_reset_header_if_no_positive_readers(
        &mut self,
        base_header: &WalIndexHeaderData,
        new_salt1: u32,
    ) -> Result<Option<WalIndexHeaderData>> {
        self.shm
            .try_reset_header_under_write_internal(base_header, new_salt1)
    }

    /// Drops `WAL_READ_LOCK(0)` while continuing to hold `EXCLUSIVE WAL_WRITE_LOCK` (`wal.c:3984-3985`).
    pub fn release_read_slot_zero_under_write(&mut self) -> Result<()> {
        if self.logical_slot != Some(0) {
            return Err(SqliteError::misuse(
                "release_read_slot_zero_under_write requires logical_slot == Some(0)",
            ));
        }
        if let Some(0) = self.os_reader_slot.take() {
            self.shm.release_reader_under_write_internal(0)?;
        }
        self.logical_slot = None;
        Ok(())
    }

    /// Claims `aReadMark[slot] = mark` (`slot` in `1..=4`) under brief `EXCLUSIVE WAL_READ_LOCK(slot)`
    /// while continuing to hold `EXCLUSIVE WAL_WRITE_LOCK`, without yet acquiring `SHARED WAL_READ_LOCK(slot)`
    /// (`walTryBeginRead(..., useWal=1)`, `wal.c:3275-3286`).
    ///
    /// Returns `Ok(true)` if claimed, `Ok(false)` on `ErrorCode::Busy`, or `Err` on other errors.
    pub fn try_claim_positive_read_mark_under_write(
        &mut self,
        slot: u8,
        mark: u32,
    ) -> Result<bool> {
        if slot == 0 || usize::from(slot) >= WAL_NREADER {
            return Err(SqliteError::misuse("positive reader slot must be in 1..=4"));
        }
        if self.logical_slot.is_some() {
            return Err(SqliteError::misuse(
                "writer already holds a reader slot; release before claiming another",
            ));
        }
        self.shm
            .try_claim_read_mark_under_write_internal(slot, mark, !self.exclusive_db_mode)
    }

    /// Acquires a positive reader slot `slot` (`1..=4`) while continuing to hold `EXCLUSIVE WAL_WRITE_LOCK`
    /// (`walTryBeginRead(..., useWal=1)`, `wal.c:3275-3306`), optionally claiming `aReadMark[slot] = claim_mark`
    /// under brief `EXCLUSIVE WAL_READ_LOCK(slot)` first.
    pub fn acquire_positive_read_slot_under_write(
        &mut self,
        slot: u8,
        claim_mark: Option<u32>,
    ) -> Result<()> {
        if slot == 0 || usize::from(slot) >= WAL_NREADER {
            return Err(SqliteError::misuse("positive reader slot must be in 1..=4"));
        }
        if self.logical_slot.is_some() {
            return Err(SqliteError::misuse(
                "writer already holds a reader slot; release before acquiring another",
            ));
        }
        self.shm
            .acquire_reader_under_write_internal(slot, claim_mark, !self.exclusive_db_mode)?;
        self.logical_slot = Some(slot);
        if !self.exclusive_db_mode {
            self.os_reader_slot = Some(slot);
        }
        Ok(())
    }

    /// Post-lock verification while `WAL_WRITE_LOCK` is held (`wal.c:3315-3320`).
    pub fn verify_post_read_lock(
        &self,
        expected_mark: Option<u32>,
        expected_hdr: &WalIndexHeaderData,
    ) -> Result<bool> {
        let slot = self
            .logical_slot
            .ok_or_else(|| SqliteError::misuse("no logical read slot held for verification"))?;
        self.shm
            .verify_post_read_lock_internal(slot, expected_mark, expected_hdr)
    }

    /// Releases the currently held positive reader slot while keeping `EXCLUSIVE WAL_WRITE_LOCK`
    /// (used if `verify_post_read_lock` detects a concurrent mark race and retries `walTryBeginRead`).
    pub fn release_positive_read_slot_under_write(&mut self) -> Result<()> {
        if let Some(slot) = self.os_reader_slot.take() {
            self.shm.release_reader_under_write_internal(slot)?;
        }
        self.logical_slot = None;
        Ok(())
    }

    /// Downgrades this writer guard back to an [`OwnedWalReadGuard`] (`Write -> Read`), releasing
    /// `WAL_WRITE_LOCK` while RETAINING the connection's reader slot (`1..=4` or `0`).
    pub fn downgrade_to_reader(mut self) -> Result<OwnedWalReadGuard> {
        let logical_slot = self.logical_slot.ok_or_else(|| {
            SqliteError::misuse("cannot downgrade writer without an active reader slot")
        })?;
        self.active = false;
        let epoch = self
            .shm
            .downgrade_writer_to_reader_internal(self.os_reader_slot)?;
        Ok(OwnedWalReadGuard::new(
            Arc::clone(&self.shm),
            self.os_reader_slot,
            logical_slot,
            epoch,
        ))
    }

    /// Releases `WAL_WRITE_LOCK` AND any held reader slot, returning the connection to `Idle`.
    pub fn unlock_all(mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.shm
            .release_writer_internal(self.os_reader_slot.take(), false)
    }
}

impl Drop for OwnedWriterTxnGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            let _ = self
                .shm
                .release_writer_internal(self.os_reader_slot.take(), true);
        }
    }
}
