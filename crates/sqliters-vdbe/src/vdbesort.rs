//! `vdbesort.c`: the sorter of `OP_SorterOpen` … `OP_SorterNext` (and `rowset.c`: `RowSet`).
//!
//! Port of the SQLite 3.54.1 memory and PMA sorting paths. What is ported
//! one to one: the record list (new records at the head), the flush decision of
//! `sqlite3VdbeSorterWrite` (`mxPmaSize` / `mnPmaSize` from the page size and the cache size
//! of the main database, `list.aMemory` accounting with `sizeof(SorterRecord)`), the list
//! sort `vdbeSorterSort` (64 slots, `vdbeSorterMerge` with "res<=0 takes p1"), the key
//! comparison (`vdbeSorterCompare` — the specialised integer / text / real comparators give
//! the same order as the general one for the records they are chosen for, so the general
//! comparator is used), `sqlite3VdbeSorterCompare`, `sqlite3VdbeSorterRowkey`.
//!
//! Runs and intermediate batches use anonymous temporary files when the
//! host supplies a VFS. External sorts read through buffers unless the
//! connection enables the optional sorter test control.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, atomic::{AtomicI32, Ordering}};

use sqliters_types::{ErrorCode, Result as SqlResult, SqliteError, TextEncoding, Value, extended};
use sqliters_vfs::{FetchRange, OpenFlags, Vfs, VfsFile};

use crate::vdbeaux_rt::{
    KeyInfoRt, UnpackedRecord, get_varint32, put_varint, sqlite3_varint_len, sqlite3_vdbe_record_compare_with_skip,
    sqlite3_vdbe_record_unpack,
};
use crate::vdbemem::{MEM_NULL, Mem};

/// The in-memory `vdbeSorterSort` schedule, shared by packed records and
/// materialized rows. C's list head is the newest record.
fn sort_memory_runs<T, E>(
    rows: Vec<T>,
    mut merge: impl FnMut(Vec<T>, Vec<T>) -> Result<Vec<T>, E>,
) -> Result<Vec<T>, E> {
    let mut slots: Vec<Option<Vec<T>>> = (0..64).map(|_| None).collect();
    for record in rows.into_iter().rev() {
        let mut run = vec![record];
        let mut index = 0;
        while let Some(other) = slots[index].take() {
            run = merge(run, other)?;
            index += 1;
        }
        slots[index] = Some(run);
    }
    let mut ordered = None;
    for run in slots.into_iter().flatten() {
        ordered = Some(match ordered {
            Some(earlier) => merge(earlier, run)?,
            None => run,
        });
    }
    Ok(ordered.unwrap_or_default())
}

/// In-memory `vdbeSorterSort` for projected rows whose comparison can fail.
#[doc(hidden)]
pub fn sort_memory_rows<T>(
    rows: Vec<T>,
    mut compare: impl FnMut(&T, &T) -> sqliters_types::Result<std::cmp::Ordering>,
) -> sqliters_types::Result<Vec<T>> {
    sort_memory_runs(rows, |left, right| {
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let mut left = left.into_iter().peekable();
        let mut right = right.into_iter().peekable();
        while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
            if compare(a, b)? != std::cmp::Ordering::Greater {
                merged.extend(left.next());
            } else {
                merged.extend(right.next());
            }
        }
        merged.extend(left);
        merged.extend(right);
        Ok(merged)
    })
}

/// `SQLITE_MAX_PMASZ`
const SQLITE_MAX_PMASZ: i64 = 1 << 29;
/// `sqlite3GlobalConfig.szPma` (`SQLITE_SORTER_PMASZ`)
const SQLITE_SORTER_PMASZ: i64 = 250;
/// `sizeof(SorterRecord)` on the reference platform (int + pointer union, 8-byte aligned)
const SZ_SORTER_RECORD: i64 = 16;

/// A conservative no-PMA bound for full FILE sort records under the pinned
/// default `szPma`. Callers supply the sort keys and every projected value,
/// including duplicates that SQLite may omit when packing its sort record.
/// Packed records contribute at most their 9-byte PMA length prefix on the
/// split-allocation path. ROUND8(record + 16) covers both allocation paths,
/// including the split allocator's heap-pressure check against mnPmaSize.
#[doc(hidden)]
pub fn file_sort_fits_default_pma<'a>(
    records: impl IntoIterator<Item = impl IntoIterator<Item = &'a Value>>,
    page_size: u32,
    enc: TextEncoding,
) -> bool {
    let mut available = i64::from(page_size).saturating_mul(SQLITE_SORTER_PMASZ) as usize;
    for record in records {
        let mut packed = 9usize; // Header length varint.
        for value in record {
            let bytes = match value {
                Value::Text(text) => {
                    // UTF-16 decoding ignores an odd trailing byte. Include it,
                    // as well as the size of a UTF-8 schema default.
                    enc.encoded_byte_len(text)
                        .saturating_add(usize::from(enc != TextEncoding::Utf8))
                        .max(text.len())
                }
                Value::Blob(blob) => blob.len(),
                Value::Null | Value::Integer(_) | Value::Real(_) => 8,
            };
            packed = packed.saturating_add(9).saturating_add(bytes);
        }
        let allocation = packed.saturating_add(SZ_SORTER_RECORD as usize + 7) & !7;
        let Some(rest) = available.checked_sub(allocation) else { return false };
        available = rest;
    }
    true
}

/// The level-0 PMA file contains a length prefix for the run and then the
/// length-prefixed records. Each writer starts at the preceding run's end.
struct PmaFile {
    handle: Arc<dyn VfsFile>,
    runs: Vec<u64>,
    end: u64,
    page_size: usize,
}

impl PmaFile {
    fn open(vfs: &dyn Vfs, page_size: usize) -> SqlResult<Self> {
        Ok(Self { handle: open_pma_file(vfs)?, runs: Vec::new(), end: 0, page_size })
    }

    fn prepare_run(&self, bytes: u64, mmap_limit: i32) {
        extend_pma_file(&self.handle, self.end.saturating_add(bytes).saturating_add(9), mmap_limit);
    }

    fn write_run(&mut self, bytes: u64, run: Vec<Vec<u8>>) -> SqlResult<()> {
        let start = self.end;
        let mut writer = PmaWriter::new(self.handle.as_ref(), self.page_size, start);
        writer.varint(bytes);
        for record in run {
            writer.record(&record);
        }
        self.end = writer.finish()?;
        self.runs.push(start);
        Ok(())
    }

    fn readers(&self, mmap_limit: i32) -> SqlResult<Vec<PmaReader>> {
        self.runs.iter().map(|&start| {
            let mut disk = PmaDiskReader::open(&self.handle, start, self.end, self.page_size, PmaMap::Serial(mmap_limit))?;
            let bytes = disk.varint()?;
            disk.end = disk.offset.checked_add(bytes).filter(|end| *end <= self.end).ok_or_else(pma_short_read)?;
            let mut reader = PmaReader { disk: Some(disk), ..PmaReader::default() };
            reader.refill_disk()?;
            Ok(reader)
        }).collect()
    }
}

fn open_pma_file(vfs: &dyn Vfs) -> SqlResult<Arc<dyn VfsFile>> {
    let flags = OpenFlags::TEMP_JOURNAL | OpenFlags::READWRITE | OpenFlags::CREATE
        | OpenFlags::EXCLUSIVE | OpenFlags::DELETEONCLOSE;
    let file = vfs.open(None, flags)?;
    // vdbeSorterOpenTempFile issues a hint even with sorter xFetch disabled.
    let _ = file.mmap_size(0x7fff_0000);
    Ok(Arc::from(file))
}

fn extend_pma_file(file: &Arc<dyn VfsFile>, size: u64, mmap_limit: i32) {
    if mmap_limit > 0 && size <= mmap_limit as u64 && file.supports_sorter_range() {
        let _ = file.set_sorter_chunk_size(4096);
        let _ = file.size_hint(size);
        // The preallocation probe is best effort and immediately unpins.
        let _ = Arc::clone(file).fetch_sorter_range(0, size as usize);
    }
}

struct PmaWriter<'a> {
    handle: &'a dyn VfsFile,
    offset: u64,
    buffer: Vec<u8>,
    page_size: usize,
    error: Option<SqliteError>,
    length: Vec<u8>,
}

impl<'a> PmaWriter<'a> {
    fn new(handle: &'a dyn VfsFile, page_size: usize, offset: u64) -> Self {
        Self { handle, offset, buffer: Vec::with_capacity(page_size), page_size,
            error: None, length: Vec::with_capacity(9) }
    }

    fn end(&self) -> u64 {
        self.offset + self.buffer.len() as u64
    }

    fn varint(&mut self, value: u64) {
        let mut length = std::mem::take(&mut self.length);
        length.clear();
        put_varint(&mut length, value);
        self.write(&length);
        self.length = length;
    }

    fn record(&mut self, record: &[u8]) {
        self.varint(record.len() as u64);
        self.write(record);
    }

    fn write(&mut self, mut input: &[u8]) {
        while !input.is_empty() && self.error.is_none() {
            let available = self.page_size - (self.offset % self.page_size as u64) as usize - self.buffer.len();
            let len = input.len().min(available);
            self.buffer.extend_from_slice(&input[..len]);
            input = &input[len..];
            if len == available {
                self.flush();
            }
        }
    }

    fn flush(&mut self) {
        self.error = self.handle.write_at(&self.buffer, self.offset).err();
        self.offset = self.end();
        self.buffer.clear();
    }

    fn finish(mut self) -> SqlResult<u64> {
        if self.error.is_none() && !self.buffer.is_empty() {
            self.flush();
        }
        self.error.map_or(Ok(self.offset), Err)
    }
}

fn pma_short_read() -> SqliteError {
    SqliteError::with_extended(extended::SQLITE_IOERR_SHORT_READ, ErrorCode::IoErr.as_str())
}

#[derive(Clone, Copy)]
enum PmaMap<'a> {
    Serial(i32),
    Worker(&'a AtomicI32),
}

impl<'a> PmaMap<'a> {
    fn worker(limit: Option<&'a AtomicI32>) -> Self {
        limit.map_or(Self::Serial(0), Self::Worker)
    }

    fn fetch(self, file: &Arc<dyn VfsFile>, length: u64) -> SqlResult<Option<FetchRange>> {
        match self {
            Self::Serial(limit) if limit > 0 && length <= limit as u64
                && file.supports_sorter_range() =>
            {
                Arc::clone(file).fetch_sorter_range(0, length as usize)
            }
            Self::Worker(limit) => {
                let limit = limit.load(Ordering::Relaxed);
                if limit > 0 && length <= limit as u64 {
                    return Arc::clone(file).fetch_worker_sorter_range(0, length as usize);
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

/// Buffered reads stop at the PMA boundary. The initial prefix is read
/// from the same partial page that the C reader loads on seeking.
struct PmaDiskReader {
    mapping: Option<FetchRange>,
    worker_mapping: bool,
    file: Arc<dyn VfsFile>,
    offset: u64,
    end: u64,
    buffer_start: u64,
    buffer: Vec<u8>,
    page_size: usize,
}

impl PmaDiskReader {
    fn open(file: &Arc<dyn VfsFile>, start: u64, end: u64, page_size: usize, map: PmaMap<'_>) -> SqlResult<Self> {
        let mapping = map.fetch(file, end)?;
        Ok(Self { mapping, worker_mapping: matches!(map, PmaMap::Worker(_)),
            file: Arc::clone(file), offset: start, end, buffer_start: start,
            buffer: Vec::new(), page_size })
    }

    fn read_exact(&mut self, mut output: &mut [u8]) -> SqlResult<()> {
        if output.len() as u64 > self.end - self.offset {
            return Err(pma_short_read());
        }
        if let Some(mapping) = &self.mapping {
            match mapping.copy_into(self.offset as usize, output) {
                Ok(()) => {
                    self.offset += output.len() as u64;
                    return Ok(());
                }
                Err(error) if self.worker_mapping && mapping.can_buffer_after_copy_error(&error) => {
                    self.mapping = None;
                }
                Err(error) => return Err(error),
            }
        }
        while !output.is_empty() {
            let offset = (self.offset - self.buffer_start) as usize;
            if offset >= self.buffer.len() {
                let page = self.page_size as u64;
                let count = ((self.end - self.offset).min(page - self.offset % page)) as usize;
                self.buffer.resize(count, 0);
                self.buffer_start = self.offset;
                if self.file.read_at(&mut self.buffer, self.offset)? != count {
                    return Err(pma_short_read());
                }
            }
            let start = (self.offset - self.buffer_start) as usize;
            let count = output.len().min(self.buffer.len() - start);
            output[..count].copy_from_slice(&self.buffer[start..start + count]);
            self.offset += count as u64;
            output = &mut output[count..];
        }
        Ok(())
    }

    fn varint(&mut self) -> SqlResult<u64> {
        let mut value = 0u64;
        let mut buf = [0u8];
        for _ in 0..8 {
            self.read_exact(&mut buf)?;
            value = (value << 7) | u64::from(buf[0] & 0x7f);
            if buf[0] & 0x80 == 0 {
                return Ok(value);
            }
        }
        self.read_exact(&mut buf)?;
        Ok((value << 8) | u64::from(buf[0]))
    }

    fn next(&mut self) -> SqlResult<Option<Vec<u8>>> {
        if self.offset == self.end {
            return Ok(None);
        }
        let bytes = self.varint()?;
        if bytes > self.end - self.offset {
            return Err(pma_short_read());
        }
        let bytes = usize::try_from(bytes).map_err(|_| pma_short_read())?;
        let mut record = Vec::new();
        record.try_reserve_exact(bytes).map_err(|_| SqliteError::new(ErrorCode::NoMem, ErrorCode::NoMem.as_str()))?;
        record.resize(bytes, 0);
        self.read_exact(&mut record)?;
        Ok(Some(record))
    }
}

pub(crate) type SortCallbackError = Arc<std::sync::Mutex<Option<SqliteError>>>;

#[derive(Clone)]
struct SorterComparator {
    key: Arc<KeyInfoRt>,
    error: Option<SortCallbackError>,
}

impl SorterComparator {
    fn unpack(&self, record: &[u8]) -> Vec<Mem> {
        sqlite3_vdbe_record_unpack(&self.key, record, usize::from(self.key.n_key_field))
    }

    fn compare(&self, lhs: &[u8], rhs: &[Mem]) -> SqlResult<i32> {
        let mut record = UnpackedRecord::new(&self.key, rhs, rhs.len() as u16, 0);
        let result = sqlite3_vdbe_record_compare_with_skip(lhs, &mut record, false);
        if let Some(error) = &self.error
            && let Some(error) = error.lock().unwrap().as_ref()
        {
            return Err(error.clone());
        }
        Ok(result)
    }
}

/// Single-thread incremental mergers reserve contiguous ranges of one file.
/// Ranges are allocated as the merge tree initializes, from children up.
struct PmaIntermediate<'a> {
    vfs: &'a dyn Vfs,
    file: Option<Arc<dyn VfsFile>>,
    next_offset: u64,
    page_size: usize,
    allocation: u64,
    mmap_limit: i32,
}

impl PmaIntermediate<'_> {
    fn reserve(&mut self, capacity: usize) -> SqlResult<PmaChunk> {
        let file = match &self.file {
            Some(file) => Arc::clone(file),
            None => {
                let opened = open_pma_file(self.vfs)?;
                extend_pma_file(&opened, self.allocation, self.mmap_limit);
                self.file = Some(Arc::clone(&opened));
                opened
            }
        };
        let start = self.next_offset;
        self.next_offset += capacity as u64;
        Ok(PmaChunk { file, start, page_size: self.page_size })
    }
}

#[derive(Clone)]
struct PmaChunk {
    file: Arc<dyn VfsFile>,
    start: u64,
    page_size: usize,
}

impl PmaChunk {
    fn populate(&self, child: &mut PmaMerge, key: &SorterComparator, capacity: usize, map: PmaMap<'_>) -> SqlResult<Option<u64>> {
        let mut writer = PmaWriter::new(self.file.as_ref(), self.page_size, self.start);
        // The writer retains a VFS error while the merge advances. Finish
        // still flushes a partial page after an input error and keeps the
        // input error first, as vdbeIncrPopulate does.
        let advanced = loop {
            let Some(record) = child.current() else { break Ok(()) };
            let bytes = record.len() + sqlite3_varint_len(record.len() as u64) as usize;
            if writer.end() - self.start + bytes as u64 > capacity as u64 {
                break Ok(());
            }
            writer.record(record);
            if let Err(err) = child.pop(key, capacity, map) {
                if key.error.as_ref().is_some_and(|error| error.lock().unwrap().is_some()) {
                    return Err(err);
                }
                break Err(err);
            }
        };
        let end = writer.finish();
        advanced?;
        let end = end?;
        Ok((end != self.start).then_some(end))
    }
}

/// A sorter job owns its input until join. Thread creation failure uses the
/// same inline fallback as sqlite3ThreadCreate.
enum SorterJob<T: Send + 'static> {
    Thread(std::thread::JoinHandle<T>),
    Inline(T),
}

impl<T: Send + 'static> SorterJob<T> {
    fn start(task: impl FnOnce() -> T + Send + 'static) -> Self {
        let task = Arc::new(std::sync::Mutex::new(Some(task)));
        let worker = Arc::clone(&task);
        let run = move || {
            let task = worker.lock().unwrap().take().expect("sorter job has one runner");
            task()
        };
        match std::thread::Builder::new().spawn(run) {
            Ok(thread) => Self::Thread(thread),
            Err(_) => {
                let task = task.lock().unwrap().take().expect("worker did not start");
                Self::Inline(task())
            }
        }
    }

    fn is_finished(&self) -> bool {
        match self {
            Self::Thread(thread) => thread.is_finished(),
            Self::Inline(_) => true,
        }
    }

    fn join(self) -> SqlResult<T> {
        match self {
            Self::Thread(thread) => thread.join().map_err(|_| {
                SqliteError::new(ErrorCode::Internal, "sorter worker panicked")
            }),
            Self::Inline(result) => Ok(result),
        }
    }
}

struct RunWriter {
    file: Option<PmaFile>,
    intermediate: Arc<std::sync::Mutex<Option<Arc<dyn VfsFile>>>>,
    job: Option<SorterJob<(Option<PmaFile>, SqlResult<()>)>>,
}

impl RunWriter {
    fn join(&mut self) -> SqlResult<()> {
        if let Some(job) = self.job.take() {
            let (file, result) = job.join()?;
            self.file = file;
            result?;
        }
        Ok(())
    }
}

impl Drop for RunWriter {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

struct MergeWork {
    merge: PmaMerge,
    key: SorterComparator,
    page_size: usize,
    capacity: usize,
    mmap_limit: Option<Arc<AtomicI32>>,
}

impl MergeWork {
    fn initialize(
        &mut self, vfs: &dyn Vfs, retained: &std::sync::Mutex<Option<Arc<dyn VfsFile>>>,
    ) -> SqlResult<()> {
        let mut intermediate = PmaIntermediate {
            vfs,
            file: None,
            next_offset: 0,
            page_size: self.page_size,
            allocation: 0,
            mmap_limit: 0,
        };
        let result = self.merge.initialize(&self.key, self.capacity, 0, Some(&mut intermediate));
        *retained.lock().unwrap() = intermediate.file;
        result
    }

    fn populate(&mut self, chunk: &PmaChunk) -> SqlResult<Option<u64>> {
        let map = PmaMap::worker(self.mmap_limit.as_deref());
        chunk.populate(&mut self.merge, &self.key, self.capacity, map)
    }
}

struct DoubleBuffer {
    files: [Option<PmaChunk>; 2],
    work: MergeWork,
}

impl DoubleBuffer {
    fn initialize(&mut self, vfs: &dyn Vfs) -> SqlResult<Option<u64>> {
        for file in &mut self.files {
            *file = Some(PmaChunk {
                file: open_pma_file(vfs)?,
                start: 0,
                page_size: self.work.page_size,
            });
        }
        self.populate()
    }

    fn populate(&mut self) -> SqlResult<Option<u64>> {
        self.work.populate(self.files[1].as_ref().expect("worker output is open"))
    }
}

type BufferResult = (DoubleBuffer, SqlResult<Option<u64>>);

struct ParallelReader {
    active: Option<SorterJob<BufferResult>>,
    finished: Option<BufferResult>,
}

impl ParallelReader {
    fn join(&mut self) -> SqlResult<()> {
        if let Some(active) = self.active.take() {
            self.finished = Some(active.join()?);
        }
        Ok(())
    }

    fn join_tree(&mut self) {
        let _ = self.join();
        if let Some((buffer, _)) = &mut self.finished {
            buffer.work.merge.join_workers();
        }
    }

    fn read_next_file(&mut self) -> SqlResult<Option<(PmaChunk, u64)>> {
        self.join()?;
        let Some((mut buffer, result)) = self.finished.take() else {
            return Ok(None);
        };
        let end = match result {
            Ok(end) => end,
            Err(error) => {
                self.finished = Some((buffer, Ok(None)));
                return Err(error);
            }
        };
        buffer.files.swap(0, 1);
        let Some(end) = end else {
            self.finished = Some((buffer, Ok(None)));
            return Ok(None);
        };
        let file = buffer.files[0].as_ref().expect("worker output is open").clone();
        self.active = Some(SorterJob::start(move || {
            let result = buffer.populate();
            (buffer, result)
        }));
        Ok(Some((file, end)))
    }
}

impl Drop for ParallelReader {
    fn drop(&mut self) {
        self.join_tree();
    }
}

/// `SORTER_MAX_MERGE_COUNT`
const PMA_FANOUT: usize = 16;

#[derive(Default)]
struct PmaReader {
    records: VecDeque<Vec<u8>>,
    disk: Option<PmaDiskReader>,
    parallel: Option<Box<ParallelReader>>,
    child: Option<Box<PmaMerge>>,
    chunk: Option<PmaChunk>,
}

impl PmaReader {
    fn refill_disk(&mut self) -> SqlResult<()> {
        if let Some(disk) = &mut self.disk {
            self.records.extend(disk.next()?);
        }
        Ok(())
    }

    fn refill(&mut self, key: &SorterComparator, refill_bytes: usize, map: PmaMap<'_>) -> SqlResult<()> {
        self.refill_disk()?;
        if !self.records.is_empty() {
            return Ok(());
        }
        if let Some(parallel) = &mut self.parallel {
            let next = parallel.read_next_file();
            self.disk = None;
            if let Ok(Some((chunk, end))) = next {
                self.disk = Some(PmaDiskReader::open(&chunk.file, chunk.start, end, chunk.page_size, map)?);
                return self.refill_disk();
            }
            // A failed buffer swap clears this reader before its siblings.
            self.parallel = None;
            return next.map(|_| ());
        }
        let Some(child) = &mut self.child else {
            self.disk = None;
            return Ok(());
        };
        if let Some(chunk) = &self.chunk {
            let end = chunk.populate(child, key, refill_bytes, map);
            // The old range remains usable during xWrite, then is released
            // before the seek to the freshly populated range.
            self.disk = None;
            self.disk = end?.map(|end| PmaDiskReader::open(&chunk.file, chunk.start, end,
                chunk.page_size, map)).transpose()?;
            return self.refill_disk();
        }
        let mut remaining = refill_bytes;
        // vdbeIncrPopulate writes each record then advances the input, even
        // when the record just written is the last one that fits.
        while let Some(record) = child.current() {
            let bytes = record.len() + sqlite3_varint_len(record.len() as u64) as usize;
            let Some(rest) = remaining.checked_sub(bytes) else { break };
            let Some(record) = child.pop(key, refill_bytes, map)? else { break };
            self.records.push_back(record);
            remaining = rest;
        }
        Ok(())
    }
}

/// A C `MergeEngine` reader tree. The leaves keep the original PMA order.
struct PmaMerge {
    readers: Vec<PmaReader>,
    winners: Vec<usize>,
}

impl PmaMerge {
    fn new(mut readers: Vec<PmaReader>, width: usize) -> Self {
        readers.resize_with(width, PmaReader::default);
        Self { readers, winners: vec![0; width] }
    }

    fn from_readers(mut readers: Vec<PmaReader>) -> Self {
        if readers.len() <= PMA_FANOUT {
            let width = readers.len().next_power_of_two().max(2);
            return Self::new(readers, width);
        }
        let mut leaf_level = true;
        while readers.len() > PMA_FANOUT {
            let mut parents = Vec::with_capacity(readers.len().div_ceil(PMA_FANOUT));
            let mut inputs = readers.into_iter();
            loop {
                let group: Vec<_> = inputs.by_ref().take(PMA_FANOUT).collect();
                if group.is_empty() {
                    break;
                }
                let width = if leaf_level { group.len().next_power_of_two().max(2) } else { PMA_FANOUT };
                parents.push(PmaReader {
                    child: Some(Box::new(Self::new(group, width))),
                    ..PmaReader::default()
                });
            }
            readers = parents;
            leaf_level = false;
        }
        Self::new(readers, PMA_FANOUT)
    }

    fn intermediate_readers(&self) -> u64 {
        self.readers.iter().filter_map(|reader| reader.child.as_deref())
            .map(|child| 1 + child.intermediate_readers()).sum()
    }

    fn initialize(&mut self, key: &SorterComparator, refill_bytes: usize, mmap_limit: i32,
        mut intermediate: Option<&mut PmaIntermediate<'_>>) -> SqlResult<()> {
        for reader in &mut self.readers {
            if let Some(child) = &mut reader.child {
                child.initialize(key, refill_bytes, mmap_limit, intermediate.as_deref_mut())?;
                if let Some(intermediate) = intermediate.as_deref_mut() {
                    reader.chunk = Some(intermediate.reserve(refill_bytes)?);
                }
                reader.refill(key, refill_bytes, PmaMap::Serial(mmap_limit))?;
            }
        }
        self.choose_initial(key)
    }

    fn join_workers(&mut self) {
        for reader in self.readers.iter_mut().rev() {
            if let Some(parallel) = &mut reader.parallel {
                parallel.join_tree();
            }
            if let Some(child) = &mut reader.child {
                child.join_workers();
            }
        }
    }

    fn choose_initial(&mut self, key: &SorterComparator) -> SqlResult<()> {
        // vdbeMergeEngineInit visits the nodes from the end of the tree.
        let width = self.winners.len();
        for node in (1..width).rev() {
            let left_child = 2 * node;
            let (left, right) = if left_child < width {
                (self.winners[left_child], self.winners[left_child + 1])
            } else {
                (left_child - width, left_child + 1 - width)
            };
            self.winners[node] = self.choose(key, left, right)?;
        }
        Ok(())
    }

    fn choose(&self, key: &SorterComparator, left: usize, right: usize) -> SqlResult<usize> {
        Ok(match (self.readers[left].records.front(), self.readers[right].records.front()) {
            (None, _) => right,
            (Some(_), None) => left,
            (Some(a), Some(b)) => {
                let rhs = key.unpack(b);
                let cmp = key.compare(a, &rhs)?;
                if cmp < 0 || (cmp == 0 && left < right) { left } else { right }
            }
        })
    }

    fn current(&self) -> Option<&[u8]> {
        self.readers[self.winners[1]].records.front().map(Vec::as_slice)
    }

    fn pop(&mut self, key: &SorterComparator, refill_bytes: usize, map: PmaMap<'_>) -> SqlResult<Option<Vec<u8>>> {
        let previous = self.winners[1];
        let reader = &mut self.readers[previous];
        let Some(result) = reader.records.pop_front() else { return Ok(None) };
        if reader.records.is_empty() {
            reader.refill(key, refill_bytes, map)?;
        }

        // vdbeMergeEngineStep keeps the winner in the same argument slot
        // while bringing in the sibling of each updated node.
        let mut left = previous & !1;
        let mut right = previous | 1;
        let mut node = (self.winners.len() + previous) / 2;
        loop {
            let winner = self.choose(key, left, right)?;
            self.winners[node] = winner;
            if node == 1 {
                break;
            }
            let sibling = self.winners[node ^ 1];
            if winner == left {
                right = sibling;
            } else {
                left = sibling;
            }
            node /= 2;
        }
        Ok(Some(result))
    }
}

/// `struct VdbeSorter`.
pub struct VdbeSorter {
    /// `pKeyInfo`: a copy whose `nKeyField` is the number of sort columns
    comparator: SorterComparator,
    /// `mnPmaSize`, `mxPmaSize`
    mn_pma_size: i64,
    mx_pma_size: i64,
    /// `list.aMemory != 0`
    use_a_memory: bool,
    /// `iMemory`
    i_memory: i64,
    /// `list.szPMA`
    sz_pma: i64,
    /// `list.pList` in insertion order (C's list has the newest record at its head)
    list: Vec<Vec<u8>>,
    /// The sorted runs written so far (`bUsePMA` when not empty)
    pmas: Vec<Vec<Vec<u8>>>,
    // Drop readers, then level-0 and intermediate file handles in VDBE order.
    worker_rows: Option<PmaReader>,
    pma_merge: Option<PmaMerge>,
    auxiliary_tasks: Vec<RunWriter>,
    next_worker: usize,
    worker_mmap_limit: Option<Arc<AtomicI32>>,
    pma_file: Option<PmaFile>,
    intermediate_file: Option<Arc<dyn VfsFile>>,
    vfs: Option<Arc<dyn Vfs>>,
    page_size: usize,
    mmap_limit: i32,
    /// `bUsePMA`
    b_use_pma: bool,
    /// `mxKeysize`, including the length varint.
    largest_pma_record: usize,
    refill_bytes: usize,
    /// After `sqlite3VdbeSorterRewind`: the sorted records and the position in them.
    out: Vec<Vec<u8>>,
    pos: usize,
    /// Statistics for traces: number of PMAs flushed.
    pub n_pma: u32,
}

impl std::fmt::Debug for VdbeSorter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VdbeSorter").field("n_pma", &self.n_pma)
            .field("b_use_pma", &self.b_use_pma).finish_non_exhaustive()
    }
}

impl VdbeSorter {
    pub(crate) fn set_vfs(&mut self, vfs: Option<Arc<dyn Vfs>>) {
        self.vfs = vfs;
    }

    pub(crate) fn enable_workers(&mut self, count: usize) {
        if self.use_a_memory && self.vfs.is_some() {
            // The foreground task also occupies one reader in the merge root.
            let count = count.min(PMA_FANOUT - 1);
            self.auxiliary_tasks.resize_with(count, || RunWriter {
                file: None,
                intermediate: Arc::new(std::sync::Mutex::new(None)),
                job: None,
            });
        }
    }

    pub(crate) fn set_worker_mmap_limit(&mut self, limit: Option<Arc<AtomicI32>>) {
        self.worker_mmap_limit = limit;
    }

    pub(crate) fn set_mmap_limit(&mut self, limit: i32) {
        self.mmap_limit = limit;
    }

    pub(crate) fn set_sort_callback_error(&mut self, error: Option<SortCallbackError>) {
        self.comparator.error = error;
    }

    /// `sqlite3VdbeSorterInit(db, nField, pCsr)`. `pgsz` is the page size of the main
    /// database, `cache_size` is `db->aDb[0].pSchema->cache_size`, `temp_in_memory` is
    /// `sqlite3TempInMemory(db)`.
    pub fn init(key_info: &Arc<KeyInfoRt>, n_field: i32, pgsz: i64, cache_size: i64, temp_in_memory: bool) -> VdbeSorter {
        let mut ki = (**key_info).clone();
        if n_field != 0 {
            ki.n_key_field = n_field as u16;
        }
        let mut s = VdbeSorter {
            comparator: SorterComparator { key: Arc::new(ki), error: None },
            mn_pma_size: 0,
            mx_pma_size: 0,
            use_a_memory: false,
            i_memory: 0,
            sz_pma: 0,
            list: Vec::new(),
            pmas: Vec::new(),
            worker_rows: None,
            auxiliary_tasks: Vec::new(),
            next_worker: 0,
            worker_mmap_limit: None,
            pma_file: None,
            intermediate_file: None,
            vfs: None,
            page_size: pgsz.max(1) as usize,
            mmap_limit: 0,
            b_use_pma: false,
            largest_pma_record: 0,
            pma_merge: None,
            refill_bytes: 0,
            out: Vec::new(),
            pos: 0,
            n_pma: 0,
        };
        if !temp_in_memory {
            s.mn_pma_size = SQLITE_SORTER_PMASZ * pgsz;
            let mut mx_cache = cache_size;
            if mx_cache < 0 {
                // A negative cache-size value C indicates that the cache is abs(C) KiB in size.
                mx_cache *= -1024;
            } else {
                mx_cache *= pgsz;
            }
            mx_cache = mx_cache.min(SQLITE_MAX_PMASZ);
            s.mx_pma_size = s.mn_pma_size.max(i64::from(mx_cache as i32));
            // sqlite3GlobalConfig.bSmallMalloc==0
            s.use_a_memory = true;
        }
        s
    }

    /// `vdbeSorterMerge(pTask, p1, p2)`: merges two sorted lists; on a tie the record of
    /// `p1` comes first.
    fn merge(key: &SorterComparator, p1: Vec<Vec<u8>>, p2: Vec<Vec<u8>>) -> SqlResult<Vec<Vec<u8>>> {
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(p1.len() + p2.len());
        let mut i1 = p1.into_iter().peekable();
        let mut i2 = p2.into_iter().peekable();
        let mut cached: Option<Vec<Mem>> = None;
        loop {
            let (Some(a), Some(b)) = (i1.peek(), i2.peek()) else {
                break;
            };
            if cached.is_none() {
                cached = Some(key.unpack(b));
            }
            let res = key.compare(a, cached.as_deref().unwrap_or(&[]))?;
            if res <= 0 {
                out.push(i1.next().unwrap_or_default());
            } else {
                out.push(i2.next().unwrap_or_default());
                cached = None;
            }
        }
        out.extend(i1);
        out.extend(i2);
        Ok(out)
    }

    /// `vdbeSorterSort(pTask, pList)`: sorts the record list.
    fn sort_list(&mut self) -> SqlResult<Vec<Vec<u8>>> {
        let comparator = &self.comparator;
        sort_memory_runs(std::mem::take(&mut self.list), |p, s| Self::merge(comparator, p, s))
    }

    /// `vdbeSorterFlushPMA`
    fn flush_pma(&mut self) -> SqlResult<()> {
        self.b_use_pma = true;
        if self.list.is_empty() {
            return Ok(());
        }
        let worker_count = self.auxiliary_tasks.len();
        // C starts with task 0 and skips busy tasks in round-robin order.
        for offset in 0..worker_count {
            let index = (self.next_worker + offset) % worker_count;
            let worker = &mut self.auxiliary_tasks[index];
            if worker.job.as_ref().is_some_and(|job| !job.is_finished()) {
                continue;
            }
            worker.join()?;
            self.next_worker = (index + 1) % worker_count;
            let vfs = Arc::clone(self.vfs.as_ref().expect("worker has VFS"));
            let key = self.comparator.clone();
            let page_size = self.page_size;
            let bytes = self.sz_pma as u64;
            let list = std::mem::take(&mut self.list);
            let mut file = worker.file.take();
            worker.job = Some(SorterJob::start(move || {
                let result = (|| {
                    if file.is_none() {
                        file = Some(PmaFile::open(vfs.as_ref(), page_size)?);
                    }
                    let output = file.as_mut().expect("PMA file open");
                    let sorted = sort_memory_runs(list, |left, right| Self::merge(&key, left, right))?;
                    output.write_run(bytes, sorted)
                })();
                (file, result)
            }));
            self.n_pma += 1;
            return Ok(());
        }
        if self.pma_file.is_none() && let Some(vfs) = &self.vfs {
            self.pma_file = Some(PmaFile::open(vfs.as_ref(), self.page_size)?);
        }
        if let Some(file) = &self.pma_file {
            file.prepare_run(self.sz_pma as u64, self.mmap_limit);
        }
        let run = self.sort_list()?;
        if let Some(file) = &mut self.pma_file {
            file.write_run(self.sz_pma as u64, run)?;
        } else {
            self.pmas.push(run);
        }
        self.n_pma += 1;
        Ok(())
    }

    /// `sqlite3VdbeSorterWrite(pCsr, pVal)`
    pub fn write(&mut self, val: &[u8]) {
        self.write_checked(val).expect("standalone sorter keeps PMAs in memory")
    }

    pub(crate) fn write_checked(&mut self, val: &[u8]) -> SqlResult<()> {
        // (the typeMask bookkeeping selects among comparators that give the same order)
        let n_req = val.len() as i64 + SZ_SORTER_RECORD;
        let n_pma = val.len() as i64 + sqlite3_varint_len(val.len() as u64) as i64;
        if self.mx_pma_size != 0 {
            let b_flush = if self.use_a_memory {
                self.i_memory != 0 && (self.i_memory + n_req) > self.mx_pma_size
            } else {
                self.sz_pma > self.mx_pma_size
            };
            if b_flush {
                self.flush_pma()?;
                self.sz_pma = 0;
                self.i_memory = 0;
            }
        }
        self.sz_pma += n_pma;
        self.largest_pma_record = self.largest_pma_record.max(n_pma as usize);
        if self.use_a_memory {
            // ROUND8(nReq)
            self.i_memory += (n_req + 7) & !7;
        }
        self.list.push(val.to_vec());
        Ok(())
    }

    /// `sqlite3VdbeSorterRewind(pCsr, &bEof)`: returns `bEof`.
    pub fn rewind(&mut self) -> bool {
        self.rewind_checked().expect("standalone sorter keeps PMAs in memory")
    }

    pub(crate) fn rewind_checked(&mut self) -> SqlResult<bool> {
        self.pos = 0;
        self.worker_rows = None;
        self.pma_merge = None;
        if !self.b_use_pma {
            if self.list.is_empty() {
                self.out = Vec::new();
                return Ok(true);
            }
            self.out = self.sort_list()?;
            return Ok(false);
        }
        let flush = self.flush_pma();
        self.refill_bytes = (self.mx_pma_size as usize / 2).max(self.largest_pma_record + 9);
        if !self.auxiliary_tasks.is_empty() {
            self.join_run_writers(flush)?;
            return self.rewind_workers();
        }
        flush?;
        let readers = if let Some(file) = &self.pma_file {
            file.readers(self.mmap_limit)?
        } else {
            std::mem::take(&mut self.pmas).into_iter().map(|run| PmaReader {
                records: run.into(),
                ..PmaReader::default()
            }).collect()
        };
        let mut merge = PmaMerge::from_readers(readers);
        let mut intermediate = self.vfs.as_deref().map(|vfs| PmaIntermediate {
            vfs, file: None, next_offset: 0, page_size: self.page_size,
            allocation: if self.mmap_limit > 0 { (self.refill_bytes as u64)
                .saturating_mul(merge.intermediate_readers()) } else { 0 },
            mmap_limit: self.mmap_limit,
        });
        let init = merge.initialize(&self.comparator, self.refill_bytes, self.mmap_limit, intermediate.as_mut());
        self.intermediate_file = intermediate.and_then(|files| files.file);
        init?;
        let empty = merge.current().is_none();
        self.pma_merge = Some(merge);
        self.out.clear();
        Ok(empty)
    }

    fn rewind_workers(&mut self) -> SqlResult<bool> {
        let vfs = Arc::clone(self.vfs.as_ref().expect("worker has VFS"));
        let page_size = self.page_size;
        let capacity = self.refill_bytes;
        let key = &self.comparator;
        let files = self.auxiliary_tasks.iter().map(|task| task.file.as_ref())
            .chain(std::iter::once(self.pma_file.as_ref()));
        // Load the level-zero trees in task order before starting any reader.
        let readers: Vec<PmaReader> = files.map(|file| {
            let child = file.map(|file| file.readers(0).map(PmaMerge::from_readers).map(Box::new))
                .transpose()?;
            Ok(PmaReader { child, ..PmaReader::default() })
        }).collect::<SqlResult<_>>()?;
        let width = readers.len().next_power_of_two();
        let mut root = PmaMerge::new(readers, width);

        for (reader, task) in root.readers.iter_mut().zip(&self.auxiliary_tasks) {
            let Some(merge) = reader.child.take() else { continue };
            let vfs = Arc::clone(&vfs);
            let retained = Arc::clone(&task.intermediate);
            let mut buffer = DoubleBuffer {
                files: [None, None],
                work: MergeWork {
                    merge: *merge, key: key.clone(), page_size, capacity,
                    mmap_limit: self.worker_mmap_limit.clone(),
                },
            };
            reader.parallel = Some(Box::new(ParallelReader {
                active: Some(SorterJob::start(move || {
                    let result = buffer.work.initialize(vfs.as_ref(), &retained)
                        .and_then(|_| buffer.initialize(vfs.as_ref()));
                    (buffer, result)
                })),
                finished: None,
            }));
        }

        let mut buffer = DoubleBuffer {
            files: [None, None],
            work: MergeWork {
                merge: root, key: key.clone(), page_size, capacity,
                mmap_limit: self.worker_mmap_limit.clone(),
            },
        };
        let initial = (|| {
            let root = &mut buffer.work.merge;
            let foreground = &mut root.readers[self.auxiliary_tasks.len()];
            if let Some(merge) = &mut foreground.child {
                let mut intermediate = PmaIntermediate {
                    vfs: vfs.as_ref(), file: None, next_offset: 0, page_size,
                    allocation: 0, mmap_limit: 0,
                };
                let init = merge.initialize(key, capacity, 0, Some(&mut intermediate))
                    .and_then(|_| intermediate.reserve(capacity));
                self.intermediate_file = intermediate.file;
                foreground.chunk = Some(init?);
            }
            // C warms the foreground reader before joining the reader tasks.
            for reader in root.readers.iter_mut().rev() {
                reader.refill(key, capacity, PmaMap::Serial(0))?;
            }
            root.choose_initial(key)?;
            buffer.initialize(vfs.as_ref())
        })();

        let (end, error) = match initial {
            Ok(end) => (end, None),
            Err(error) => (None, Some(error)),
        };
        let mut output = PmaReader {
            parallel: Some(Box::new(ParallelReader { active: None, finished: Some((buffer, Ok(end))) })),
            ..PmaReader::default()
        };
        let first = error.map_or_else(|| output.refill(key, capacity, PmaMap::Serial(0)), Err);
        let empty = output.records.is_empty();
        // Keep every still-live reader for the root-first cleanup path,
        // including when initial merge setup failed.
        self.worker_rows = Some(output);
        first?;
        Ok(empty)
    }

    /// `sqlite3VdbeSorterNext(db, pCsr)`: `true` is `SQLITE_OK`, `false` is `SQLITE_DONE`.
    #[inline]
    pub fn next(&mut self) -> bool {
        self.next_checked().expect("standalone sorter keeps PMAs in memory")
    }

    pub(crate) fn next_checked(&mut self) -> SqlResult<bool> {
        if let Some(reader) = &mut self.worker_rows {
            reader.records.pop_front();
            let map = PmaMap::worker(self.worker_mmap_limit.as_deref());
            reader.refill(&self.comparator, self.refill_bytes, map)?;
            return Ok(!reader.records.is_empty());
        }
        if let Some(merge) = &mut self.pma_merge {
            merge.pop(&self.comparator, self.refill_bytes, PmaMap::Serial(self.mmap_limit))?;
            return Ok(merge.current().is_some());
        }
        // (the record is released as C frees it)
        if let Some(r) = self.out.get_mut(self.pos) {
            *r = Vec::new();
        }
        self.pos += 1;
        Ok(self.pos < self.out.len())
    }

    /// `vdbeSorterRowkey`
    #[inline]
    pub fn rowkey(&self) -> &[u8] {
        if let Some(reader) = &self.worker_rows {
            return reader.records.front().map_or(&[], Vec::as_slice);
        }
        if let Some(merge) = &self.pma_merge {
            return merge.current().unwrap_or_default();
        }
        self.out.get(self.pos).map_or(&[], |v| v.as_slice())
    }

    /// `sqlite3VdbeSorterCompare(pCsr, pVal, nKeyCol, &res)`; `key_info` is the cursor's
    /// (`pCsr->pKeyInfo`).
    pub fn compare_with(&self, key_info: &KeyInfoRt, val: &[u8], n_key_col: i32) -> i32 {
        let key = self.rowkey();
        let r2 = sqlite3_vdbe_record_unpack(key_info, key, n_key_col.max(0) as usize);
        for m in r2.iter().take(n_key_col.max(0) as usize) {
            if m.flags & MEM_NULL != 0 {
                return -1;
            }
        }
        let mut r = UnpackedRecord::new(key_info, &r2, r2.len() as u16, 0);
        sqlite3_vdbe_record_compare_with_skip(val, &mut r, false)
    }

    fn join_run_writers(&mut self, mut result: SqlResult<()>) -> SqlResult<()> {
        for task in self.auxiliary_tasks.iter_mut().rev() {
            let joined = task.join();
            result = result.and(joined);
        }
        result
    }

    fn join_workers(&mut self) {
        if let Some(reader) = &mut self.worker_rows
            && let Some(parallel) = &mut reader.parallel
        {
            parallel.join_tree();
        }
        let _ = self.join_run_writers(Ok(()));
    }

    /// `sqlite3VdbeSorterReset`
    pub fn reset(&mut self) {
        self.join_workers();
        self.worker_rows = None;
        for task in &mut self.auxiliary_tasks {
            task.file = None;
            task.intermediate.lock().unwrap().take();
        }
        self.list.clear();
        self.pmas.clear();
        self.pma_merge = None;
        self.pma_file = None;
        self.intermediate_file = None;
        self.largest_pma_record = 0;
        self.refill_bytes = 0;
        self.out.clear();
        self.pos = 0;
        self.b_use_pma = false;
        self.i_memory = 0;
        self.sz_pma = 0;
    }

    /// First serial type of a record (trace aid).
    pub fn first_type(rec: &[u8]) -> u32 {
        get_varint32(rec, 1).0
    }
}

impl Drop for VdbeSorter {
    fn drop(&mut self) {
        self.join_workers();
    }
}

/// `struct RowSet` (`rowset.c`): a set of rowids with the two access patterns of the
/// machine — `OP_RowSetAdd` / `OP_RowSetRead` (insert, then extract in sorted order) and
/// `OP_RowSetTest` (batched membership test).
#[derive(Debug, Default)]
pub struct RowSet {
    /// `pEntry`: the entries inserted since the forest was last extended
    entry: Vec<i64>,
    /// `pForest`: the entries of earlier batches
    forest: BTreeSet<i64>,
    /// `iBatch`
    i_batch: i32,
    /// `ROWSET_NEXT`: `sqlite3RowSetNext` has been called; `next` is the position
    next_mode: bool,
    next: usize,
}

impl RowSet {
    /// `sqlite3RowSetInit`
    pub fn new() -> RowSet {
        RowSet::default()
    }

    /// `sqlite3RowSetInsert`
    #[inline]
    pub fn insert(&mut self, rowid: i64) {
        self.entry.push(rowid);
    }

    /// `sqlite3RowSetTest(pRowSet, iBatch, iRowid)`
    pub fn test(&mut self, i_batch: i32, rowid: i64) -> bool {
        if i_batch != self.i_batch {
            if !self.entry.is_empty() {
                for v in self.entry.drain(..) {
                    self.forest.insert(v);
                }
            }
            self.i_batch = i_batch;
        }
        self.forest.contains(&rowid)
    }

    /// `sqlite3RowSetNext(pRowSet, &rowid)`: the smallest remaining entry.
    pub fn next(&mut self) -> Option<i64> {
        if !self.next_mode {
            // rowSetEntrySort: sorted, duplicates removed
            self.entry.sort_unstable();
            self.entry.dedup();
            self.next_mode = true;
            self.next = 0;
        }
        let v = self.entry.get(self.next).copied();
        if v.is_some() {
            self.next += 1;
        } else {
            // sqlite3RowSetClear
            self.entry.clear();
            self.forest.clear();
            self.next_mode = false;
            self.next = 0;
        }
        v
    }
}

#[cfg(test)]
#[test]
fn pma_merge_calls_comparator_on_each_step() {
    use crate::vdbemem::CollRt;
    use sqliters_types::{CustomCollation, decode_record, encode_record};
    use std::sync::Mutex;

    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let collation = CollRt::from_custom(CustomCollation::new("app_order", move |left, right| {
        seen.lock().unwrap().push((left.as_bytes()[0], right.as_bytes()[0]));
        Ok(right.cmp(left))
    }));
    let key = Arc::new(KeyInfoRt {
        enc: 1,
        n_key_field: 1,
        n_all_field: 1,
        a_sort_flags: vec![0],
        a_coll: vec![Some(Arc::new(collation))],
    });
    let mut sorter = VdbeSorter::init(&key, 0, 512, 1, false);
    let padding = "x".repeat(60_000);
    for label in ["d", "a", "c", "b"] {
        sorter.write(&encode_record(&[Value::Text(format!("{label}{padding}").into())]));
    }
    let take_calls = || std::mem::take(&mut *calls.lock().unwrap());
    let current = |sorter: &VdbeSorter| {
        let row = decode_record(sorter.rowkey()).unwrap();
        let Value::Text(text) = &row[0] else { panic!("expected a text key") };
        text.as_bytes()[0]
    };
    assert_eq!(take_calls(), [(b'd', b'a')]);
    assert!(!sorter.rewind());
    assert_eq!(sorter.n_pma, 2);
    assert_eq!(current(&sorter), b'd');
    assert_eq!(take_calls(), [(b'c', b'b'), (b'd', b'c')]);

    assert!(sorter.next());
    assert_eq!(current(&sorter), b'c');
    assert_eq!(take_calls(), [(b'a', b'c')]);
    assert!(sorter.next());
    assert_eq!(current(&sorter), b'b');
    assert_eq!(take_calls(), [(b'a', b'b')]);
    assert!(sorter.next());
    assert_eq!(current(&sorter), b'a');
    assert!(take_calls().is_empty());
    assert!(!sorter.next());
    assert!(sorter.rowkey().is_empty());

    sorter.reset();
    assert!(sorter.rewind());
    assert!(sorter.rowkey().is_empty());
    assert!(take_calls().is_empty());
}

#[cfg(test)]
#[test]
fn pma_file_open_error_stops_before_sort_comparisons() {
    use crate::vdbemem::CollRt;
    use sqliters_types::{CustomCollation, encode_record};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RejectTempOpen(AtomicUsize);
    impl Vfs for RejectTempOpen {
        fn name(&self) -> &str { "reject-sorter-temp" }

        fn open(&self, path: Option<&Path>, flags: OpenFlags) -> SqlResult<Box<dyn VfsFile>> {
            assert!(path.is_none());
            for flag in [OpenFlags::TEMP_JOURNAL, OpenFlags::READWRITE, OpenFlags::CREATE,
                         OpenFlags::EXCLUSIVE, OpenFlags::DELETEONCLOSE] {
                assert!(flags.contains(flag));
            }
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(SqliteError::new(ErrorCode::CantOpen, ErrorCode::CantOpen.as_str()))
        }

        fn delete(&self, _: &Path, _: bool) -> SqlResult<()> { unreachable!() }
        fn exists(&self, _: &Path) -> SqlResult<bool> { unreachable!() }
        fn full_pathname(&self, _: &Path) -> SqlResult<PathBuf> { unreachable!() }
    }

    let callbacks = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&callbacks);
    let collation = CollRt::from_custom(CustomCollation::new("app_order", move |left, right| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(left.cmp(right))
    }));
    let key = Arc::new(KeyInfoRt {
        enc: 1,
        n_key_field: 1,
        n_all_field: 1,
        a_sort_flags: vec![0],
        a_coll: vec![Some(Arc::new(collation))],
    });
    let opens = Arc::new(RejectTempOpen(AtomicUsize::new(0)));
    let mut sorter = VdbeSorter::init(&key, 0, 512, 1, false);
    sorter.set_vfs(Some(opens.clone()));
    let padding = "x".repeat(60_000);
    for label in ["b", "a"] {
        sorter.write_checked(&encode_record(&[Value::Text(format!("{label}{padding}").into())])).unwrap();
    }
    assert_eq!(opens.0.load(Ordering::Relaxed), 0);
    let error = sorter.write_checked(&encode_record(&[Value::Text(format!("c{padding}").into())])).unwrap_err();
    assert_eq!(error.code, ErrorCode::CantOpen);
    assert_eq!(opens.0.load(Ordering::Relaxed), 1);
    assert_eq!(callbacks.load(Ordering::Relaxed), 0);
    assert_eq!(sorter.n_pma, 0);
    sorter.reset();
    assert!(sorter.rewind_checked().unwrap());
}

#[cfg(test)]
#[test]
fn pma_intermediate_open_error_after_level_zero_runs() {
    use sqliters_types::encode_record;
    use sqliters_vfs::MemoryVfs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct RejectSecondTempOpen {
        inner: MemoryVfs,
        opens: AtomicUsize,
    }

    impl Vfs for RejectSecondTempOpen {
        fn name(&self) -> &str {
            "reject-second-sorter-temp"
        }

        fn open(&self, path: Option<&Path>, flags: OpenFlags) -> SqlResult<Box<dyn VfsFile>> {
            assert!(path.is_none());
            assert!(flags.contains(OpenFlags::TEMP_JOURNAL | OpenFlags::DELETEONCLOSE));
            if self.opens.fetch_add(1, Ordering::Relaxed) == 1 {
                return Err(SqliteError::new(ErrorCode::CantOpen, ErrorCode::CantOpen.as_str()));
            }
            self.inner.open(path, flags)
        }

        fn delete(&self, path: &Path, sync_dir: bool) -> SqlResult<()> {
            self.inner.delete(path, sync_dir)
        }
        fn exists(&self, path: &Path) -> SqlResult<bool> {
            self.inner.exists(path)
        }
        fn full_pathname(&self, path: &Path) -> SqlResult<PathBuf> {
            self.inner.full_pathname(path)
        }
    }

    let key = Arc::new(KeyInfoRt {
        enc: 1,
        n_key_field: 1,
        n_all_field: 1,
        a_sort_flags: vec![0],
        a_coll: vec![None],
    });
    let mut sorter = VdbeSorter::init(&key, 0, 512, 1, false);
    let vfs = Arc::new(RejectSecondTempOpen::default());
    sorter.set_vfs(Some(vfs.clone()));
    let padding = "x".repeat(60_000);
    for row in 0..35 {
        sorter.write_checked(&encode_record(&[Value::Text(format!("{row:02}{padding}").into())])).unwrap();
    }
    assert_eq!(sorter.n_pma, 17);
    assert_eq!(vfs.opens.load(Ordering::Relaxed), 1);
    assert_eq!(sorter.rewind_checked().unwrap_err().code, ErrorCode::CantOpen);
    assert_eq!(vfs.opens.load(Ordering::Relaxed), 2);
    assert!(sorter.rowkey().is_empty());
    sorter.reset();
    assert!(sorter.rewind_checked().unwrap());
}

#[cfg(test)]
#[test]
fn worker_pma_reset_joins_reader_jobs_and_restarts() {
    use sqliters_types::{decode_record, encode_record};
    use sqliters_vfs::MemoryVfs;

    let key = Arc::new(KeyInfoRt {
        enc: 1,
        n_key_field: 2,
        n_all_field: 2,
        a_sort_flags: vec![0, 0],
        a_coll: vec![None, None],
    });
    let vfs = Arc::new(MemoryVfs::new());
    let mut sorter = VdbeSorter::init(&key, 0, 512, 1, false);
    sorter.set_vfs(Some(vfs.clone()));
    sorter.enable_workers(1);
    let payload = "x".repeat(45_000);
    for _ in 0..2 {
        for id in (1..=55).rev() {
            sorter.write_checked(&encode_record(&[
                Value::Text(payload.clone().into()),
                Value::Integer(id),
            ])).unwrap();
        }
        assert!(!sorter.rewind_checked().unwrap());
        assert!(sorter.auxiliary_tasks[0].file.is_some());
        assert!(sorter.worker_rows.as_ref().unwrap().parallel.is_some());
        assert_eq!(decode_record(sorter.rowkey()).unwrap()[1], Value::Integer(1));
        assert!(sorter.next_checked().unwrap());
        assert_eq!(decode_record(sorter.rowkey()).unwrap()[1], Value::Integer(2));
        sorter.reset();
        assert_eq!(Arc::strong_count(&vfs), 2);
        assert!(sorter.rewind_checked().unwrap());
    }
}

#[cfg(all(test, unix))]
#[test]
fn worker_pma_late_anon_mapping_releases_range_on_reset() {
    use sqliters_types::{decode_record, encode_record};
    use sqliters_vfs::OsVfs;

    let vfs = Arc::new(OsVfs::default());
    let probe = open_pma_file(vfs.as_ref()).unwrap();
    if !probe.supports_fetch_range() {
        return; // The platform's temporary files use the named fallback.
    }
    drop(probe);

    let key = Arc::new(KeyInfoRt {
        enc: 1,
        n_key_field: 2,
        n_all_field: 2,
        a_sort_flags: vec![0, 0],
        a_coll: vec![None, None],
    });
    let limit = Arc::new(AtomicI32::new(0));
    let mut sorter = VdbeSorter::init(&key, 0, 512, 1, false);
    sorter.set_vfs(Some(vfs.clone()));
    sorter.enable_workers(1);
    sorter.set_worker_mmap_limit(Some(Arc::clone(&limit)));
    let padding = "x".repeat(8190);
    for id in (1..=53).rev() {
        sorter.write_checked(&encode_record(&[
            Value::Text(format!("{id:03}{padding}").into()),
            Value::Integer(id),
        ])).unwrap();
    }
    assert!(!sorter.rewind_checked().unwrap());
    assert_eq!(decode_record(sorter.rowkey()).unwrap()[1], Value::Integer(1));
    let is_mapped = |sorter: &VdbeSorter| {
        sorter.worker_rows.as_ref().unwrap().disk.as_ref().unwrap().mapping.is_some()
    };
    assert!(!is_mapped(&sorter));
    limit.store(i32::MAX, Ordering::Relaxed);
    for id in 2..=53 {
        assert!(sorter.next_checked().unwrap());
        assert_eq!(decode_record(sorter.rowkey()).unwrap()[1], Value::Integer(id));
        if is_mapped(&sorter) {
            limit.store(0, Ordering::Relaxed);
            assert!(is_mapped(&sorter));
            sorter.reset();
            assert_eq!(Arc::strong_count(&vfs), 2);
            assert!(sorter.rewind_checked().unwrap());
            return;
        }
    }
    panic!("short final PMA batch should fetch an anonymous range");
}
