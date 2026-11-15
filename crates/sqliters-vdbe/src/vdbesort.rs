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
//! What is different: a PMA ("packed memory array") is not written to a temporary file; it
//! is kept as a sorted run in memory, and the runs are merged pairwise when the sorter is
//! rewound instead of through `MergeEngine` / `IncrMerger` trees. Both merges take the
//! record of the earlier run on a tie (`vdbeMergeEngineCompare`: "iRes<0 || (iRes==0 &&
//! pReadr1<pReadr2)"), so the output order — including the order of equal keys — is C's.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::vdbeaux_rt::{
    KeyInfoRt, UnpackedRecord, get_varint32, sqlite3_varint_len, sqlite3_vdbe_record_compare_with_skip,
    sqlite3_vdbe_record_unpack,
};
use crate::vdbemem::{MEM_NULL, Mem};

/// `SQLITE_MAX_PMASZ`
const SQLITE_MAX_PMASZ: i64 = 1 << 29;
/// `sqlite3GlobalConfig.szPma` (`SQLITE_SORTER_PMASZ`)
const SQLITE_SORTER_PMASZ: i64 = 250;
/// `sizeof(SorterRecord)` on the reference platform (int + pointer union, 8-byte aligned)
const SZ_SORTER_RECORD: i64 = 16;

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
        let mut a_slot: Vec<Option<Vec<Vec<u8>>>> = (0..64).map(|_| None).collect();
        // C walks the list from its head: the newest record first.
        let list = std::mem::take(&mut self.list);
        for rec in list.into_iter().rev() {
            let mut p = vec![rec];
            let mut i = 0;
            while let Some(s) = a_slot[i].take() {
                p = Self::merge(&key_info, p, s);
                i += 1;
            }
            a_slot[i] = Some(p);
        }
        let mut p: Option<Vec<Vec<u8>>> = None;
        for slot in &mut a_slot {
            let Some(s) = slot.take() else {
                continue;
            };
            p = Some(match p {
                Some(p) => Self::merge(&key_info, p, s),
                None => s,
            });
        }
        p.unwrap_or_default()
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
        if self.use_a_memory {
            // ROUND8(nReq)
            self.i_memory += (n_req + 7) & !7;
        }
        self.list.push(val.to_vec());
    }

    /// `sqlite3VdbeSorterRewind(pCsr, &bEof)`: returns `bEof`.
    pub fn rewind(&mut self) -> bool {
        self.pos = 0;
        if !self.b_use_pma {
            if self.list.is_empty() {
                self.out = Vec::new();
                return true;
            }
            self.out = self.sort_list();
            return false;
        }
        // Write the current in-memory list to a PMA, then merge all PMAs: the earlier PMA
        // wins a tie.
        self.flush_pma();
        let key_info = Arc::clone(&self.key_info);
        let mut runs = std::mem::take(&mut self.pmas);
        while runs.len() > 1 {
            let mut next: Vec<Vec<Vec<u8>>> = Vec::with_capacity(runs.len().div_ceil(2));
            let mut it = runs.into_iter();
            while let Some(a) = it.next() {
                match it.next() {
                    Some(b) => next.push(Self::merge(&key_info, a, b)),
                    None => next.push(a),
                }
            }
            runs = next;
        }
        self.out = runs.pop().unwrap_or_default();
        self.out.is_empty()
    }

    /// `sqlite3VdbeSorterNext(db, pCsr)`: `true` is `SQLITE_OK`, `false` is `SQLITE_DONE`.
    #[inline]
    pub fn next(&mut self) -> bool {
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
