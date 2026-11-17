//! Unix `xFetch` / `xUnfetch` and `SQLITE_FCNTL_MMAP_SIZE` (os_unix.c).
use std::fs::File;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{FetchPage, sys};
use sqliters_types::{ErrorCode, Result, SqliteError, extended};

#[derive(Default)]
pub(crate) struct FetchState {
    limit: i64,
    mapping: Option<sys::unix::ReadMapping>,
    usable_len: u64,
    outstanding: usize,
}

fn lock(state: &Mutex<FetchState>) -> Result<MutexGuard<'_, FetchState>> {
    state
        .lock()
        .map_err(|_| SqliteError::new(ErrorCode::Internal, "poisoned fetch state"))
}

impl FetchState {
    fn map(&mut self, file: &File, size_hint: Option<u64>) -> Result<()> {
        debug_assert_eq!(self.outstanding, 0);
        let file_size = match size_hint {
            Some(size) => size,
            None => file
                .metadata()
                .map_err(|e| SqliteError::with_extended(extended::SQLITE_IOERR_FSTAT, e.to_string()))?
                .len(),
        };
        let size = file_size.min(self.limit as u64);
        if size != 0 {
            self.mapping = sys::unix::ReadMapping::map(file, size as usize);
            if self.mapping.is_none() {
                // unixRemapfile: mapping failure is not a read error. Disable
                // subsequent mapping attempts and let the pager use xRead.
                self.limit = 0;
            }
        }
        self.usable_len = self.mapping.as_ref().map_or(0, |m| m.len() as u64);
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
        if state.usable_len != 0 {
            state.mapping = None;
            state.usable_len = 0;
            state.map(file, None)?;
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
        state.map(file, None)?;
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
    if end > state.usable_len {
        return Ok(None);
    }
    let data = mapping.copy(offset as usize, amount);
    state.outstanding += 1;
    Ok(Some(FetchPage::new(
        data,
        Box::new(Lease(Arc::clone(shared))),
    )))
}

/// fcntlSizeHint for the Unix mapping, with SQLite's default chunk size of zero.
pub(crate) fn size_hint(shared: &Mutex<FetchState>, file: &File, size: u64) -> Result<()> {
    use std::os::fd::AsRawFd;

    let mut state = lock(shared)?;
    if state.limit <= 0 || size <= state.usable_len {
        return Ok(());
    }
    sys::unix::ftruncate_fd(file.as_raw_fd(), size, extended::SQLITE_IOERR_TRUNCATE)?;
    // unixMapfile may extend the mapping only when there are no live leases.
    if state.outstanding == 0 && size.min(state.limit as u64) > state.usable_len {
        state.mapping = None;
        state.usable_len = 0;
        state.map(file, Some(size))?;
    }
    Ok(())
}

pub(crate) fn truncate(
    shared: &Mutex<FetchState>,
    file: &File,
    size: u64,
) -> Result<()> {
    use std::os::fd::AsRawFd;

    // unixTruncate shortens the usable part of a mapping. Keep the original
    // length for munmap and outstanding leases, and serialize with xFetch.
    let mut state = lock(shared)?;
    sys::unix::ftruncate_fd(file.as_raw_fd(), size, extended::SQLITE_IOERR_TRUNCATE)?;
    state.usable_len = state.usable_len.min(size);
    Ok(())
}

pub(crate) fn unfetch_all(shared: &Mutex<FetchState>) -> Result<()> {
    let mut state = lock(shared)?;
    if state.outstanding != 0 {
        return Err(SqliteError::misuse("cannot unmap outstanding fetch pages"));
    }
    state.mapping = None;
    state.usable_len = 0;
    Ok(())
}
