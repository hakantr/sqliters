//! `vdbesort.c`: the sorter of `OP_SorterOpen` … `OP_SorterNext` (and `rowset.c`: `RowSet`).
//!
//! Port of SQLite 3.54.1 as the reference build runs it (no worker threads). What is ported
//! one to one: the record list (new records at the head), the flush decision of
//! `sqlite3VdbeSorterWrite` (`mxPmaSize` / `mnPmaSize` from the page size and the cache size
//! of the main database, `list.aMemory` accounting with `sizeof(SorterRecord)`), the list
//! sort `vdbeSorterSort` (64 slots, `vdbeSorterMerge` with "res<=0 takes p1"), the key
//! comparison (`vdbeSorterCompare` — the specialised integer / text / real comparators give
//! the same order as the general one for the records they are chosen for, so the general
//! comparator is used), `sqlite3VdbeSorterCompare`, `sqlite3VdbeSorterRowkey`.
//!
//! PMA runs are held in memory. Their merge uses C's reader tree and
//! bounded intermediate batches. This preserves comparison order across
//! `SorterSort` and `SorterNext` while keeping the current storage backend.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use sqliters_types::{TextEncoding, Value};

use crate::vdbeaux_rt::{
    KeyInfoRt, UnpackedRecord, get_varint32, sqlite3_varint_len, sqlite3_vdbe_record_compare_with_skip,
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

/// `SORTER_MAX_MERGE_COUNT`
const PMA_FANOUT: usize = 16;

#[derive(Debug, Default)]
struct PmaReader {
    records: VecDeque<Vec<u8>>,
    child: Option<Box<PmaMerge>>,
}

impl PmaReader {
    fn refill(&mut self, key: &KeyInfoRt, refill_bytes: usize) {
        let Some(child) = &mut self.child else { return };
        let mut remaining = refill_bytes;
        // vdbeIncrPopulate writes each record then advances the input, even
        // when the record just written is the last one that fits.
        while let Some(record) = child.current() {
            let bytes = record.len() + sqlite3_varint_len(record.len() as u64) as usize;
            let Some(rest) = remaining.checked_sub(bytes) else { break };
            let Some(record) = child.pop(key, refill_bytes) else { break };
            self.records.push_back(record);
            remaining = rest;
        }
    }
}

/// A C `MergeEngine` reader tree. The leaves keep the original PMA order.
#[derive(Debug)]
struct PmaMerge {
    readers: Vec<PmaReader>,
    winners: Vec<usize>,
}

impl PmaMerge {
    fn new(mut readers: Vec<PmaReader>, width: usize) -> Self {
        readers.resize_with(width, PmaReader::default);
        Self { readers, winners: vec![0; width] }
    }

    fn from_runs(runs: Vec<Vec<Vec<u8>>>) -> Self {
        let mut readers: Vec<_> = runs.into_iter().map(|run| PmaReader {
            records: run.into(),
            child: None,
        }).collect();
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
                    records: VecDeque::new(),
                    child: Some(Box::new(Self::new(group, width))),
                });
            }
            readers = parents;
            leaf_level = false;
        }
        Self::new(readers, PMA_FANOUT)
    }

    fn initialize(&mut self, key: &KeyInfoRt, refill_bytes: usize) {
        for reader in &mut self.readers {
            if let Some(child) = &mut reader.child {
                child.initialize(key, refill_bytes);
                reader.refill(key, refill_bytes);
            }
        }
        // vdbeMergeEngineInit visits the nodes from the end of the tree.
        let width = self.winners.len();
        for node in (1..width).rev() {
            let left_child = 2 * node;
            let (left, right) = if left_child < width {
                (self.winners[left_child], self.winners[left_child + 1])
            } else {
                (left_child - width, left_child + 1 - width)
            };
            self.winners[node] = self.choose(key, left, right);
        }
    }

    fn choose(&self, key: &KeyInfoRt, left: usize, right: usize) -> usize {
        match (self.readers[left].records.front(), self.readers[right].records.front()) {
            (None, _) => right,
            (Some(_), None) => left,
            (Some(a), Some(b)) => {
                let rhs = sqlite3_vdbe_record_unpack(key, b, usize::from(key.n_key_field));
                let cmp = VdbeSorter::compare(key, a, &rhs);
                if cmp < 0 || (cmp == 0 && left < right) { left } else { right }
            }
        }
    }

    fn current(&self) -> Option<&[u8]> {
        self.readers[self.winners[1]].records.front().map(Vec::as_slice)
    }

    fn pop(&mut self, key: &KeyInfoRt, refill_bytes: usize) -> Option<Vec<u8>> {
        let previous = self.winners[1];
        let reader = &mut self.readers[previous];
        let result = reader.records.pop_front()?;
        if reader.records.is_empty() {
            reader.refill(key, refill_bytes);
        }

        // vdbeMergeEngineStep keeps the winner in the same argument slot
        // while bringing in the sibling of each updated node.
        let mut left = previous & !1;
        let mut right = previous | 1;
        let mut node = (self.winners.len() + previous) / 2;
        loop {
            let winner = self.choose(key, left, right);
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
        Some(result)
    }
}

/// `struct VdbeSorter`.
#[derive(Debug)]
pub struct VdbeSorter {
    /// `pKeyInfo`: a copy whose `nKeyField` is the number of sort columns
    key_info: Arc<KeyInfoRt>,
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
    /// `bUsePMA`
    b_use_pma: bool,
    /// `mxKeysize`, including the length varint.
    largest_pma_record: usize,
    pma_merge: Option<PmaMerge>,
    refill_bytes: usize,
    /// After `sqlite3VdbeSorterRewind`: the sorted records and the position in them.
    out: Vec<Vec<u8>>,
    pos: usize,
    /// Statistics for traces: number of PMAs flushed.
    pub n_pma: u32,
}

impl VdbeSorter {
    /// `sqlite3VdbeSorterInit(db, nField, pCsr)`. `pgsz` is the page size of the main
    /// database, `cache_size` is `db->aDb[0].pSchema->cache_size`, `temp_in_memory` is
    /// `sqlite3TempInMemory(db)`.
    pub fn init(key_info: &Arc<KeyInfoRt>, n_field: i32, pgsz: i64, cache_size: i64, temp_in_memory: bool) -> VdbeSorter {
        let mut ki = (**key_info).clone();
        if n_field != 0 {
            ki.n_key_field = n_field as u16;
        }
        let mut s = VdbeSorter {
            key_info: Arc::new(ki),
            mn_pma_size: 0,
            mx_pma_size: 0,
            use_a_memory: false,
            i_memory: 0,
            sz_pma: 0,
            list: Vec::new(),
            pmas: Vec::new(),
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

    /// `vdbeSorterCompare(pTask, &bCached, pKey1, nKey1, pKey2, nKey2)` with `r2` = the
    /// unpacked `pKey2`.
    #[inline]
    fn compare(key_info: &KeyInfoRt, key1: &[u8], r2: &[Mem]) -> i32 {
        let mut r = UnpackedRecord::new(key_info, r2, r2.len() as u16, 0);
        sqlite3_vdbe_record_compare_with_skip(key1, &mut r, false)
    }

    /// `vdbeSorterMerge(pTask, p1, p2)`: merges two sorted lists; on a tie the record of
    /// `p1` comes first.
    fn merge(key_info: &KeyInfoRt, p1: Vec<Vec<u8>>, p2: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let n_field = usize::from(key_info.n_key_field);
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(p1.len() + p2.len());
        let mut i1 = p1.into_iter().peekable();
        let mut i2 = p2.into_iter().peekable();
        let mut cached: Option<Vec<Mem>> = None;
        loop {
            let (Some(a), Some(b)) = (i1.peek(), i2.peek()) else {
                break;
            };
            if cached.is_none() {
                cached = Some(sqlite3_vdbe_record_unpack(key_info, b, n_field));
            }
            let res = Self::compare(key_info, a, cached.as_deref().unwrap_or(&[]));
            if res <= 0 {
                out.push(i1.next().unwrap_or_default());
            } else {
                out.push(i2.next().unwrap_or_default());
                cached = None;
            }
        }
        out.extend(i1);
        out.extend(i2);
        out
    }

    /// `vdbeSorterSort(pTask, pList)`: sorts the record list.
    fn sort_list(&mut self) -> Vec<Vec<u8>> {
        let key_info = Arc::clone(&self.key_info);
        sort_memory_runs(std::mem::take(&mut self.list), |p, s| {
            Ok::<_, std::convert::Infallible>(Self::merge(&key_info, p, s))
        }).unwrap_or_else(|never| match never {})
    }

    /// `vdbeSorterFlushPMA`
    fn flush_pma(&mut self) {
        self.b_use_pma = true;
        if !self.list.is_empty() {
            let run = self.sort_list();
            self.pmas.push(run);
            self.n_pma += 1;
        }
    }

    /// `sqlite3VdbeSorterWrite(pCsr, pVal)`
    pub fn write(&mut self, val: &[u8]) {
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
                self.flush_pma();
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
    }

    /// `sqlite3VdbeSorterRewind(pCsr, &bEof)`: returns `bEof`.
    pub fn rewind(&mut self) -> bool {
        self.pos = 0;
        self.pma_merge = None;
        if !self.b_use_pma {
            if self.list.is_empty() {
                self.out = Vec::new();
                return true;
            }
            self.out = self.sort_list();
            return false;
        }
        self.flush_pma();
        self.refill_bytes = (self.mx_pma_size as usize / 2).max(self.largest_pma_record + 9);
        let mut merge = PmaMerge::from_runs(std::mem::take(&mut self.pmas));
        merge.initialize(&self.key_info, self.refill_bytes);
        let empty = merge.current().is_none();
        self.pma_merge = Some(merge);
        self.out.clear();
        empty
    }

    /// `sqlite3VdbeSorterNext(db, pCsr)`: `true` is `SQLITE_OK`, `false` is `SQLITE_DONE`.
    #[inline]
    pub fn next(&mut self) -> bool {
        if let Some(merge) = &mut self.pma_merge {
            merge.pop(&self.key_info, self.refill_bytes);
            return merge.current().is_some();
        }
        // (the record is released as C frees it)
        if let Some(r) = self.out.get_mut(self.pos) {
            *r = Vec::new();
        }
        self.pos += 1;
        self.pos < self.out.len()
    }

    /// `vdbeSorterRowkey`
    #[inline]
    pub fn rowkey(&self) -> &[u8] {
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

    /// `sqlite3VdbeSorterReset`
    pub fn reset(&mut self) {
        self.list.clear();
        self.pmas.clear();
        self.pma_merge = None;
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
