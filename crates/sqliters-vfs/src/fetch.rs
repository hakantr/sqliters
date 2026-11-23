//! Unix `xFetch` / `xUnfetch` and `SQLITE_FCNTL_MMAP_SIZE` (os_unix.c).
use std::fs::File;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{FetchPage, sys};
use sqliters_types::{ErrorCode, Result, SqliteError, extended};

#[derive(Default)]
pub(crate) struct FetchState {
    limit: i64,
    chunk_size: u64,
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

fn available(state: &mut FetchState, file: &File, offset: u64, amount: usize) -> Result<bool> {
    if state.limit <= 0 {
        return Ok(false);
    }
    if state.mapping.is_none() {
        state.map(file, None)?;
    }
    // unixFetch requires at least 256 bytes of addressable EOF padding.
    Ok(offset.checked_add(amount as u64).and_then(|n| n.checked_add(256))
        .is_some_and(|end| end <= state.usable_len))
}

pub(crate) fn fetch(
    shared: &Arc<Mutex<FetchState>>,
    file: &File,
    offset: u64,
    amount: usize,
) -> Result<Option<FetchPage>> {
    let mut state = lock(shared)?;
    if !available(&mut state, file, offset, amount)? {
        return Ok(None);
    }
    let data = state.mapping.as_ref().unwrap().copy(offset as usize, amount);
    state.outstanding += 1;
    Ok(Some(FetchPage::new(data, Box::new(Lease(Arc::clone(shared))))))
}

pub(crate) struct RangeLease {
    lease: Lease,
    offset: usize,
    len: usize,
}

impl RangeLease {
    pub(crate) fn copy_into(&self, offset: usize, output: &mut [u8]) -> Result<()> {
        let state = lock(&self.lease.0)?;
        if offset > self.len || output.len() > self.len - offset
            || (self.offset + offset + output.len()) as u64 > state.usable_len
        {
            return Err(SqliteError::misuse("fetch range out of bounds"));
        }
        state.mapping.as_ref().unwrap().copy_into(self.offset + offset, output);
        Ok(())
    }
}

pub(crate) fn fetch_range(
    shared: &Arc<Mutex<FetchState>>,
    file: &File,
    offset: u64,
    amount: usize,
) -> Result<Option<RangeLease>> {
    let mut state = lock(shared)?;
    if !available(&mut state, file, offset, amount)? {
        return Ok(None);
    }
    state.outstanding += 1;
    Ok(Some(RangeLease { lease: Lease(Arc::clone(shared)), offset: offset as usize, len: amount }))
}

pub(crate) fn set_chunk_size(shared: &Mutex<FetchState>, size: u32) -> Result<()> {
    lock(shared)?.chunk_size = u64::from(size);
    Ok(())
}

/// Unix fcntlSizeHint. With a chunk size, the pinned C build touches the last
/// byte of filesystem blocks while extending the file to a rounded size.
pub(crate) fn size_hint(shared: &Mutex<FetchState>, file: &File, size: u64) -> Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let mut state = lock(shared)?;
    let chunk = state.chunk_size;
    if chunk != 0 {
        let ioerr = extended::SQLITE_IOERR_WRITE;
        let metadata = file.metadata().map_err(|error|
            SqliteError::with_extended(extended::SQLITE_IOERR_FSTAT, error.to_string()))?;
        let rounded = size.div_ceil(chunk).checked_mul(chunk)
            .ok_or_else(|| SqliteError::with_extended(ioerr, ErrorCode::IoErr.as_str()))?;
        let block = metadata.blksize().max(1);
        let mut offset = metadata.len();
        while offset < rounded {
            let last = offset.saturating_add(block - offset % block).min(rounded) - 1;
            sys::unix::pwrite_all(file, &[0], last, ioerr).map_err(|error|
                SqliteError::with_extended(ioerr, error.message))?;
            offset = last + 1;
        }
    }
    if state.limit <= 0 || size <= state.usable_len {
        return Ok(());
    }
    if chunk == 0 {
        sys::unix::ftruncate_fd(file.as_raw_fd(), size, extended::SQLITE_IOERR_TRUNCATE)?;
    }
    // unixMapfile may extend the mapping only when there are no live leases.
    if state.outstanding == 0 && size.min(state.limit as u64) > state.usable_len {
        state.mapping = None;
        state.usable_len = 0;
        state.map(file, Some(size))?;
    }
    Ok(())
}

pub(crate) fn write_private_sorter(
    shared: &Mutex<FetchState>,
    file: &File,
    buf: &[u8],
    offset: u64,
) -> Result<()> {
    // A range copy and this write must not access the mapped bytes concurrently.
    let _guard = lock(shared)?;
    sys::unix::pwrite_all(file, buf, offset, extended::SQLITE_IOERR_WRITE)
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


#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::mpsc::{RecvTimeoutError, sync_channel};
    use std::time::Duration;

    #[test]
    fn private_sorter_write_waits_for_the_fetch_lock() {
        let (_, file) = sys::unix::open_anonymous_temp().unwrap();
        let Some(file) = file else {
            return; // This filesystem uses the named fallback.
        };
        let shared = Arc::new(Mutex::new(FetchState::default()));
        set_limit(&shared, &file, 8192).unwrap();
        size_hint(&shared, &file, 4096).unwrap();
        let range = fetch_range(&shared, &file, 0, 1024).unwrap().unwrap();

        std::thread::scope(|scope| {
            let guard = lock(&shared).unwrap();
            let (started, start) = sync_channel(0);
            let (done, completed) = sync_channel(1);
            let file = &file;
            let shared = &shared;
            scope.spawn(move || {
                started.send(()).unwrap();
                done.send(write_private_sorter(shared, file, &[57; 16], 500)).unwrap();
            });
            start.recv().unwrap();
            assert!(matches!(
                completed.recv_timeout(Duration::from_millis(25)),
                Err(RecvTimeoutError::Timeout)
            ));
            drop(guard);
            completed.recv().unwrap().unwrap();
        });

        let mut bytes = [0; 16];
        range.copy_into(500, &mut bytes).unwrap();
        assert_eq!(bytes, [57; 16]);
    }
}
