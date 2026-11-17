//! Unix `xFetch` / `xUnfetch` and `SQLITE_FCNTL_MMAP_SIZE` (os_unix.c).
use std::fs::File;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{FetchPage, sys};
use sqliters_types::{ErrorCode, Result, SqliteError, extended};

#[derive(Default)]
pub(crate) struct FetchState {
    limit: i64,
    mapping: Option<sys::unix::ReadMapping>,
    outstanding: usize,
}

fn lock(state: &Mutex<FetchState>) -> Result<MutexGuard<'_, FetchState>> {
    state
        .lock()
        .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned fetch state"))
}

impl FetchState {
    fn map(&mut self, file: &File) -> Result<()> {
        debug_assert_eq!(self.outstanding, 0);
        let size = file
            .metadata()
            .map_err(|e| SqliteError::with_extended(extended::SQLITE_IOERR_FSTAT, e.to_string()))?
            .len()
            .min(self.limit as u64);
        if size != 0 {
            self.mapping = sys::unix::ReadMapping::map(file, size as usize);
            if self.mapping.is_none() {
                // unixRemapfile: mapping failure is not a read error. Disable
                // subsequent mapping attempts and let the pager use xRead.
                self.limit = 0;
            }
        }
        Ok(())
    }
}

pub(crate) fn set_limit(state: &Mutex<FetchState>, file: &File, requested: i64) -> Result<i64> {
    let mut state = lock(state)?;
    let old = state.limit;
    let requested = requested.min(0x7fff0000);
    // os_unix.c:4243: an outstanding fetch vetoes the update, not just remapping.
    if requested >= 0 && requested != old && state.outstanding == 0 {
        state.limit = requested;
        if state.mapping.take().is_some() {
            state.map(file)?;
        }
    }
    Ok(old)
}

struct Lease(Arc<Mutex<FetchState>>);

impl Drop for Lease {
    fn drop(&mut self) {
        // No user code executes under this mutex. Recover the count even if a
        // previous operation unwound while holding it.
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert!(state.outstanding > 0);
        state.outstanding -= 1;
    }
}

pub(crate) fn fetch(
    shared: &Arc<Mutex<FetchState>>,
    file: &File,
    offset: u64,
    amount: usize,
) -> Result<Option<FetchPage>> {
    let mut state = lock(shared)?;
    if state.limit <= 0 {
        return Ok(None);
    }
    if state.mapping.is_none() {
        state.map(file)?;
    }
    let Some(mapping) = state.mapping.as_ref() else {
        return Ok(None);
    };
    let Some(end) = offset
        .checked_add(amount as u64)
        .and_then(|n| n.checked_add(256))
    else {
        return Ok(None);
    };
    // unixFetch's 256-byte EOF buffer also excludes the last physical page.
    if end > mapping.len() as u64 {
        return Ok(None);
    }
    let data = mapping.copy(offset as usize, amount);
    state.outstanding += 1;
    Ok(Some(FetchPage::new(
        data,
        Box::new(Lease(Arc::clone(shared))),
    )))
}

pub(crate) fn unfetch_all(shared: &Mutex<FetchState>) -> Result<()> {
    let mut state = lock(shared)?;
    if state.outstanding != 0 {
        return Err(SqliteError::misuse("cannot unmap outstanding fetch pages"));
    }
    state.mapping = None;
    Ok(())
}
