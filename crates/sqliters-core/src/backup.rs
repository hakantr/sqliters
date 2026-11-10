//! Online Backup Engine (`sqlite3_backup_init`, `sqlite3_backup_step`, `sqlite3_backup_finish`,
//! `sqlite3_backup_remaining`, `sqlite3_backup_pagecount` in `sqlite/src/backup.c`).
#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use sqliters_btree::BackupTracker;
use sqliters_pager::{JournalMode, TransactionState};
use sqliters_types::{DatabaseId, DatabaseKind, ErrorCode, Pgno, Result, SqliteError, extended};

use crate::{
    Connection, ConnectionShared, DbSlot, HandleId, HandleKind, PendingDrop, Schema,
    SchemaInstanceId,
};

/// Outcome of a single [`Backup::step`] invocation (`SQLITE_OK` vs `SQLITE_DONE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStepResult {
    /// Additional pages remain to be copied (`SQLITE_OK`).
    More,
    /// All pages have been copied and committed to the destination database (`SQLITE_DONE`).
    Done,
}

impl BackupStepResult {
    /// Returns `true` if the backup has completed (`BackupStepResult::Done`).
    #[inline]
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }
}

/// Online database backup handle (`sqlite3_backup*` in `sqlite/src/backup.c`).
///
/// Holds `Arc` references to the source and destination connection states without holding
/// connection locks between [`Backup::step`] calls.
pub struct Backup {
    src: Arc<ConnectionShared>,
    src_instance_id: SchemaInstanceId,
    dst: Arc<ConnectionShared>,
    dst_instance_id: SchemaInstanceId,
    dst_handle_id: Option<HandleId>,
    tracker: Arc<Mutex<BackupTracker>>,
    i_next: Pgno,
    has_started_copy: bool,
    dest_locked: bool,
    is_done: bool,
    fatal_error: Option<SqliteError>,
    remaining: i32,
    pagecount: i32,
    last_src_page_size: u32,
    last_src_cc: u32,
    last_src_wal: Option<(u32, u32, u32)>,
}

impl std::fmt::Debug for Backup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backup")
            .field("src_conn_id", &self.src.conn_id)
            .field("src_instance_id", &self.src_instance_id)
            .field("dst_conn_id", &self.dst.conn_id)
            .field("dst_instance_id", &self.dst_instance_id)
            .field("i_next", &self.i_next)
            .field("remaining", &self.remaining)
            .field("pagecount", &self.pagecount)
            .field("is_done", &self.is_done)
            .finish()
    }
}

fn paths_refer_to_same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b))
        && ca == cb
    {
        return true;
    }
    false
}

impl Backup {
    /// Initializes an online backup from `(src, src_db)` into `(dst, dst_db)`
    /// (`sqlite3_backup_init` in `sqlite/src/backup.c:141-221`).
    pub fn init(src: &Connection, src_db: &str, dst: &Connection, dst_db: &str) -> Result<Self> {
        let src_shared = Arc::clone(src.shared());
        let dst_shared = Arc::clone(dst.shared());

        if src_shared.conn_id == dst_shared.conn_id {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "source and destination must be distinct",
            ));
        }

        let (src_instance_id, dst_instance_id, dst_handle_id, tracker) = {
            let mut session = ConnectionShared::enter_dual(&src_shared, &dst_shared)?;
            let (src_inner, dst_inner) = session.src_and_dst_mut();
            src_inner.ensure_usable()?;
            dst_inner.ensure_usable()?;

            let Some(src_db_id) = src_inner.db_set.find_db_id(src_db) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {src_db}"),
                ));
            };
            let Some(dst_db_id) = dst_inner.db_set.find_db_id(dst_db) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {dst_db}"),
                ));
            };

            if src_db_id == DatabaseId::TEMP && !src_inner.db_set.is_temp_open() {
                let enc = src_inner.effective_encoding();
                let ws = src_inner.writable_schema;
                src_inner.db_set.ensure_temp_open(enc, ws)?;
            }
            if dst_db_id == DatabaseId::TEMP && !dst_inner.db_set.is_temp_open() {
                let enc = dst_inner.effective_encoding();
                let ws = dst_inner.writable_schema;
                dst_inner.db_set.ensure_temp_open(enc, ws)?;
            }

            let src_slot = src_inner.db_set.slot(src_db_id).ok_or_else(|| {
                SqliteError::new(ErrorCode::Error, format!("unknown database {src_db}"))
            })?;
            let dst_slot = dst_inner.db_set.slot(dst_db_id).ok_or_else(|| {
                SqliteError::new(ErrorCode::Error, format!("unknown database {dst_db}"))
            })?;

            let src_bt = src_slot
                .btree
                .as_ref()
                .ok_or_else(|| SqliteError::misuse("source database slot closed"))?;
            let dst_bt = dst_slot
                .btree
                .as_ref()
                .ok_or_else(|| SqliteError::misuse("destination database slot closed"))?;

            if dst_shared.is_readonly || dst_bt.pager().is_readonly() {
                return Err(SqliteError::new(
                    ErrorCode::ReadOnly,
                    "destination database is read-only",
                ));
            }

            // Reject if both `src_db` and `dst_db` resolve to the same physical on-disk file.
            if src_bt.pager().db_kind() == DatabaseKind::NamedDisk
                && dst_bt.pager().db_kind() == DatabaseKind::NamedDisk
                && let (Some(sp), Some(dp)) = (src_bt.pager().db_path(), dst_bt.pager().db_path())
            {
                let same_vfs = src_bt.pager().vfs().name() == dst_bt.pager().vfs().name();
                if same_vfs {
                    let full_s = src_bt
                        .pager()
                        .vfs()
                        .full_pathname(sp)
                        .unwrap_or_else(|_| sp.to_path_buf());
                    let full_d = dst_bt
                        .pager()
                        .vfs()
                        .full_pathname(dp)
                        .unwrap_or_else(|_| dp.to_path_buf());
                    if paths_refer_to_same_file(&full_s, &full_d) {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "source and destination must be distinct",
                        ));
                    }
                }
            }

            // `checkReadTransaction(pDestDb, pDest)` in `sqlite/src/backup.c:125-131`:
            // Reject if destination database already has an open transaction or active lease.
            let dst_instance_id = dst_slot.schema_instance_id;
            if dst_bt.transaction_state() != TransactionState::Open
                || dst_inner
                    .activity
                    .has_active_tx_on_instance(dst_instance_id)
                || dst_slot.active_backups > 0
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "destination database is in use",
                ));
            }

            let src_instance_id = src_slot.schema_instance_id;
            let tracker = Arc::new(Mutex::new(BackupTracker::default()));

            let src_slot_mut = src_inner.db_set.slot_mut(src_db_id).unwrap();
            src_slot_mut.active_backups += 1;
            src_slot_mut.attach_backup_tracker(Arc::clone(&tracker));

            let dst_slot_mut = dst_inner.db_set.slot_mut(dst_db_id).unwrap();
            dst_slot_mut.active_backups += 1;

            let dst_handle_id = dst_inner.register_handle(HandleKind::BackupDestination);
            (src_instance_id, dst_instance_id, dst_handle_id, tracker)
        };

        Ok(Self {
            src: src_shared,
            src_instance_id,
            dst: dst_shared,
            dst_instance_id,
            dst_handle_id: Some(dst_handle_id),
            tracker,
            i_next: 1,
            has_started_copy: false,
            dest_locked: false,
            is_done: false,
            fatal_error: None,
            remaining: -1,
            pagecount: -1,
            last_src_page_size: 0,
            last_src_cc: 0,
            last_src_wal: None,
        })
    }

    /// Returns the number of pages remaining to be backed up as of the most recent [`Self::step`]
    /// (`sqlite3_backup_remaining`).
    #[inline]
    pub fn remaining(&self) -> i32 {
        self.remaining
    }

    /// Returns the total number of source pages as of the most recent [`Self::step`]
    /// (`sqlite3_backup_pagecount`).
    #[inline]
    pub fn pagecount(&self) -> i32 {
        self.pagecount
    }

    /// Copies up to `n_pages` pages (or all remaining pages when `n_pages < 0`) from the source
    /// database to the destination database (`sqlite3_backup_step` in `sqlite/src/backup.c:325-593`).
    pub fn step(&mut self, n_pages: i32) -> Result<BackupStepResult> {
        if self.is_done {
            return Ok(BackupStepResult::Done);
        }
        if let Some(ref err) = self.fatal_error {
            return Err(err.clone());
        }

        let mut session = ConnectionShared::enter_dual(&self.src, &self.dst)?;
        let (src_inner, dst_inner) = session.src_and_dst_mut();
        src_inner.ensure_usable()?;
        dst_inner.ensure_usable()?;

        let Some(src_db_id) = src_inner
            .db_set
            .find_db_id_by_instance(self.src_instance_id)
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "source database is no longer attached",
            ));
        };
        let Some(dst_db_id) = dst_inner
            .db_set
            .find_db_id_by_instance(self.dst_instance_id)
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "destination database is no longer attached",
            ));
        };

        // 1. Reject if `src` is currently in an uncommitted write transaction on `src_slot`.
        let src_has_writer_lease = src_inner
            .activity
            .has_active_writer_on_instance(self.src_instance_id);
        {
            let src_bt = src_inner
                .db_set
                .slot(src_db_id)
                .and_then(|s| s.btree.as_ref())
                .ok_or_else(|| SqliteError::misuse("source database slot closed"))?;
            if src_bt.transaction_state() == TransactionState::Write || src_has_writer_lease {
                return Err(SqliteError::new(
                    ErrorCode::Busy,
                    "source database is in a write transaction",
                ));
            }
        }

        // 2. Source read-transaction ownership (`bCloseTrans` in `sqlite/src/backup.c:347, 363-366, 575-580`):
        // If `src_slot` already has an open `Read` transaction, borrow it and preserve it at the end of `step()`.
        // If `src_slot` is in `TransactionState::Open`, open a read transaction now and close it before `step()` exits!
        let mut b_close_trans = false;
        {
            let src_slot = src_inner.db_set.slot_mut(src_db_id).unwrap();
            let src_bt = src_slot.btree.as_mut().unwrap();
            if src_bt.transaction_state() == TransactionState::Open {
                src_bt.begin_read()?;
                b_close_trans = true;
            }
        }

        let step_result = (|| -> Result<BackupStepResult> {
            let src_ps = src_inner
                .db_set
                .slot(src_db_id)
                .unwrap()
                .btree
                .as_ref()
                .unwrap()
                .pager()
                .page_size();

            // 3. Lock the destination database in a write transaction on first `step()`.
            if !self.dest_locked {
                let hid = self
                    .dst_handle_id
                    .ok_or_else(|| SqliteError::misuse("backup handle already finished"))?;
                if dst_inner
                    .activity
                    .has_active_tx_on_instance(self.dst_instance_id)
                {
                    return Err(SqliteError::new(
                        ErrorCode::Busy,
                        "destination database is busy",
                    ));
                }
                let dst_slot = dst_inner.db_set.slot_mut(dst_db_id).unwrap();
                let dst_bt = dst_slot.btree.as_mut().unwrap();
                if dst_bt.transaction_state() != TransactionState::Open {
                    return Err(SqliteError::new(
                        ErrorCode::Busy,
                        "destination database is busy",
                    ));
                }
                if !dst_bt.pager().is_page_size_fixed()
                    && dst_bt.pager().page_count() == 0
                    && !dst_bt.pager().is_wal_mode()
                {
                    let _ = dst_bt.pager_mut().set_page_size(src_ps);
                }
                dst_bt.begin_write()?;
                self.dest_locked = true;
                dst_inner.activity.start_with_instances(
                    hid,
                    true,
                    true,
                    vec![self.dst_instance_id],
                    vec![self.dst_instance_id],
                );
            }

            // 4. Validate page-size compatibility with WAL destination (`backup.c:401-410`).
            let (dst_orig_ps, dst_is_wal) = {
                let dst_bt = dst_inner
                    .db_set
                    .slot(dst_db_id)
                    .unwrap()
                    .btree
                    .as_ref()
                    .unwrap();
                let is_wal =
                    dst_bt.journal_mode() == JournalMode::Wal || dst_bt.pager().is_wal_mode();
                (dst_bt.pager().tx_orig_page_size(), is_wal)
            };
            if dst_is_wal && src_ps != dst_orig_ps {
                return Err(SqliteError::new(
                    ErrorCode::ReadOnly,
                    "destination database is in WAL mode and page sizes differ",
                ));
            }

            // 5. Inspect leaf `BackupTracker` and source snapshot metadata to determine whether
            // we must restart from page 1 or re-copy dirty pages `< self.i_next`.
            let (cur_src_pages, cur_src_cc, cur_src_wal) = {
                let src_bt = src_inner
                    .db_set
                    .slot(src_db_id)
                    .unwrap()
                    .btree
                    .as_ref()
                    .unwrap();
                (
                    src_bt.pager().page_count(),
                    src_bt.pager().header().change_counter,
                    src_bt.pager().wal_snapshot_signature(),
                )
            };

            let mut dirty_to_recopy: Vec<Pgno> = Vec::new();
            if self.has_started_copy {
                let mut tr = self.tracker.lock().unwrap_or_else(PoisonError::into_inner);
                let external_change = (cur_src_cc != self.last_src_cc
                    && tr.last_self_cc != Some(cur_src_cc))
                    || (cur_src_wal != self.last_src_wal && tr.last_self_wal != cur_src_wal);
                let shrunk_below_progress = cur_src_pages.saturating_add(1) < self.i_next;
                let ps_changed = src_ps != self.last_src_page_size;

                if tr.needs_restart || external_change || shrunk_below_progress || ps_changed {
                    self.i_next = 1;
                    tr.needs_restart = false;
                    tr.dirty_pages.clear();
                    dst_inner
                        .db_set
                        .slot_mut(dst_db_id)
                        .unwrap()
                        .btree
                        .as_mut()
                        .unwrap()
                        .pager_mut()
                        .backup_reset_raw_staging();
                } else {
                    let i_next = self.i_next;
                    dirty_to_recopy = tr
                        .dirty_pages
                        .iter()
                        .copied()
                        .filter(|&pgno| pgno >= 1 && pgno < i_next && pgno <= cur_src_pages)
                        .collect();
                    tr.dirty_pages.clear();
                }
            } else {
                let mut tr = self.tracker.lock().unwrap_or_else(PoisonError::into_inner);
                tr.needs_restart = false;
                tr.dirty_pages.clear();
            }

            // Re-copy any dirty pages `< self.i_next` from the current source read snapshot.
            for dirty_pgno in dirty_to_recopy {
                let is_lock_page = src_inner
                    .db_set
                    .slot(src_db_id)
                    .unwrap()
                    .btree
                    .as_ref()
                    .unwrap()
                    .pager()
                    .lock_byte_page()
                    == dirty_pgno;
                if is_lock_page {
                    continue;
                }
                let page_bytes = src_inner
                    .db_set
                    .slot_mut(src_db_id)
                    .unwrap()
                    .btree
                    .as_mut()
                    .unwrap()
                    .pager_mut()
                    .read_page(dirty_pgno)?;
                dst_inner
                    .db_set
                    .slot_mut(dst_db_id)
                    .unwrap()
                    .btree
                    .as_mut()
                    .unwrap()
                    .pager_mut()
                    .backup_write_page(dirty_pgno, src_ps, page_bytes)?;
            }

            // 6. Copy up to `n_pages` sequential pages starting at `self.i_next`.
            let mut copied = 0i32;
            while (n_pages < 0 || copied < n_pages) && self.i_next <= cur_src_pages {
                let src_pgno = self.i_next;
                let is_lock_page = src_inner
                    .db_set
                    .slot(src_db_id)
                    .unwrap()
                    .btree
                    .as_ref()
                    .unwrap()
                    .pager()
                    .lock_byte_page()
                    == src_pgno;
                if !is_lock_page {
                    let page_bytes = src_inner
                        .db_set
                        .slot_mut(src_db_id)
                        .unwrap()
                        .btree
                        .as_mut()
                        .unwrap()
                        .pager_mut()
                        .read_page(src_pgno)?;
                    dst_inner
                        .db_set
                        .slot_mut(dst_db_id)
                        .unwrap()
                        .btree
                        .as_mut()
                        .unwrap()
                        .pager_mut()
                        .backup_write_page(src_pgno, src_ps, page_bytes)?;
                }
                self.i_next += 1;
                copied += 1;
            }

            self.has_started_copy = true;
            self.last_src_page_size = src_ps;
            self.last_src_cc = cur_src_cc;
            self.last_src_wal = cur_src_wal;
            {
                let mut tr = self.tracker.lock().unwrap_or_else(PoisonError::into_inner);
                tr.last_self_cc = Some(cur_src_cc);
                tr.last_self_wal = cur_src_wal;
            }

            self.pagecount = cur_src_pages as i32;
            self.remaining = (cur_src_pages + 1).saturating_sub(self.i_next) as i32;

            if self.i_next > cur_src_pages {
                // 7. Finalize and commit destination database (`rc == SQLITE_DONE` in `backup.c:444-568`).
                {
                    let (src_hdr, src_av) = {
                        let src_slot = src_inner.db_set.slot(src_db_id).unwrap();
                        let src_bt = src_slot.btree.as_ref().unwrap();
                        (
                            src_bt.pager().header().clone(),
                            src_bt.pager().auto_vacuum_mode(),
                        )
                    };
                    let dst_slot = dst_inner.db_set.slot_mut(dst_db_id).unwrap();
                    let dst_bt = dst_slot.btree.as_mut().unwrap();
                    dst_bt
                        .pager_mut()
                        .backup_finalize(cur_src_pages, &src_hdr, src_av)?;
                    dst_bt.commit()?;
                }

                // Reset destination schema (`sqlite3ResetAllSchemasOfConnection`, `backup.c:454`)
                // and invalidate any open blobs/prepared statements on `dst`.
                {
                    let dst_slot = dst_inner.db_set.slot_mut(dst_db_id).unwrap();
                    let slot_name = dst_slot.name.clone();
                    if let Some(ref mut dst_bt) = dst_slot.btree {
                        if dst_db_id == DatabaseId::MAIN && dst_bt.pager().page_count() > 0 {
                            dst_inner.connection_encoding = dst_bt.encoding();
                        }
                        if dst_bt.pager().db_kind() == DatabaseKind::NamedDisk
                            && !dst_bt.pager().is_wal_mode()
                        {
                            dst_bt.pager_mut().invalidate_header_loaded();
                            dst_slot.schema = Schema::default();
                        } else {
                            match Schema::load_for_schema(dst_bt, Some(&slot_name)) {
                                Ok(mut fresh) => {
                                    fresh.rebind_collations(&dst_inner.collations);
                                    dst_slot.schema = fresh;
                                }
                                Err(_) => {
                                    dst_slot.schema = Schema::default();
                                }
                            }
                        }
                    }
                }
                let _ = dst_inner.db_set.reconcile_temp_triggers();
                dst_inner
                    .db_set
                    .invalidate_blobs_for_instance(self.dst_instance_id);
                dst_inner.schema_generation += 1;
                dst_inner.v1_expire_generation += 1;
                if let Some(hid) = self.dst_handle_id {
                    dst_inner.activity.remove(hid);
                }
                self.dest_locked = false;
                self.is_done = true;
                self.remaining = 0;
                Ok(BackupStepResult::Done)
            } else {
                Ok(BackupStepResult::More)
            }
        })();

        // Always close the source read transaction if `step()` opened it (`bCloseTrans`).
        if b_close_trans
            && let Some(src_slot) = src_inner.db_set.slot_mut(src_db_id)
            && let Some(ref mut src_bt) = src_slot.btree
        {
            let _ = src_bt.pager_mut().end_read_if_unused();
        }

        if let Err(ref err) = step_result
            && err.code != ErrorCode::Busy
            && err.code != ErrorCode::Locked
        {
            self.fatal_error = Some(err.clone());
        }

        step_result
    }

    /// Finishes the backup handle (`sqlite3_backup_finish` in `sqlite/src/backup.c:598-648`),
    /// rolling back any uncommitted destination transaction and releasing source/destination
    /// backup leases.
    pub fn finish(mut self) -> Result<()> {
        self.cleanup_explicit()
    }

    fn cleanup_explicit(&mut self) -> Result<()> {
        let Some(dst_hid) = self.dst_handle_id.take() else {
            return Ok(());
        };
        let mut session = ConnectionShared::enter_dual(&self.src, &self.dst)?;
        let (src_inner, dst_inner) = session.src_and_dst_mut();

        if let Some(src_slot) = src_inner.db_set.slot_by_instance_mut(self.src_instance_id) {
            src_slot.active_backups = src_slot.active_backups.saturating_sub(1);
            src_slot.detach_backup_tracker(&self.tracker);
        }

        let mut rb_res = Ok(());
        if let Some(dst_slot) = dst_inner.db_set.slot_by_instance_mut(self.dst_instance_id) {
            dst_slot.active_backups = dst_slot.active_backups.saturating_sub(1);
            if self.dest_locked
                && !self.is_done
                && let Some(ref mut dst_bt) = dst_slot.btree
            {
                dst_bt.pager_mut().backup_reset_raw_staging();
                if dst_bt.transaction_state() == TransactionState::Write {
                    rb_res =
                        dst_bt.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false);
                }
            }
        }
        if self.dest_locked {
            dst_inner.activity.remove(dst_hid);
            self.dest_locked = false;
        }
        dst_inner.open_handles.remove(&dst_hid);

        rb_res?;
        let _ = self.fatal_error.take();
        Ok(())
    }
}

impl Drop for Backup {
    fn drop(&mut self) {
        if let Some(dst_hid) = self.dst_handle_id.take() {
            self.src.enqueue_or_run_drop(PendingDrop::BackupSource {
                src_instance_id: self.src_instance_id,
                tracker: Arc::clone(&self.tracker),
            });
            self.dst
                .enqueue_or_run_drop(PendingDrop::BackupDestination {
                    handle_id: dst_hid,
                    dst_instance_id: self.dst_instance_id,
                    in_write_tx: self.dest_locked && !self.is_done,
                    active_lease: self.dest_locked,
                });
            self.dest_locked = false;
        }
    }
}

#[allow(dead_code)]
fn _assert_slot_used(_: &DbSlot) {}
