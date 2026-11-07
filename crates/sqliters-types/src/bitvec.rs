//! Fixed-range 1-based page bitmap ported conceptually from `sqlite/src/bitvec.c`.
//!
//! Used by the pager and rollback journal to track which pages (1..=`max_page`)
//! have already had their original pre-images journaled in the current transaction,
//! and by `PRAGMA integrity_check` to detect duplicate page references.

use crate::error::{ErrorCode, Result, SqliteError};
use crate::limits::Pgno;

/// Compact 1-based bitmap over page numbers `1..=size`.
#[derive(Debug, Clone)]
pub struct Bitvec {
    size: u32,
    count_set: u32,
    words: Vec<u64>,
}

impl Bitvec {
    /// Creates an empty `Bitvec` tracking pages `1..=size`.
    pub fn new(size: u32) -> Self {
        let word_count = (size as usize).div_ceil(64);
        Self {
            size,
            count_set: 0,
            words: vec![0u64; word_count],
        }
    }

    /// Maximum 1-based index (`Pgno`) representable without resizing.
    #[inline]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Number of bits currently set.
    #[inline]
    pub const fn len(&self) -> u32 {
        self.count_set
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.count_set == 0
    }

    /// Grows the upper bound if `new_size > self.size`.
    pub fn grow_to(&mut self, new_size: u32) {
        if new_size > self.size {
            let word_count = (new_size as usize).div_ceil(64);
            if word_count > self.words.len() {
                self.words.resize(word_count, 0);
            }
            self.size = new_size;
        }
    }

    /// Tests whether 1-based page `pgno` is set (`sqlite3BitvecTest`).
    #[inline]
    pub fn test(&self, pgno: Pgno) -> bool {
        if pgno == 0 || pgno > self.size {
            return false;
        }
        let idx = (pgno - 1) as usize;
        let word = idx / 64;
        let bit = idx % 64;
        (self.words[word] & (1u64 << bit)) != 0
    }

    /// Sets 1-based page `pgno` (`sqlite3BitvecSet`). Returns `true` if newly set, `false` if already set.
    pub fn set(&mut self, pgno: Pgno) -> Result<bool> {
        if pgno == 0 || pgno > self.size {
            return Err(SqliteError::new(
                ErrorCode::Range,
                format!("bitvec index {pgno} out of range 1..={}", self.size),
            ));
        }
        let idx = (pgno - 1) as usize;
        let word = idx / 64;
        let mask = 1u64 << (idx % 64);
        let was_set = (self.words[word] & mask) != 0;
        if !was_set {
            self.words[word] |= mask;
            self.count_set += 1;
        }
        Ok(!was_set)
    }

    /// Clears 1-based page `pgno` (`sqlite3BitvecClear`).
    pub fn clear(&mut self, pgno: Pgno) {
        if pgno == 0 || pgno > self.size {
            return;
        }
        let idx = (pgno - 1) as usize;
        let word = idx / 64;
        let mask = 1u64 << (idx % 64);
        if (self.words[word] & mask) != 0 {
            self.words[word] &= !mask;
            self.count_set -= 1;
        }
    }

    /// Resets all bits to 0.
    pub fn clear_all(&mut self) {
        self.words.fill(0);
        self.count_set = 0;
    }
}
