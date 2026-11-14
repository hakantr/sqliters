//! The page cache: a port of the bookkeeping of `pcache.c` (`struct PCache`, `PgHdr`) and
//! `pcache1.c` (`struct PCache1` with its private `PGroup`) of SQLite 3.54.1.
//!
//! What C decides from this bookkeeping is observable — which dirty page is written in the
//! middle of a transaction (`sqlite3PcacheFetchStress`), which clean page is thrown away
//! (`pcache1FetchStage2`, `pcache1Unpin`, `pcache1EnforceMaxPage`) — so every list and counter
//! is kept exactly as C keeps it:
//!
//! * per page: `PgHdr.flags` (`PGHDR_CLEAN/DIRTY/WRITEABLE/NEED_SYNC/DONT_WRITE`), `nRef`,
//! * the dirty list (`pDirty` .. `pDirtyTail`, `PCACHE_DIRTYLIST_ADD/REMOVE/FRONT`) and `pSynced`,
//! * `eCreate`, `szCache`, `szSpill`, `nRefSum`,
//! * `nPage`, `nRecyclable`, `nMax`, `n90pct`, `mxPinned`, the LRU list of unpinned pages.
//!
//! Page content stays addressed by page number (the b-tree layer reads and writes pages by
//! number). The identity of a C `PgHdr` object — its reference count and its place in the dirty
//! list / LRU list — lives in a separate record ([`PgIdent`]) so that `sqlite3PagerRekey` (the
//! page renumbering of `balance_nonroot`) can move identities between page numbers while the
//! content written for a page number stays under that number.

use std::collections::HashMap;

/// Decision trace of this cache (class `pc`), only for caches with a trace id (named database
/// files, as in the instrumented C build) and only in `--cfg sqliters_dtrace` builds.
macro_rules! pctr {
    ($c:expr, $($arg:tt)*) => {
        #[allow(unexpected_cfgs)]
        {
            #[cfg(sqliters_dtrace)]
            {
                if $c.tr_id > 0 && sqliters_types::dtrace_enabled() {
                    eprintln!($($arg)*);
                }
            }
        }
    };
}
pub(crate) use pctr;

/// Class `pcx` of the decision trace (every page reference taken / given back): only with
/// `DTRACE_PCX` set.
#[allow(dead_code)]
pub(crate) fn pcx_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DTRACE_PCX").is_some())
}

/// FNV-1a 32 of a page image (trace: what was written where).
#[allow(dead_code)]
pub(crate) fn page_hash(d: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in d {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h
}

use sqliters_types::Pgno;

pub(crate) const PGHDR_CLEAN: u16 = 0x001;
pub(crate) const PGHDR_DIRTY: u16 = 0x002;
pub(crate) const PGHDR_WRITEABLE: u16 = 0x004;
pub(crate) const PGHDR_NEED_SYNC: u16 = 0x008;
pub(crate) const PGHDR_DONT_WRITE: u16 = 0x010;

/// `ROUND8(sizeof(MemPage))` of the reference build: the `szExtra` the b-tree layer passes to
/// `sqlite3PagerOpen`, used by `numberOfCachePages` (`pcache.c`) for a negative `cache_size`.
pub(crate) const PCACHE_SZ_EXTRA: i64 = 136;

const MAX_DENSE_PGNO: Pgno = 1_048_576;
pub(crate) const NIL: u32 = u32::MAX;

#[derive(Default, Clone, Copy)]
pub(crate) struct MulXorHasher(u64);

impl std::hash::Hasher for MulXorHasher {
    #[inline(always)]
    fn finish(&self) -> u64 {
        let h = self.0;
        h ^ (h >> 32)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
        self.0 = h;
    }

    #[inline(always)]
    fn write_u32(&mut self, i: u32) {
        self.0 = u64::from(i).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    #[inline(always)]
    fn write_u64(&mut self, i: u64) {
        self.0 = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

pub(crate) type MulXorBuildHasher = std::hash::BuildHasherDefault<MulXorHasher>;
pub(crate) type FastPgnoMap<V> = HashMap<Pgno, V, MulXorBuildHasher>;

/// The content of one cached page and the part of `PgHdr` that belongs to the page number.
pub(crate) struct CachedPage {
    pub(crate) pgno: Pgno,
    pub(crate) data: Vec<u8>,
    /// `PgHdr.flags`.
    pub(crate) flags: u16,
    pub(crate) occupied: bool,
    /// Index of the [`PgIdent`] that currently carries this page number.
    ident: u32,
}

/// The identity of a `PgHdr` / `PgHdr1` object: reference count and list links.
#[derive(Clone, Copy)]
struct PgIdent {
    /// Slot (index into `entries`) of the page this identity belongs to.
    slot: u32,
    /// `PgHdr.nRef`.
    n_ref: i32,
    /// `PgHdr.pDirtyPrev` (towards `pDirty`, the head) / `pDirtyNext` (towards `pDirtyTail`).
    d_prev: u32,
    d_next: u32,
    /// `PgHdr1.pLruPrev` (towards the most recently unpinned) / `pLruNext`.
    l_prev: u32,
    l_next: u32,
    /// `PAGE_IS_UNPINNED`: the page is on the LRU list.
    in_lru: bool,
}

impl PgIdent {
    const fn new(slot: u32) -> Self {
        Self {
            slot,
            n_ref: 0,
            d_prev: NIL,
            d_next: NIL,
            l_prev: NIL,
            l_next: NIL,
            in_lru: false,
        }
    }
}

/// Outcome of `pcache1FetchStage2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage2 {
    /// "Abort if createFlag is 1 but the cache is nearly full": the caller goes on to
    /// `sqlite3PcacheFetchStress`.
    Refused,
    /// A page slot for the key (recycled from the LRU tail, or newly allocated).
    Slot(u32),
}

pub(crate) struct PageCache {
    // ---- storage, addressed by page number
    dense_slots: Vec<u32>,
    sparse_slots: FastPgnoMap<u32>,
    pub(crate) entries: Vec<CachedPage>,
    idents: Vec<PgIdent>,
    free_slots: Vec<u32>,
    /// `PCache1.iMaxKey`.
    max_cached_pgno: Pgno,

    // ---- pcache.c: struct PCache
    /// `pDirty` (most recently added / released), `pDirtyTail`, `pSynced` — identity indices.
    p_dirty: u32,
    p_dirty_tail: u32,
    p_synced: u32,
    /// `nRefSum`.
    pub(crate) n_ref_sum: i64,
    /// `szCache` (the configured value: pages if positive, -KiB if negative) and `szSpill`.
    sz_cache: i32,
    sz_spill: i32,
    /// `szPage`.
    sz_page: u32,
    /// `bPurgeable`: false for an in-memory database (nothing is ever unpinned or recycled).
    b_purgeable: bool,
    /// `eCreate`: 2 while the dirty list is empty (or the cache is not purgeable), else 1.
    e_create: u8,
    n_dirty: u32,

    // ---- pcache1.c: struct PCache1 with its own PGroup (`separateCache`)
    /// `nMax`, `n90pct`, `PGroup.mxPinned`, `nMin`.
    n_max: u32,
    n90pct: u32,
    mx_pinned: u32,
    n_min: u32,
    /// `nPage`, `nRecyclable`.
    n_page: u32,
    n_recyclable: u32,
    /// LRU list of unpinned pages: `lru.pLruNext` (most recent) .. `lru.pLruPrev` (victim).
    lru_head: u32,
    lru_tail: u32,
    /// Trace id of the cache (C: order of opening among named database files; 0 = not traced).
    #[allow(dead_code)]
    pub(crate) tr_id: i32,
}

impl PageCache {
    /// `sqlite3PcacheOpen` + `pcache1Create` + `sqlite3PcacheSetCachesize(100)`.
    pub(crate) fn new(sz_page: u32, purgeable: bool) -> Self {
        let mut c = Self {
            dense_slots: Vec::new(),
            sparse_slots: FastPgnoMap::default(),
            entries: Vec::new(),
            idents: Vec::new(),
            free_slots: Vec::new(),
            max_cached_pgno: 0,
            p_dirty: NIL,
            p_dirty_tail: NIL,
            p_synced: NIL,
            n_ref_sum: 0,
            sz_cache: 100,
            sz_spill: 1,
            sz_page,
            b_purgeable: purgeable,
            e_create: 2,
            n_dirty: 0,
            n_max: 0,
            n90pct: 0,
            mx_pinned: 10,
            n_min: if purgeable { 10 } else { 0 },
            n_page: 0,
            n_recyclable: 0,
            lru_head: NIL,
            lru_tail: NIL,
            tr_id: 0,
        };
        let n = c.number_of_cache_pages();
        c.pcache1_cachesize(n);
        c
    }

    // ------------------------------------------------------------------ lookups

    #[inline(always)]
    pub(crate) fn slot_of(&self, pgno: Pgno) -> Option<u32> {
        if pgno <= MAX_DENSE_PGNO {
            let s = *self.dense_slots.get(pgno as usize)?;
            if s != NIL { Some(s) } else { None }
        } else {
            self.sparse_slots.get(&pgno).copied()
        }
    }

    #[inline(always)]
    pub(crate) fn entry_data(&self, slot: u32) -> &[u8] {
        self.entries[slot as usize].data.as_slice()
    }

    #[inline(always)]
    pub(crate) fn entry_data_mut(&mut self, slot: u32) -> &mut [u8] {
        self.entries[slot as usize].data.as_mut_slice()
    }

    #[inline(always)]
    pub(crate) fn flags(&self, slot: u32) -> u16 {
        self.entries[slot as usize].flags
    }

    #[inline(always)]
    pub(crate) fn flags_mut(&mut self, slot: u32) -> &mut u16 {
        &mut self.entries[slot as usize].flags
    }

    #[inline(always)]
    pub(crate) fn pgno_of(&self, slot: u32) -> Pgno {
        self.entries[slot as usize].pgno
    }

    #[inline(always)]
    pub(crate) fn n_ref(&self, slot: u32) -> i32 {
        self.idents[self.entries[slot as usize].ident as usize].n_ref
    }

    #[inline(always)]
    pub(crate) fn is_dirty(&self, pgno: Pgno) -> bool {
        self.slot_of(pgno)
            .is_some_and(|slot| self.entries[slot as usize].flags & PGHDR_DIRTY != 0)
    }

    #[inline(always)]
    pub(crate) fn get_data(&self, pgno: Pgno) -> Option<&[u8]> {
        let slot = self.slot_of(pgno)?;
        Some(self.entries[slot as usize].data.as_slice())
    }

    #[inline(always)]
    pub(crate) fn get_data_mut(&mut self, pgno: Pgno) -> Option<&mut [u8]> {
        let slot = self.slot_of(pgno)?;
        Some(self.entries[slot as usize].data.as_mut_slice())
    }

    /// `sqlite3PcachePagecount` (`pcache1Pagecount`).
    #[inline(always)]
    pub(crate) fn pagecount(&self) -> u32 {
        self.n_page
    }

    #[inline(always)]
    pub(crate) fn sz_spill(&self) -> i32 {
        self.sz_spill
    }

    #[inline(always)]
    pub(crate) fn sz_cache(&self) -> i32 {
        self.sz_cache
    }

    #[inline(always)]
    pub(crate) fn e_create(&self) -> u8 {
        self.e_create
    }

    #[inline(always)]
    pub(crate) fn any_dirty(&self) -> bool {
        self.p_dirty != NIL
    }

    /// Whether a dirty page with a number up to `max_pgno` (other than `skip`) exists.
    pub(crate) fn any_dirty_upto(&self, max_pgno: Pgno, skip: Pgno) -> bool {
        let mut id = self.p_dirty;
        while id != NIL {
            let i = &self.idents[id as usize];
            let p = self.entries[i.slot as usize].pgno;
            if p <= max_pgno && p != skip {
                return true;
            }
            id = i.d_next;
        }
        false
    }

    // ------------------------------------------------------------------ hash (pgno -> slot)

    fn hash_set(&mut self, pgno: Pgno, slot: u32) {
        if pgno <= MAX_DENSE_PGNO {
            let idx = pgno as usize;
            if idx >= self.dense_slots.len() {
                self.dense_slots.resize(idx + 1, NIL);
            }
            self.dense_slots[idx] = slot;
        } else {
            self.sparse_slots.insert(pgno, slot);
        }
        if pgno > self.max_cached_pgno {
            self.max_cached_pgno = pgno;
        }
    }

    fn hash_clear(&mut self, pgno: Pgno) {
        if pgno <= MAX_DENSE_PGNO {
            if let Some(cell) = self.dense_slots.get_mut(pgno as usize) {
                *cell = NIL;
            }
        } else {
            self.sparse_slots.remove(&pgno);
        }
    }

    // ------------------------------------------------------------------ LRU (pcache1.c)

    /// `pcache1PinPage`: take the page off the LRU list.
    fn pcache1_pin_page(&mut self, id: u32) {
        let (prev, next) = {
            let i = &self.idents[id as usize];
            debug_assert!(i.in_lru);
            (i.l_prev, i.l_next)
        };
        if prev != NIL {
            self.idents[prev as usize].l_next = next;
        } else {
            self.lru_head = next;
        }
        if next != NIL {
            self.idents[next as usize].l_prev = prev;
        } else {
            self.lru_tail = prev;
        }
        let i = &mut self.idents[id as usize];
        i.l_prev = NIL;
        i.l_next = NIL;
        i.in_lru = false;
        self.n_recyclable -= 1;
    }

    /// `pcache1RemoveFromHash(pPage, freeFlag=1)`: the page leaves the cache and its buffer
    /// is freed. The page must not be on the LRU list.
    fn pcache1_remove_from_hash_free(&mut self, slot: u32) {
        let pgno = self.entries[slot as usize].pgno;
        self.hash_clear(pgno);
        self.n_page -= 1;
        let e = &mut self.entries[slot as usize];
        e.occupied = false;
        e.flags = 0;
        e.data = Vec::new();
        let id = e.ident;
        self.idents[id as usize] = PgIdent::new(slot);
        self.free_slots.push(slot);
    }

    /// `pcache1EnforceMaxPage`: "discard unpinned pages from the LRU list until the number of
    /// purgeable pages is at or below `nMaxPage`". `max_page` is `PGroup.nMaxPage` (0 for
    /// `pcache1Shrink`).
    fn pcache1_enforce_max_page(&mut self, max_page: u32) {
        while self.n_page > max_page && self.lru_tail != NIL {
            let id = self.lru_tail;
            let slot = self.idents[id as usize].slot;
            pctr!(
                self,
                "TR|pc|purge|c={} pg={}",
                self.tr_id,
                self.entries[slot as usize].pgno
            );
            self.pcache1_pin_page(id);
            self.pcache1_remove_from_hash_free(slot);
        }
    }

    /// `pcache1Cachesize`.
    fn pcache1_cachesize(&mut self, n_max: i32) {
        if self.b_purgeable {
            // (`n = (u32)nMax`, clamped to 0x7fff0000 with a private group)
            let mut n = n_max as u32;
            if n > 0x7fff_0000 {
                n = 0x7fff_0000;
            }
            // `pGroup->mxPinned = pGroup->nMaxPage + 10 - pGroup->nMinPage`
            self.mx_pinned = n.wrapping_add(10).wrapping_sub(self.n_min);
            self.n_max = n;
            self.n90pct = (u64::from(n) * 9 / 10) as u32;
            pctr!(
                self,
                "TR|pc|nmax|c={} nmax={} n90={} mxpin={} npage={}",
                self.tr_id,
                self.n_max,
                self.n90pct,
                self.mx_pinned,
                self.n_page
            );
            self.pcache1_enforce_max_page(n);
        }
    }

    /// `pcache1Shrink` (`PRAGMA shrink_memory`, `sqlite3_db_release_memory`).
    pub(crate) fn shrink(&mut self) {
        if self.b_purgeable {
            self.pcache1_enforce_max_page(0);
        }
    }

    /// FNV-1a over the LRU list from the most recently unpinned page to the victim (trace).
    #[allow(dead_code)]
    fn lru_hash(&self) -> (u32, u32) {
        let mut h: u32 = 0x811c_9dc5;
        let mut n = 0u32;
        let mut id = self.lru_head;
        while id != NIL {
            let slot = self.idents[id as usize].slot;
            h = (h ^ self.entries[slot as usize].pgno).wrapping_mul(0x0100_0193);
            n += 1;
            id = self.idents[id as usize].l_next;
        }
        (h, n)
    }

    /// `pcache1FetchStage2(pCache, iKey, createFlag)`: the page is not in the cache.
    /// `spare` is a buffer to use for a newly allocated page (page size bytes).
    #[allow(unexpected_cfgs)]
    pub(crate) fn pcache1_fetch_stage2(&mut self, pgno: Pgno, create_flag: u8) -> Stage2 {
        // Step 3: "Abort if createFlag is 1 but the cache is nearly full".
        let n_pinned = self.n_page - self.n_recyclable;
        if create_flag == 1 && (n_pinned >= self.mx_pinned || n_pinned >= self.n90pct) {
            pctr!(
                self,
                "TR|pc|miss|c={} pg={} cf={} npage={} nrec={} nmax={} o=refuse",
                self.tr_id,
                pgno,
                create_flag,
                self.n_page,
                self.n_recyclable,
                self.n_max
            );
            return Stage2::Refused;
        }

        // Step 4: "Try to recycle a page."
        let slot;
        if self.b_purgeable && self.lru_tail != NIL && self.n_page + 1 >= self.n_max {
            let id = self.lru_tail;
            slot = self.idents[id as usize].slot;
            #[cfg(sqliters_dtrace)]
            {
                let (h, n) = self.lru_hash();
                pctr!(
                    self,
                    "TR|pc|miss|c={} pg={} cf={} npage={} nrec={} nmax={} o=recycle rp={} lru={:08x}/{}",
                    self.tr_id,
                    pgno,
                    create_flag,
                    self.n_page,
                    self.n_recyclable,
                    self.n_max,
                    self.entries[slot as usize].pgno,
                    h,
                    n
                );
            }
            // pcache1RemoveFromHash(pPage, 0); pcache1PinPage(pPage);
            let old = self.entries[slot as usize].pgno;
            self.hash_clear(old);
            self.n_page -= 1;
            self.pcache1_pin_page(id);
        } else {
            // Step 5: "attempt to allocate a new one".
            pctr!(
                self,
                "TR|pc|miss|c={} pg={} cf={} npage={} nrec={} nmax={} o=alloc",
                self.tr_id,
                pgno,
                create_flag,
                self.n_page,
                self.n_recyclable,
                self.n_max
            );
            slot = if let Some(s) = self.free_slots.pop() {
                s
            } else {
                let s = self.entries.len() as u32;
                self.entries.push(CachedPage {
                    pgno: 0,
                    data: Vec::new(),
                    flags: 0,
                    occupied: false,
                    ident: s,
                });
                self.idents.push(PgIdent::new(s));
                s
            };
        }
        self.n_page += 1;
        let e = &mut self.entries[slot as usize];
        e.pgno = pgno;
        e.occupied = true;
        // pcacheFetchFinishWithInit: `pPgHdr->flags = PGHDR_CLEAN`, nRef = 0, no dirty links.
        e.flags = PGHDR_CLEAN;
        let id = e.ident;
        let i = &mut self.idents[id as usize];
        i.n_ref = 0;
        i.d_prev = NIL;
        i.d_next = NIL;
        self.hash_set(pgno, slot);
        Stage2::Slot(slot)
    }

    /// `pcache1Fetch` for a page that is in the cache: "if the page is unpinned, pin it".
    #[inline]
    pub(crate) fn pcache1_fetch_hit(&mut self, slot: u32) {
        let id = self.entries[slot as usize].ident;
        if self.idents[id as usize].in_lru {
            self.pcache1_pin_page(id);
        }
    }

    /// `pcache1Unpin(p, pPg, reuseUnlikely)`: an unreferenced clean page goes to the LRU list —
    /// or is freed at once when the cache holds more than `nMax` pages. Returns `true` when
    /// the page was freed.
    fn pcache1_unpin(&mut self, slot: u32, reuse_unlikely: bool) -> bool {
        if reuse_unlikely || self.n_page > self.n_max {
            pctr!(
                self,
                "TR|pc|drop|c={} pg={} reuse={} npage={} nmax={}",
                self.tr_id,
                self.entries[slot as usize].pgno,
                u8::from(reuse_unlikely),
                self.n_page,
                self.n_max
            );
            self.pcache1_remove_from_hash_free(slot);
            true
        } else {
            let id = self.entries[slot as usize].ident;
            let head = self.lru_head;
            {
                let i = &mut self.idents[id as usize];
                i.l_prev = NIL;
                i.l_next = head;
                i.in_lru = true;
            }
            if head != NIL {
                self.idents[head as usize].l_prev = id;
            } else {
                self.lru_tail = id;
            }
            self.lru_head = id;
            self.n_recyclable += 1;
            false
        }
    }

    // ------------------------------------------------------------------ dirty list (pcache.c)

    /// `pcacheManageDirtyList(pPage, PCACHE_DIRTYLIST_REMOVE)`.
    fn dirty_list_remove(&mut self, id: u32) {
        let (prev, next) = {
            let i = &self.idents[id as usize];
            (i.d_prev, i.d_next)
        };
        // "Update the PCache1.pSynced variable if necessary."
        if self.p_synced == id {
            self.p_synced = prev;
        }
        if next != NIL {
            self.idents[next as usize].d_prev = prev;
        } else {
            self.p_dirty_tail = prev;
        }
        if prev != NIL {
            self.idents[prev as usize].d_next = next;
        } else {
            // "If there are now no dirty pages in the cache, set eCreate to 2."
            self.p_dirty = next;
            if self.p_dirty == NIL {
                self.e_create = 2;
            }
        }
    }

    /// `pcacheManageDirtyList(pPage, PCACHE_DIRTYLIST_ADD)`.
    fn dirty_list_add(&mut self, id: u32) {
        let head = self.p_dirty;
        {
            let i = &mut self.idents[id as usize];
            i.d_prev = NIL;
            i.d_next = head;
        }
        if head != NIL {
            self.idents[head as usize].d_prev = id;
        } else {
            self.p_dirty_tail = id;
            if self.b_purgeable {
                self.e_create = 1;
            }
        }
        self.p_dirty = id;
        // "If pSynced is NULL and this page has a clear NEED_SYNC flag, set pSynced to point
        // to this page."
        if self.p_synced == NIL {
            let slot = self.idents[id as usize].slot;
            if self.entries[slot as usize].flags & PGHDR_NEED_SYNC == 0 {
                self.p_synced = id;
            }
        }
    }

    /// `pcacheManageDirtyList(pPage, PCACHE_DIRTYLIST_FRONT)`.
    #[inline]
    fn dirty_list_front(&mut self, id: u32) {
        self.dirty_list_remove(id);
        self.dirty_list_add(id);
    }

    // ------------------------------------------------------------------ references

    /// `sqlite3PcacheFetchFinish`: one more reference.
    #[inline]
    #[allow(unexpected_cfgs)]
    pub(crate) fn fetch_finish(&mut self, slot: u32) {
        let id = self.entries[slot as usize].ident;
        self.idents[id as usize].n_ref += 1;
        self.n_ref_sum += 1;
        #[cfg(sqliters_dtrace)]
        if pcx_on() {
            pctr!(
                self,
                "TR|pcx|get|c={} pg={} ref={} fl={:x}",
                self.tr_id,
                self.entries[slot as usize].pgno,
                self.idents[id as usize].n_ref,
                self.entries[slot as usize].flags
            );
        }
    }

    /// `sqlite3PcacheRelease`: one reference less; at zero a clean page is unpinned
    /// (`pcacheUnpin`) and a dirty page moves to the front of the dirty list. Returns `true`
    /// when the page left the cache.
    #[allow(unexpected_cfgs)]
    pub(crate) fn release(&mut self, slot: u32) -> bool {
        let id = self.entries[slot as usize].ident;
        if self.idents[id as usize].n_ref <= 0 {
            #[cfg(sqliters_dtrace)]
            if pcx_on() {
                pctr!(
                    self,
                    "TR|pcx|rel-unreferenced|c={} pg={}",
                    self.tr_id,
                    self.entries[slot as usize].pgno
                );
            }
            return false;
        }
        self.idents[id as usize].n_ref -= 1;
        self.n_ref_sum -= 1;
        #[cfg(sqliters_dtrace)]
        if pcx_on() {
            pctr!(
                self,
                "TR|pcx|rel|c={} pg={} ref={} fl={:x}",
                self.tr_id,
                self.entries[slot as usize].pgno,
                self.idents[id as usize].n_ref,
                self.entries[slot as usize].flags
            );
        }
        let i = &self.idents[id as usize];
        if i.n_ref == 0 {
            if self.entries[slot as usize].flags & PGHDR_CLEAN != 0 {
                if self.b_purgeable {
                    return self.pcache1_unpin(slot, false);
                }
            } else {
                self.dirty_list_front(id);
            }
        }
        false
    }

    /// No transaction is open any more, so nobody can hold a page reference (C: the pager
    /// unlocks when the reference count drops to zero). References still counted here were
    /// taken on a path that did not give them back (an error return): they are released now.
    /// Returns how many pages were affected.
    ///
    /// `keep_page1`: one reference on page 1 stays (the b-tree layer's, while a transaction
    /// is open) — the state after `saveAllCursors(pBt, 0, 0)`, which C runs before a
    /// savepoint is rolled back.
    pub(crate) fn release_all_refs(&mut self, keep_page1: bool) -> u32 {
        if self.n_ref_sum <= i64::from(keep_page1) {
            return 0;
        }
        let mut n = 0u32;
        for slot in 0..self.entries.len() as u32 {
            if !self.entries[slot as usize].occupied {
                continue;
            }
            let id = self.entries[slot as usize].ident;
            let r = self.idents[id as usize].n_ref;
            let keep = i32::from(keep_page1 && self.entries[slot as usize].pgno == 1);
            if r > keep {
                self.n_ref_sum -= i64::from(r - keep - 1);
                self.idents[id as usize].n_ref = keep + 1;
                self.release(slot);
                n += 1;
            }
        }
        n
    }

    /// `sqlite3PcacheDrop`: the page (one reference, which is dropped) leaves the cache.
    pub(crate) fn drop_page(&mut self, slot: u32) {
        let id = self.entries[slot as usize].ident;
        if self.entries[slot as usize].flags & PGHDR_DIRTY != 0 {
            self.dirty_list_remove(id);
            self.n_dirty -= 1;
        }
        let n = self.idents[id as usize].n_ref;
        self.n_ref_sum -= i64::from(n);
        self.idents[id as usize].n_ref = 0;
        if self.idents[id as usize].in_lru {
            self.pcache1_pin_page(id);
        }
        // xUnpin(.., reuseUnlikely=1)
        pctr!(
            self,
            "TR|pc|drop|c={} pg={} reuse=1 npage={} nmax={}",
            self.tr_id,
            self.entries[slot as usize].pgno,
            self.n_page,
            self.n_max
        );
        self.pcache1_remove_from_hash_free(slot);
    }

    /// `sqlite3PcacheMakeDirty`.
    pub(crate) fn make_dirty(&mut self, slot: u32) {
        let e = &mut self.entries[slot as usize];
        if e.flags & (PGHDR_CLEAN | PGHDR_DONT_WRITE) != 0 {
            e.flags &= !PGHDR_DONT_WRITE;
            if e.flags & PGHDR_CLEAN != 0 {
                e.flags ^= PGHDR_DIRTY | PGHDR_CLEAN;
                let id = e.ident;
                // (A dirty page is never on the LRU list: C only dirties referenced pages.)
                if self.idents[id as usize].in_lru {
                    self.pcache1_pin_page(id);
                }
                self.n_dirty += 1;
                self.dirty_list_add(id);
            }
        }
    }

    /// `sqlite3PcacheMakeClean`. Returns `true` when the page left the cache (an unreferenced
    /// page that became clean while the cache is over its limit).
    pub(crate) fn make_clean(&mut self, slot: u32) -> bool {
        let e = &mut self.entries[slot as usize];
        if e.flags & PGHDR_DIRTY == 0 {
            return false;
        }
        let id = e.ident;
        self.dirty_list_remove(id);
        self.n_dirty -= 1;
        let e = &mut self.entries[slot as usize];
        e.flags &= !(PGHDR_DIRTY | PGHDR_NEED_SYNC | PGHDR_WRITEABLE);
        e.flags |= PGHDR_CLEAN;
        if self.idents[id as usize].n_ref == 0 && self.b_purgeable {
            return self.pcache1_unpin(slot, false);
        }
        false
    }

    /// `sqlite3PcacheCleanAll`: "make every page in the cache clean", from the head of the
    /// dirty list.
    pub(crate) fn clean_all(&mut self) {
        while self.p_dirty != NIL {
            let slot = self.idents[self.p_dirty as usize].slot;
            self.make_clean(slot);
        }
    }

    /// `sqlite3PcacheClearSyncFlags`.
    pub(crate) fn clear_sync_flags(&mut self) {
        let mut id = self.p_dirty;
        while id != NIL {
            let slot = self.idents[id as usize].slot;
            self.entries[slot as usize].flags &= !PGHDR_NEED_SYNC;
            id = self.idents[id as usize].d_next;
        }
        self.p_synced = self.p_dirty_tail;
    }

    /// The victim choice of `sqlite3PcacheFetchStress`: "first try to find a page that does
    /// not require a journal-sync (one with PGHDR_NEED_SYNC cleared), but if that is not
    /// possible settle for any other unreferenced dirty page". Updates `pSynced`.
    pub(crate) fn stress_victim(&mut self) -> Option<u32> {
        let mut id = self.p_synced;
        while id != NIL {
            let i = &self.idents[id as usize];
            if i.n_ref == 0 && self.entries[i.slot as usize].flags & PGHDR_NEED_SYNC == 0 {
                break;
            }
            id = i.d_prev;
        }
        self.p_synced = id;
        if id == NIL {
            id = self.p_dirty_tail;
            while id != NIL {
                let i = &self.idents[id as usize];
                if i.n_ref == 0 {
                    break;
                }
                id = i.d_prev;
            }
        }
        if id == NIL {
            None
        } else {
            Some(self.idents[id as usize].slot)
        }
    }

    /// Trace state of the dirty list: `(pSynced pgno, FNV-1a over (pgno, referenced?,
    /// NEED_SYNC?) from head to tail, length)`.
    #[allow(dead_code)]
    pub(crate) fn dirty_list_hash(&self) -> (Pgno, u32, u32) {
        let mut h: u32 = 0x811c_9dc5;
        let mut n = 0u32;
        let mut id = self.p_dirty;
        while id != NIL {
            let i = &self.idents[id as usize];
            let e = &self.entries[i.slot as usize];
            h = (h ^ e.pgno).wrapping_mul(0x0100_0193);
            let bits = u32::from(i.n_ref != 0) | if e.flags & PGHDR_NEED_SYNC != 0 { 2 } else { 0 };
            h = (h ^ bits).wrapping_mul(0x0100_0193);
            n += 1;
            id = i.d_next;
        }
        let synced = if self.p_synced == NIL {
            0
        } else {
            self.entries[self.idents[self.p_synced as usize].slot as usize].pgno
        };
        (synced, h, n)
    }

    /// `sqlite3PcacheDirtyList`: the dirty pages in ascending page-number order.
    pub(crate) fn dirty_pgnos_sorted(&self) -> Vec<Pgno> {
        let mut v = Vec::with_capacity(self.n_dirty as usize);
        let mut id = self.p_dirty;
        while id != NIL {
            let i = &self.idents[id as usize];
            v.push(self.entries[i.slot as usize].pgno);
            id = i.d_next;
        }
        v.sort_unstable();
        v
    }

    /// `sqlite3PagerRekey` as `balance_nonroot` uses it to renumber its new pages:
    ///
    /// ```c
    /// for(i=0; i<nNew-1; i++){
    ///   iB = index of the smallest page number among apNew[i..];
    ///   if( iB!=i ){
    ///     sqlite3PagerRekey(apNew[i]->pDbPage, pgnoTemp, fgB);
    ///     sqlite3PagerRekey(apNew[iB]->pDbPage, pgnoA, fgA);
    ///     sqlite3PagerRekey(apNew[i]->pDbPage, pgnoB, fgB);
    ///   }
    /// }
    /// ```
    ///
    /// The two `PgHdr` objects swap page numbers. Reference counts and list positions travel
    /// with the objects; the flags given to an object are those of its new number, so content
    /// and flags stay with the page numbers here. Each rekey whose flags have `PGHDR_DIRTY`
    /// and `PGHDR_NEED_SYNC` moves the object to the front of the dirty list
    /// (`sqlite3PcacheMove`). `a_pgno[i]` is the number of new page `i` on entry,
    /// `pgno_temp` C's `PENDING_BYTE/pageSize + 1`.
    ///
    /// Returns the swaps `(pgnoA, pgnoB)` in the order they were made: whoever holds a
    /// reference on one of the two objects (a cursor on the page being balanced) now holds it
    /// under the other number.
    pub(crate) fn rekey_balance(
        &mut self,
        a_pgno: &[Pgno],
        pgno_temp: Pgno,
    ) -> ([(Pgno, Pgno); 8], usize) {
        let mut swaps = [(0 as Pgno, 0 as Pgno); 8];
        let mut n_swaps = 0usize;
        let _ = pgno_temp;
        let n = a_pgno.len();
        if n > 8 || n < 2 {
            return (swaps, 0);
        }
        let mut ids = [NIL; 8];
        let mut pg = [0 as Pgno; 8];
        for i in 0..n {
            let Some(s) = self.slot_of(a_pgno[i]) else {
                return (swaps, 0);
            };
            ids[i] = self.entries[s as usize].ident;
            pg[i] = a_pgno[i];
        }
        let needs_front = |fl: u16| fl & PGHDR_DIRTY != 0 && fl & PGHDR_NEED_SYNC != 0;
        for i in 0..n - 1 {
            let mut ib = i;
            for j in i + 1..n {
                if pg[j] < pg[ib] {
                    ib = j;
                }
            }
            if ib == i {
                continue;
            }
            let (pa, pb) = (pg[i], pg[ib]);
            let (Some(sa), Some(sb)) = (self.slot_of(pa), self.slot_of(pb)) else {
                return (swaps, n_swaps);
            };
            let fga = self.entries[sa as usize].flags;
            let fgb = self.entries[sb as usize].flags;
            pctr!(
                self,
                "TR|pc|move|c={} from={} to={} fl={:x}",
                self.tr_id,
                pa,
                pgno_temp,
                fgb
            );
            // (the object carries `fgB` while it has the temporary number)
            self.entries[sa as usize].flags = fgb;
            if needs_front(fgb) {
                self.dirty_list_front(ids[i]);
            }
            pctr!(
                self,
                "TR|pc|move|c={} from={} to={} fl={:x}",
                self.tr_id,
                pb,
                pa,
                fga
            );
            self.entries[sa as usize].flags = fga;
            if needs_front(fga) {
                self.dirty_list_front(ids[ib]);
            }
            pctr!(
                self,
                "TR|pc|move|c={} from={} to={} fl={:x}",
                self.tr_id,
                pgno_temp,
                pb,
                fgb
            );
            if needs_front(fgb) {
                self.dirty_list_front(ids[i]);
            }
            // object `iB` now carries `pgnoA`, object `i` carries `pgnoB`
            self.entries[sa as usize].ident = ids[ib];
            self.idents[ids[ib] as usize].slot = sa;
            self.entries[sb as usize].ident = ids[i];
            self.idents[ids[i] as usize].slot = sb;
            pg[i] = pb;
            pg[ib] = pa;
            swaps[n_swaps] = (pa, pb);
            n_swaps += 1;
        }
        (swaps, n_swaps)
    }

    // ------------------------------------------------------------------ sizes

    /// `numberOfCachePages`.
    pub(crate) fn number_of_cache_pages(&self) -> i32 {
        if self.sz_cache >= 0 {
            self.sz_cache
        } else {
            let n =
                (-1024 * i64::from(self.sz_cache)) / (i64::from(self.sz_page) + PCACHE_SZ_EXTRA);
            n.min(1_000_000_000) as i32
        }
    }

    /// `sqlite3PcacheSetCachesize`.
    pub(crate) fn set_cachesize(&mut self, mx_page: i32) {
        self.sz_cache = mx_page;
        let n = self.number_of_cache_pages();
        self.pcache1_cachesize(n);
        pctr!(
            self,
            "TR|pc|cachesize|c={} sz={} n={}",
            self.tr_id,
            mx_page,
            n
        );
    }

    /// `sqlite3PcacheSetSpillsize`: returns the effective spill size (the pragma's value).
    pub(crate) fn set_spillsize(&mut self, mut mx_page: i32) -> i32 {
        let arg = mx_page;
        if mx_page != 0 {
            if mx_page < 0 {
                mx_page = ((-1024 * i64::from(mx_page))
                    / (i64::from(self.sz_page) + PCACHE_SZ_EXTRA)) as i32;
            }
            self.sz_spill = mx_page;
        }
        let mut res = self.number_of_cache_pages();
        if res < self.sz_spill {
            res = self.sz_spill;
        }
        let _ = arg;
        pctr!(
            self,
            "TR|pc|spillsize|c={} arg={} sz={} res={}",
            self.tr_id,
            arg,
            self.sz_spill,
            res
        );
        res
    }

    /// `sqlite3PcacheSetPageSize`: a new (empty) `PCache1` for the new page size; `szCache`
    /// and `szSpill` are kept.
    pub(crate) fn set_page_size(&mut self, sz_page: u32) {
        self.reset_storage();
        // "xCachesize(pNew, numberOfCachePages(pCache)); ... pCache->szPage = szPage": the new
        // cache is sized with the OLD page size; a negative `cache_size` is converted with the
        // new one only at the next `sqlite3PcacheSetCachesize`.
        let n = self.number_of_cache_pages();
        self.sz_page = sz_page;
        self.n_max = 0;
        // (the new `PCache1` is sized before it gets its trace id: no event)
        let id = std::mem::take(&mut self.tr_id);
        self.pcache1_cachesize(n);
        self.tr_id = id;
    }

    /// `PCache.szPage`.
    #[inline]
    pub(crate) fn page_size(&self) -> u32 {
        self.sz_page
    }

    // ------------------------------------------------------------------ truncation

    fn reset_storage(&mut self) {
        self.dense_slots.clear();
        self.sparse_slots.clear();
        self.entries.clear();
        self.idents.clear();
        self.free_slots.clear();
        self.max_cached_pgno = 0;
        self.p_dirty = NIL;
        self.p_dirty_tail = NIL;
        self.p_synced = NIL;
        self.n_ref_sum = 0;
        self.e_create = 2;
        self.n_dirty = 0;
        self.n_page = 0;
        self.n_recyclable = 0;
        self.lru_head = NIL;
        self.lru_tail = NIL;
    }

    /// `sqlite3PcacheClear` as `pager_reset` uses it: every page leaves the cache. (C keeps a
    /// zeroed page 1 while references are outstanding; the pager re-reads page 1 itself.)
    pub(crate) fn clear(&mut self) {
        pctr!(self, "TR|pc|trunc|c={} pgno=0", self.tr_id);
        self.reset_storage();
    }

    /// `sqlite3PcacheTruncate(pCache, pgno)`: "drop every cache entry whose page number is
    /// greater than pgno" — dirty ones are made clean first.
    pub(crate) fn truncate(&mut self, pgno: Pgno) {
        pctr!(self, "TR|pc|trunc|c={} pgno={}", self.tr_id, pgno);
        // for(p=pCache->pDirty; p; p=pNext) if( p->pgno>pgno ) sqlite3PcacheMakeClean(p);
        let mut id = self.p_dirty;
        while id != NIL {
            let next = self.idents[id as usize].d_next;
            let slot = self.idents[id as usize].slot;
            if self.entries[slot as usize].pgno > pgno {
                // (not unpinned here: the page is removed just below)
                self.dirty_list_remove(id);
                self.n_dirty -= 1;
                let e = &mut self.entries[slot as usize];
                e.flags &= !(PGHDR_DIRTY | PGHDR_NEED_SYNC | PGHDR_WRITEABLE);
                e.flags |= PGHDR_CLEAN;
            }
            id = next;
        }
        // pcache1Truncate(p, pgno+1)
        if pgno >= self.max_cached_pgno {
            return;
        }
        let keep_len = (pgno.min(MAX_DENSE_PGNO) as usize) + 1;
        let mut victims: Vec<u32> = Vec::new();
        if keep_len < self.dense_slots.len() {
            for cell in &self.dense_slots[keep_len..] {
                if *cell != NIL {
                    victims.push(*cell);
                }
            }
        }
        if !self.sparse_slots.is_empty() {
            for (&p, &slot) in &self.sparse_slots {
                if p > pgno {
                    victims.push(slot);
                }
            }
        }
        for slot in victims {
            let id = self.entries[slot as usize].ident;
            if self.idents[id as usize].in_lru {
                self.pcache1_pin_page(id);
            }
            let n = self.idents[id as usize].n_ref;
            self.n_ref_sum -= i64::from(n);
            self.pcache1_remove_from_hash_free(slot);
        }
        if keep_len < self.dense_slots.len() {
            self.dense_slots.truncate(keep_len);
        }
        self.max_cached_pgno = pgno;
    }
}
