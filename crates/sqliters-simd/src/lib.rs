//! Hardware SIMD leaf kernels (`core::arch::x86_64` SSE2/AVX2 intrinsics) behind 100% safe,
//! slice-validated Rust APIs.
//!
//! Design invariants:
//! - Every `pub fn` accepts safe Rust types (`&[u8]`, `&[f32]`, `[u32; 2]`, `bool`, ...) and
//!   preserves bit-identical results with its plain scalar fallback across all lengths,
//!   alignments, and byte values.
//! - Zero inline or external assembly (`asm!` / `global_asm!`).
//! - Zero general-purpose `u64` SWAR bit-tricks in scalar fallbacks or sub-vector tails.
#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
#[cfg(target_arch = "x86_64")]
use std::sync::atomic::{AtomicU8, Ordering as AtomicOrdering};

#[cfg(target_arch = "x86_64")]
const FEAT_INIT: u8 = 1 << 0;
#[cfg(target_arch = "x86_64")]
const FEAT_AVX2: u8 = 1 << 1;

#[cfg(target_arch = "x86_64")]
static CPU_FEATURES: AtomicU8 = AtomicU8::new(0);

#[cfg(target_arch = "x86_64")]
#[cold]
#[inline(never)]
fn init_cpu_features() -> u8 {
    let mut mask = FEAT_INIT;
    if std::arch::is_x86_feature_detected!("avx2") {
        mask |= FEAT_AVX2;
    }
    CPU_FEATURES.store(mask, AtomicOrdering::Relaxed);
    mask
}

/// Returns `true` when runtime `x86_64` `AVX2` support is available (cached in a single `Relaxed`
/// `AtomicU8` load after first probe).
#[inline(always)]
pub fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let mut f = CPU_FEATURES.load(AtomicOrdering::Relaxed);
        if f == 0 {
            f = init_cpu_features();
        }
        (f & FEAT_AVX2) != 0
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

// ============================================================================
// SIMD-1: E-02 Text Substring (`LIKE '%lit%'`, `GLOB '*lit*'`), Folded Slice
//         Equality, and Wildcard Stop Byte Search (`find1_byte` / `find2_byte`)
// ============================================================================

#[inline(always)]
fn find1_scalar(haystack: &[u8], c1: u8) -> usize {
    for (i, &b) in haystack.iter().enumerate() {
        if b == c1 {
            return i;
        }
    }
    haystack.len()
}

#[inline(always)]
fn find2_scalar(haystack: &[u8], c1: u8, c2: u8) -> usize {
    for (i, &b) in haystack.iter().enumerate() {
        if b == c1 || b == c2 {
            return i;
        }
    }
    haystack.len()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn find1_sse2(z: &[u8], c1: u8) -> usize {
    let n = z.len();
    if n < 16 {
        return find1_scalar(z, c1);
    }
    let v1 = _mm_set1_epi8(c1 as i8);
    let ptr = z.as_ptr();
    let mut i = 0usize;
    while i + 16 <= n {
        // SAFETY: `i + 16 <= z.len()`, so `ptr.add(i)` is valid for a 16-byte unaligned read.
        let x = unsafe { _mm_loadu_si128(ptr.add(i).cast::<__m128i>()) };
        let m = _mm_movemask_epi8(_mm_cmpeq_epi8(x, v1)) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 16;
    }
    if i < n {
        let tail_i = n - 16;
        // SAFETY: `n >= 16` and `tail_i = n - 16`, so `ptr.add(tail_i)` reads `z[n - 16..n]`.
        let x = unsafe { _mm_loadu_si128(ptr.add(tail_i).cast::<__m128i>()) };
        let m = (_mm_movemask_epi8(_mm_cmpeq_epi8(x, v1)) as u32) & (!0u32 << (i - tail_i));
        if m != 0 {
            return tail_i + m.trailing_zeros() as usize;
        }
    }
    n
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn find1_avx2(z: &[u8], c1: u8) -> usize {
    let n = z.len();
    if n < 32 {
        return find1_sse2(z, c1);
    }
    let v1 = _mm256_set1_epi8(c1 as i8);
    let ptr = z.as_ptr();
    let mut i = 0usize;
    while i + 32 <= n {
        // SAFETY: `i + 32 <= z.len()`, so `ptr.add(i)` is valid for a 32-byte unaligned read.
        let x = unsafe { _mm256_loadu_si256(ptr.add(i).cast::<__m256i>()) };
        let m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(x, v1)) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 32;
    }
    if i < n {
        let tail_i = n - 32;
        // SAFETY: `n >= 32` and `tail_i = n - 32`, so `ptr.add(tail_i)` reads `z[n - 32..n]`.
        let x = unsafe { _mm256_loadu_si256(ptr.add(tail_i).cast::<__m256i>()) };
        let m = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(x, v1)) as u32) & (!0u32 << (i - tail_i));
        if m != 0 {
            return tail_i + m.trailing_zeros() as usize;
        }
    }
    n
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn find2_sse2(z: &[u8], c1: u8, c2: u8) -> usize {
    let n = z.len();
    if n < 16 {
        return find2_scalar(z, c1, c2);
    }
    let v1 = _mm_set1_epi8(c1 as i8);
    let v2 = _mm_set1_epi8(c2 as i8);
    let ptr = z.as_ptr();
    let mut i = 0usize;
    while i + 16 <= n {
        // SAFETY: `i + 16 <= z.len()`, so `ptr.add(i)` is valid for a 16-byte unaligned read.
        let x = unsafe { _mm_loadu_si128(ptr.add(i).cast::<__m128i>()) };
        let m =
            _mm_movemask_epi8(_mm_or_si128(_mm_cmpeq_epi8(x, v1), _mm_cmpeq_epi8(x, v2))) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 16;
    }
    if i < n {
        let tail_i = n - 16;
        // SAFETY: `n >= 16` and `tail_i = n - 16`, so `ptr.add(tail_i)` reads `z[n - 16..n]`.
        let x = unsafe { _mm_loadu_si128(ptr.add(tail_i).cast::<__m128i>()) };
        let eq = _mm_or_si128(_mm_cmpeq_epi8(x, v1), _mm_cmpeq_epi8(x, v2));
        let m = (_mm_movemask_epi8(eq) as u32) & (!0u32 << (i - tail_i));
        if m != 0 {
            return tail_i + m.trailing_zeros() as usize;
        }
    }
    n
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn find2_avx2(z: &[u8], c1: u8, c2: u8) -> usize {
    let n = z.len();
    if n < 32 {
        return find2_sse2(z, c1, c2);
    }
    let v1 = _mm256_set1_epi8(c1 as i8);
    let v2 = _mm256_set1_epi8(c2 as i8);
    let ptr = z.as_ptr();
    let mut i = 0usize;
    while i + 32 <= n {
        // SAFETY: `i + 32 <= z.len()`, so `ptr.add(i)` is valid for a 32-byte unaligned read.
        let x = unsafe { _mm256_loadu_si256(ptr.add(i).cast::<__m256i>()) };
        let m = _mm256_movemask_epi8(_mm256_or_si256(
            _mm256_cmpeq_epi8(x, v1),
            _mm256_cmpeq_epi8(x, v2),
        )) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 32;
    }
    if i < n {
        let tail_i = n - 32;
        // SAFETY: `n >= 32` and `tail_i = n - 32`, so `ptr.add(tail_i)` reads `z[n - 32..n]`.
        let x = unsafe { _mm256_loadu_si256(ptr.add(tail_i).cast::<__m256i>()) };
        let eq = _mm256_or_si256(_mm256_cmpeq_epi8(x, v1), _mm256_cmpeq_epi8(x, v2));
        let m = (_mm256_movemask_epi8(eq) as u32) & (!0u32 << (i - tail_i));
        if m != 0 {
            return tail_i + m.trailing_zeros() as usize;
        }
    }
    n
}

/// Returns the first index in `haystack` equal to `c1`, or `haystack.len()` if absent.
#[inline]
pub fn find1_byte(haystack: &[u8], c1: u8) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { find1_avx2(haystack, c1) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { find1_sse2(haystack, c1) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        find1_scalar(haystack, c1)
    }
}

/// Returns the first index in `haystack` equal to `c1` or `c2`, or `haystack.len()` if absent.
#[inline]
pub fn find2_byte(haystack: &[u8], c1: u8, c2: u8) -> usize {
    if c1 == c2 {
        return find1_byte(haystack, c1);
    }
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { find2_avx2(haystack, c1, c2) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { find2_sse2(haystack, c1, c2) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        find2_scalar(haystack, c1, c2)
    }
}

#[inline(always)]
fn eq_ascii_nocase_scalar(hay: &[u8], lower_needle: &[u8]) -> bool {
    if hay.len() != lower_needle.len() {
        return false;
    }
    for i in 0..lower_needle.len() {
        if hay[i].to_ascii_lowercase() != lower_needle[i] {
            return false;
        }
    }
    true
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn eq_ascii_nocase_sse2(hay: &[u8], lower_needle: &[u8]) -> bool {
    let n = lower_needle.len();
    if hay.len() != n {
        return false;
    }
    if n < 16 {
        return eq_ascii_nocase_scalar(hay, lower_needle);
    }
    let ca = _mm_set1_epi8(b'A' as i8);
    let c25 = _mm_set1_epi8(25);
    let c20 = _mm_set1_epi8(0x20);
    let fold = |x: __m128i| {
        let t = _mm_sub_epi8(x, ca);
        let up = _mm_cmpeq_epi8(_mm_min_epu8(t, c25), t);
        _mm_or_si128(x, _mm_and_si128(up, c20))
    };
    let hp = hay.as_ptr();
    let np = lower_needle.as_ptr();
    let mut i = 0usize;
    while i + 16 <= n {
        // SAFETY: `i + 16 <= n` and both slices have length `n`.
        let hv = unsafe { _mm_loadu_si128(hp.add(i).cast::<__m128i>()) };
        // SAFETY: `i + 16 <= n` and `lower_needle.len() == n`.
        let nv = unsafe { _mm_loadu_si128(np.add(i).cast::<__m128i>()) };
        if _mm_movemask_epi8(_mm_cmpeq_epi8(fold(hv), nv)) != 0xFFFF {
            return false;
        }
        i += 16;
    }
    if i < n {
        let tail_i = n - 16;
        // SAFETY: `n >= 16` and `tail_i = n - 16`.
        let hv = unsafe { _mm_loadu_si128(hp.add(tail_i).cast::<__m128i>()) };
        // SAFETY: `n >= 16` and `tail_i = n - 16`.
        let nv = unsafe { _mm_loadu_si128(np.add(tail_i).cast::<__m128i>()) };
        if _mm_movemask_epi8(_mm_cmpeq_epi8(fold(hv), nv)) != 0xFFFF {
            return false;
        }
    }
    true
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn eq_ascii_nocase_avx2(hay: &[u8], lower_needle: &[u8]) -> bool {
    let n = lower_needle.len();
    if hay.len() != n {
        return false;
    }
    if n < 32 {
        return eq_ascii_nocase_sse2(hay, lower_needle);
    }
    let ca = _mm256_set1_epi8(b'A' as i8);
    let c25 = _mm256_set1_epi8(25);
    let c20 = _mm256_set1_epi8(0x20);
    let fold = |x: __m256i| {
        let t = _mm256_sub_epi8(x, ca);
        let up = _mm256_cmpeq_epi8(_mm256_min_epu8(t, c25), t);
        _mm256_or_si256(x, _mm256_and_si256(up, c20))
    };
    let hp = hay.as_ptr();
    let np = lower_needle.as_ptr();
    let mut i = 0usize;
    while i + 32 <= n {
        // SAFETY: `i + 32 <= n` and both slices have length `n`.
        let hv = unsafe { _mm256_loadu_si256(hp.add(i).cast::<__m256i>()) };
        // SAFETY: `i + 32 <= n` and `lower_needle.len() == n`.
        let nv = unsafe { _mm256_loadu_si256(np.add(i).cast::<__m256i>()) };
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(fold(hv), nv)) as u32 != 0xFFFF_FFFF {
            return false;
        }
        i += 32;
    }
    if i < n {
        let tail_i = n - 32;
        // SAFETY: `n >= 32` and `tail_i = n - 32`.
        let hv = unsafe { _mm256_loadu_si256(hp.add(tail_i).cast::<__m256i>()) };
        // SAFETY: `n >= 32` and `tail_i = n - 32`.
        let nv = unsafe { _mm256_loadu_si256(np.add(tail_i).cast::<__m256i>()) };
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(fold(hv), nv)) as u32 != 0xFFFF_FFFF {
            return false;
        }
    }
    true
}

/// Checks whether `hay` equals `lower_needle` under ASCII case-folding (`A..=Z` -> `a..=z`),
/// assuming `lower_needle` has already had ASCII uppercase bytes folded to lowercase.
#[inline]
pub fn eq_ascii_nocase(hay: &[u8], lower_needle: &[u8]) -> bool {
    if hay.len() != lower_needle.len() {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if lower_needle.len() < 16 {
            return eq_ascii_nocase_scalar(hay, lower_needle);
        }
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { eq_ascii_nocase_avx2(hay, lower_needle) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { eq_ascii_nocase_sse2(hay, lower_needle) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        eq_ascii_nocase_scalar(hay, lower_needle)
    }
}

#[inline(always)]
fn contains_ascii_nocase_scalar(hay: &[u8], lower_needle: &[u8]) -> bool {
    let n = lower_needle.len();
    if n == 0 {
        return true;
    }
    if n == 1 {
        let b = lower_needle[0];
        let u = b.to_ascii_uppercase();
        return find2_scalar(hay, b, u) < hay.len();
    }
    if hay.len() < n {
        return false;
    }
    let first = lower_needle[0];
    let last = lower_needle[n - 1];
    let span = hay.len() - n + 1;
    let mid = &lower_needle[1..n - 1];
    for i in 0..span {
        if hay[i].to_ascii_lowercase() == first
            && hay[i + n - 1].to_ascii_lowercase() == last
            && (n == 2 || eq_ascii_nocase_scalar(&hay[i + 1..i + n - 1], mid))
        {
            return true;
        }
    }
    false
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn fold_first_last_lower(lower_needle: &[u8]) -> (u8, u8, u8, u8) {
    let first = lower_needle[0];
    let last = lower_needle[lower_needle.len() - 1];
    let fm = if first.is_ascii_lowercase() { 0x20 } else { 0 };
    let lm = if last.is_ascii_lowercase() { 0x20 } else { 0 };
    (first, fm, last, lm)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn contains_ascii_nocase_sse2(hay: &[u8], lower_needle: &[u8]) -> bool {
    let n = lower_needle.len();
    if n == 0 {
        return true;
    }
    if n == 1 {
        let b = lower_needle[0];
        let u = b.to_ascii_uppercase();
        return find2_sse2(hay, b, u) < hay.len();
    }
    if hay.len() < n {
        return false;
    }
    let span = hay.len() - n + 1;
    if span < 16 {
        return contains_ascii_nocase_scalar(hay, lower_needle);
    }
    let (first, fm, last, lm) = fold_first_last_lower(lower_needle);
    let f = _mm_set1_epi8(first as i8);
    let fmv = _mm_set1_epi8(fm as i8);
    let l = _mm_set1_epi8(last as i8);
    let lmv = _mm_set1_epi8(lm as i8);
    let mid = &lower_needle[1..n - 1];
    let hp = hay.as_ptr();
    let mut i = 0usize;
    while i + 16 <= span {
        // SAFETY: `i + 16 <= span = hay.len() - n + 1`, so `i + 16 <= hay.len()` and
        // `i + n - 1 + 16 <= hay.len()`. Both 16-byte unaligned loads stay in bounds of `hay`.
        let a = unsafe { _mm_loadu_si128(hp.add(i).cast::<__m128i>()) };
        // SAFETY: See bounds proof above (`i + n - 1 + 16 <= hay.len()`).
        let b = unsafe { _mm_loadu_si128(hp.add(i + n - 1).cast::<__m128i>()) };
        let ea = _mm_cmpeq_epi8(_mm_or_si128(a, fmv), f);
        let eb = _mm_cmpeq_epi8(_mm_or_si128(b, lmv), l);
        let mut m = _mm_movemask_epi8(_mm_and_si128(ea, eb)) as u32;
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = i + j;
            let ok = if mid.len() < 16 {
                eq_ascii_nocase_scalar(&hay[pos + 1..pos + n - 1], mid)
            } else {
                eq_ascii_nocase_sse2(&hay[pos + 1..pos + n - 1], mid)
            };
            if n == 2 || ok {
                return true;
            }
            m &= m - 1;
        }
        i += 16;
    }
    if i < span {
        let tail_i = span - 16;
        // SAFETY: `span >= 16` and `tail_i = span - 16`, so `tail_i + n - 1 + 16 == hay.len()`.
        let a = unsafe { _mm_loadu_si128(hp.add(tail_i).cast::<__m128i>()) };
        // SAFETY: `tail_i + n - 1 + 16 == hay.len()`, exact end of `hay`.
        let b = unsafe { _mm_loadu_si128(hp.add(tail_i + n - 1).cast::<__m128i>()) };
        let ea = _mm_cmpeq_epi8(_mm_or_si128(a, fmv), f);
        let eb = _mm_cmpeq_epi8(_mm_or_si128(b, lmv), l);
        let mut m = (_mm_movemask_epi8(_mm_and_si128(ea, eb)) as u32) & (!0u32 << (i - tail_i));
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = tail_i + j;
            let ok = if mid.len() < 16 {
                eq_ascii_nocase_scalar(&hay[pos + 1..pos + n - 1], mid)
            } else {
                eq_ascii_nocase_sse2(&hay[pos + 1..pos + n - 1], mid)
            };
            if n == 2 || ok {
                return true;
            }
            m &= m - 1;
        }
    }
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn contains_ascii_nocase_avx2(hay: &[u8], lower_needle: &[u8]) -> bool {
    let n = lower_needle.len();
    if n == 0 {
        return true;
    }
    if n == 1 {
        let b = lower_needle[0];
        let u = b.to_ascii_uppercase();
        return if b == u {
            find1_avx2(hay, b) < hay.len()
        } else {
            find2_avx2(hay, b, u) < hay.len()
        };
    }
    if hay.len() < n {
        return false;
    }
    let span = hay.len() - n + 1;
    if span < 32 {
        return contains_ascii_nocase_sse2(hay, lower_needle);
    }
    let (first, fm, last, lm) = fold_first_last_lower(lower_needle);
    let f = _mm256_set1_epi8(first as i8);
    let fmv = _mm256_set1_epi8(fm as i8);
    let l = _mm256_set1_epi8(last as i8);
    let lmv = _mm256_set1_epi8(lm as i8);
    let mid = &lower_needle[1..n - 1];
    let hp = hay.as_ptr();
    let mut i = 0usize;
    while i + 32 <= span {
        // SAFETY: `i + 32 <= span = hay.len() - n + 1`, so `i + n - 1 + 32 <= hay.len()`.
        // Both 32-byte unaligned loads stay strictly within `hay`.
        let a = unsafe { _mm256_loadu_si256(hp.add(i).cast::<__m256i>()) };
        // SAFETY: `i + n - 1 + 32 <= hay.len()`.
        let b = unsafe { _mm256_loadu_si256(hp.add(i + n - 1).cast::<__m256i>()) };
        let ea = _mm256_cmpeq_epi8(_mm256_or_si256(a, fmv), f);
        let eb = _mm256_cmpeq_epi8(_mm256_or_si256(b, lmv), l);
        let mut m = _mm256_movemask_epi8(_mm256_and_si256(ea, eb)) as u32;
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = i + j;
            let ok = if mid.len() < 16 {
                eq_ascii_nocase_scalar(&hay[pos + 1..pos + n - 1], mid)
            } else {
                eq_ascii_nocase_avx2(&hay[pos + 1..pos + n - 1], mid)
            };
            if n == 2 || ok {
                return true;
            }
            m &= m - 1;
        }
        i += 32;
    }
    if i < span {
        let tail_i = span - 32;
        // SAFETY: `span >= 32` and `tail_i = span - 32`, so `tail_i + n - 1 + 32 == hay.len()`.
        let a = unsafe { _mm256_loadu_si256(hp.add(tail_i).cast::<__m256i>()) };
        // SAFETY: `tail_i + n - 1 + 32 == hay.len()`, exact end of `hay`.
        let b = unsafe { _mm256_loadu_si256(hp.add(tail_i + n - 1).cast::<__m256i>()) };
        let ea = _mm256_cmpeq_epi8(_mm256_or_si256(a, fmv), f);
        let eb = _mm256_cmpeq_epi8(_mm256_or_si256(b, lmv), l);
        let mut m =
            (_mm256_movemask_epi8(_mm256_and_si256(ea, eb)) as u32) & (!0u32 << (i - tail_i));
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = tail_i + j;
            let ok = if mid.len() < 16 {
                eq_ascii_nocase_scalar(&hay[pos + 1..pos + n - 1], mid)
            } else {
                eq_ascii_nocase_avx2(&hay[pos + 1..pos + n - 1], mid)
            };
            if n == 2 || ok {
                return true;
            }
            m &= m - 1;
        }
    }
    false
}

/// Case-insensitive ASCII substring search (`LIKE '%lit%'`), where `lower_needle` has already had
/// ASCII uppercase bytes (`A..=Z`) folded to lowercase (`a..=z`).
#[inline]
pub fn contains_ascii_nocase(haystack: &[u8], lower_needle: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { contains_ascii_nocase_avx2(haystack, lower_needle) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { contains_ascii_nocase_sse2(haystack, lower_needle) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        contains_ascii_nocase_scalar(haystack, lower_needle)
    }
}

#[inline(always)]
fn contains_bytes_scalar(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 {
        return true;
    }
    if n == 1 {
        return find1_scalar(hay, needle[0]) < hay.len();
    }
    if hay.len() < n {
        return false;
    }
    let first = needle[0];
    let last = needle[n - 1];
    let span = hay.len() - n + 1;
    let mid = &needle[1..n - 1];
    for i in 0..span {
        if hay[i] == first && hay[i + n - 1] == last && (n == 2 || &hay[i + 1..i + n - 1] == mid) {
            return true;
        }
    }
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn contains_bytes_sse2(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 {
        return true;
    }
    if n == 1 {
        return find1_sse2(hay, needle[0]) < hay.len();
    }
    if hay.len() < n {
        return false;
    }
    let span = hay.len() - n + 1;
    if span < 16 {
        return contains_bytes_scalar(hay, needle);
    }
    let f = _mm_set1_epi8(needle[0] as i8);
    let l = _mm_set1_epi8(needle[n - 1] as i8);
    let mid = &needle[1..n - 1];
    let hp = hay.as_ptr();
    let mut i = 0usize;
    while i + 16 <= span {
        // SAFETY: `i + 16 <= span = hay.len() - n + 1`, so `i + n - 1 + 16 <= hay.len()`.
        let a = unsafe { _mm_loadu_si128(hp.add(i).cast::<__m128i>()) };
        // SAFETY: `i + n - 1 + 16 <= hay.len()`.
        let b = unsafe { _mm_loadu_si128(hp.add(i + n - 1).cast::<__m128i>()) };
        let mut m =
            _mm_movemask_epi8(_mm_and_si128(_mm_cmpeq_epi8(a, f), _mm_cmpeq_epi8(b, l))) as u32;
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = i + j;
            if n == 2 || &hay[pos + 1..pos + n - 1] == mid {
                return true;
            }
            m &= m - 1;
        }
        i += 16;
    }
    if i < span {
        let tail_i = span - 16;
        // SAFETY: `span >= 16` and `tail_i = span - 16`, so `tail_i + n - 1 + 16 == hay.len()`.
        let a = unsafe { _mm_loadu_si128(hp.add(tail_i).cast::<__m128i>()) };
        // SAFETY: `tail_i + n - 1 + 16 == hay.len()`.
        let b = unsafe { _mm_loadu_si128(hp.add(tail_i + n - 1).cast::<__m128i>()) };
        let eq = _mm_and_si128(_mm_cmpeq_epi8(a, f), _mm_cmpeq_epi8(b, l));
        let mut m = (_mm_movemask_epi8(eq) as u32) & (!0u32 << (i - tail_i));
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = tail_i + j;
            if n == 2 || &hay[pos + 1..pos + n - 1] == mid {
                return true;
            }
            m &= m - 1;
        }
    }
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn contains_bytes_avx2(hay: &[u8], needle: &[u8]) -> bool {
    let n = needle.len();
    if n == 0 {
        return true;
    }
    if n == 1 {
        return find1_avx2(hay, needle[0]) < hay.len();
    }
    if hay.len() < n {
        return false;
    }
    let span = hay.len() - n + 1;
    if span < 32 {
        return contains_bytes_sse2(hay, needle);
    }
    let f = _mm256_set1_epi8(needle[0] as i8);
    let l = _mm256_set1_epi8(needle[n - 1] as i8);
    let mid = &needle[1..n - 1];
    let hp = hay.as_ptr();
    let mut i = 0usize;
    while i + 32 <= span {
        // SAFETY: `i + 32 <= span = hay.len() - n + 1`, so `i + n - 1 + 32 <= hay.len()`.
        let a = unsafe { _mm256_loadu_si256(hp.add(i).cast::<__m256i>()) };
        // SAFETY: `i + n - 1 + 32 <= hay.len()`.
        let b = unsafe { _mm256_loadu_si256(hp.add(i + n - 1).cast::<__m256i>()) };
        let mut m = _mm256_movemask_epi8(_mm256_and_si256(
            _mm256_cmpeq_epi8(a, f),
            _mm256_cmpeq_epi8(b, l),
        )) as u32;
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = i + j;
            if n == 2 || &hay[pos + 1..pos + n - 1] == mid {
                return true;
            }
            m &= m - 1;
        }
        i += 32;
    }
    if i < span {
        let tail_i = span - 32;
        // SAFETY: `span >= 32` and `tail_i = span - 32`, so `tail_i + n - 1 + 32 == hay.len()`.
        let a = unsafe { _mm256_loadu_si256(hp.add(tail_i).cast::<__m256i>()) };
        // SAFETY: `tail_i + n - 1 + 32 == hay.len()`.
        let b = unsafe { _mm256_loadu_si256(hp.add(tail_i + n - 1).cast::<__m256i>()) };
        let eq = _mm256_and_si256(_mm256_cmpeq_epi8(a, f), _mm256_cmpeq_epi8(b, l));
        let mut m = (_mm256_movemask_epi8(eq) as u32) & (!0u32 << (i - tail_i));
        while m != 0 {
            let j = m.trailing_zeros() as usize;
            let pos = tail_i + j;
            if n == 2 || &hay[pos + 1..pos + n - 1] == mid {
                return true;
            }
            m &= m - 1;
        }
    }
    false
}

/// Case-sensitive byte-slice substring search (`LIKE '%lit%'` with `PRAGMA case_sensitive_like=ON`
/// and `GLOB '*lit*'`).
#[inline]
pub fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { contains_bytes_avx2(haystack, needle) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { contains_bytes_sse2(haystack, needle) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        contains_bytes_scalar(haystack, needle)
    }
}

// ============================================================================
// SIMD-3: E-08 4-Chain AVX2 (`T^16`) / SSE2 (`T^4`) Cumulative WAL Checksum
// ============================================================================

const fn wal_fib(n: u64) -> u32 {
    let (mut a, mut b) = (0u32, 1u32);
    let mut i = 0;
    while i < n {
        let t = a.wrapping_add(b);
        a = b;
        b = t;
        i += 1;
    }
    a
}

const fn wal_tpow(k: u64) -> [u32; 3] {
    let p = if k == 0 { 1 } else { wal_fib(2 * k - 1) };
    [p, wal_fib(2 * k), wal_fib(2 * k + 1)]
}

#[cfg(target_arch = "x86_64")]
const fn wal_tpow_table<const L: usize>() -> [[u32; 3]; L] {
    let mut t = [[0u32; 3]; L];
    let mut i = 0;
    while i < L {
        t[i] = wal_tpow(i as u64);
        i += 1;
    }
    t
}

#[cfg(target_arch = "x86_64")]
const fn wal_combine_tables<const L: usize, const N: usize>() -> ([u32; N], [u32; N]) {
    let mut m1 = [0u32; N];
    let mut m2 = [0u32; N];
    let mut l = 0;
    while l < L {
        let [a, b, c] = wal_tpow((L - 1 - l) as u64);
        m1[2 * l] = a;
        m1[2 * l + 1] = b;
        m2[2 * l] = b;
        m2[2 * l + 1] = c;
        l += 1;
    }
    (m1, m2)
}

#[cfg(target_arch = "x86_64")]
const WAL_TPOW4: [[u32; 3]; 4] = wal_tpow_table::<4>();
#[cfg(target_arch = "x86_64")]
static WAL_T16_TABLES: ([u32; 32], [u32; 32]) = wal_combine_tables::<16, 32>();

#[inline(always)]
fn wal_checksum_scalar_inner<const SWAP: bool>(data: &[u8], init: [u32; 2]) -> [u32; 2] {
    let [mut s1, mut s2] = init;
    for pair in data.as_chunks::<8>().0 {
        let mut w0 = u32::from_ne_bytes([pair[0], pair[1], pair[2], pair[3]]);
        let mut w1 = u32::from_ne_bytes([pair[4], pair[5], pair[6], pair[7]]);
        if SWAP {
            w0 = w0.swap_bytes();
            w1 = w1.swap_bytes();
        }
        s1 = s1.wrapping_add(w0).wrapping_add(s2);
        s2 = s2.wrapping_add(w1).wrapping_add(s1);
    }
    [s1, s2]
}

#[inline(always)]
fn wal_checksum_scalar(data: &[u8], init: [u32; 2], swap_bytes: bool) -> [u32; 2] {
    if swap_bytes {
        wal_checksum_scalar_inner::<true>(data, init)
    } else {
        wal_checksum_scalar_inner::<false>(data, init)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn wal_checksum_sse2_inner<const SWAP: bool>(data: &[u8], init: [u32; 2]) -> [u32; 2] {
    let n = data.len() & !63;
    if n == 0 {
        return wal_checksum_scalar_inner::<SWAP>(data, init);
    }
    let mut y1 = _mm_setr_epi32(0, 0, 0, init[0] as i32);
    let mut y2 = _mm_setr_epi32(0, 0, 0, init[1] as i32);
    let ptr = data.as_ptr();
    let mut off = 0usize;
    while off + 32 <= n {
        // SAFETY: `off + 32 <= n <= data.len()`, so both 16-byte loads at `off` and `off + 16`
        // read strictly within `data`.
        let mut v0 = unsafe { _mm_loadu_si128(ptr.add(off).cast::<__m128i>()) };
        // SAFETY: `off + 32 <= data.len()`.
        let mut v1 = unsafe { _mm_loadu_si128(ptr.add(off + 16).cast::<__m128i>()) };
        if SWAP {
            let b0 = _mm_or_si128(_mm_slli_epi16::<8>(v0), _mm_srli_epi16::<8>(v0));
            v0 = _mm_shufflelo_epi16::<0xB1>(_mm_shufflehi_epi16::<0xB1>(b0));
            let b1 = _mm_or_si128(_mm_slli_epi16::<8>(v1), _mm_srli_epi16::<8>(v1));
            v1 = _mm_shufflelo_epi16::<0xB1>(_mm_shufflehi_epi16::<0xB1>(b1));
        }
        let a = _mm_castps_si128(_mm_shuffle_ps::<0x88>(
            _mm_castsi128_ps(v0),
            _mm_castsi128_ps(v1),
        ));
        let b = _mm_castps_si128(_mm_shuffle_ps::<0xDD>(
            _mm_castsi128_ps(v0),
            _mm_castsi128_ps(v1),
        ));
        for _ in 0..4 {
            let t = _mm_add_epi32(y1, y2);
            y2 = _mm_add_epi32(t, y2);
            y1 = t;
        }
        y1 = _mm_add_epi32(y1, a);
        y2 = _mm_add_epi32(y2, _mm_add_epi32(a, b));
        off += 32;
    }
    let mut u1 = [0u32; 4];
    let mut u2 = [0u32; 4];
    // SAFETY: `u1` and `u2` are 16-byte stack arrays `[u32; 4]`.
    unsafe {
        _mm_storeu_si128(u1.as_mut_ptr().cast::<__m128i>(), y1);
        _mm_storeu_si128(u2.as_mut_ptr().cast::<__m128i>(), y2);
    }
    let (mut s1, mut s2) = (0u32, 0u32);
    for l in 0..4 {
        let [p, q, r] = WAL_TPOW4[3 - l];
        s1 = s1
            .wrapping_add(p.wrapping_mul(u1[l]))
            .wrapping_add(q.wrapping_mul(u2[l]));
        s2 = s2
            .wrapping_add(q.wrapping_mul(u1[l]))
            .wrapping_add(r.wrapping_mul(u2[l]));
    }
    if n == data.len() {
        [s1, s2]
    } else {
        wal_checksum_scalar_inner::<SWAP>(&data[n..], [s1, s2])
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn wal_checksum_sse2(data: &[u8], init: [u32; 2], swap_bytes: bool) -> [u32; 2] {
    if swap_bytes {
        wal_checksum_sse2_inner::<true>(data, init)
    } else {
        wal_checksum_sse2_inner::<false>(data, init)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn wal_checksum_avx2_inner<const SWAP: bool>(data: &[u8], init: [u32; 2]) -> [u32; 2] {
    let n = data.len() & !127;
    if n == 0 {
        return wal_checksum_scalar_inner::<SWAP>(data, init);
    }
    let [p, q, r] = wal_tpow(16);
    let c1 = _mm256_setr_epi32(
        p as i32, r as i32, p as i32, r as i32, p as i32, r as i32, p as i32, r as i32,
    );
    let qv = _mm256_set1_epi32(q as i32);
    let odd = _mm256_setr_epi32(0, -1, 0, -1, 0, -1, 0, -1);
    let bs = _mm256_setr_epi8(
        3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12, 3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8,
        15, 14, 13, 12,
    );
    let mut y = [_mm256_setzero_si256(); 4];
    y[3] = _mm256_setr_epi32(0, 0, 0, 0, 0, 0, init[0] as i32, init[1] as i32);
    let ptr = data.as_ptr();
    let mut off = 0usize;
    while off < n {
        for (k, yk) in y.iter_mut().enumerate() {
            // SAFETY: `off < n = data.len() & !127` and `k < 4`, so `off + 32 * k + 32 <= n <= data.len()`.
            let mut wv = unsafe { _mm256_loadu_si256(ptr.add(off + 32 * k).cast::<__m256i>()) };
            if SWAP {
                wv = _mm256_shuffle_epi8(wv, bs);
            }
            let xt = _mm256_add_epi32(wv, _mm256_and_si256(_mm256_shuffle_epi32::<0xB1>(wv), odd));
            let sy = _mm256_mullo_epi32(_mm256_shuffle_epi32::<0xB1>(*yk), qv);
            *yk = _mm256_add_epi32(_mm256_add_epi32(_mm256_mullo_epi32(*yk, c1), sy), xt);
        }
        off += 128;
    }
    let (m1, m2) = &WAL_T16_TABLES;
    let mut a1 = _mm256_setzero_si256();
    let mut a2 = _mm256_setzero_si256();
    for (k, &yk) in y.iter().enumerate() {
        // SAFETY: `m1` and `m2` have length 32 `u32`s (`128` bytes), and `8 * k + 8 <= 32` for `k < 4`.
        let v1 = unsafe { _mm256_loadu_si256(m1.as_ptr().add(8 * k).cast::<__m256i>()) };
        // SAFETY: `8 * k + 8 <= 32` for `k < 4`.
        let v2 = unsafe { _mm256_loadu_si256(m2.as_ptr().add(8 * k).cast::<__m256i>()) };
        a1 = _mm256_add_epi32(a1, _mm256_mullo_epi32(yk, v1));
        a2 = _mm256_add_epi32(a2, _mm256_mullo_epi32(yk, v2));
    }
    let mut t1 = [0u32; 8];
    let mut t2 = [0u32; 8];
    // SAFETY: `t1` and `t2` are 32-byte stack arrays `[u32; 8]`.
    unsafe {
        _mm256_storeu_si256(t1.as_mut_ptr().cast::<__m256i>(), a1);
        _mm256_storeu_si256(t2.as_mut_ptr().cast::<__m256i>(), a2);
    }
    let s1 = t1.iter().fold(0u32, |a, &b| a.wrapping_add(b));
    let s2 = t2.iter().fold(0u32, |a, &b| a.wrapping_add(b));
    if n == data.len() {
        [s1, s2]
    } else {
        wal_checksum_scalar_inner::<SWAP>(&data[n..], [s1, s2])
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn wal_checksum_avx2(data: &[u8], init: [u32; 2], swap_bytes: bool) -> [u32; 2] {
    if swap_bytes {
        wal_checksum_avx2_inner::<true>(data, init)
    } else {
        wal_checksum_avx2_inner::<false>(data, init)
    }
}

/// Computes SQLite's cumulative 8-byte WAL checksum (`walChecksumBytes`) over `data` (whose length
/// is a multiple of 8 bytes), optionally byte-swapping each 32-bit word from host byte order when
/// `swap_bytes` is `true`.
#[inline(always)]
pub fn wal_checksum_simd(data: &[u8], init: [u32; 2], swap_bytes: bool) -> [u32; 2] {
    debug_assert!(data.len().is_multiple_of(8));
    if data.len() < 128 {
        return wal_checksum_scalar(data, init, swap_bytes);
    }
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { wal_checksum_avx2(data, init, swap_bytes) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { wal_checksum_sse2(data, init, swap_bytes) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        wal_checksum_scalar(data, init, swap_bytes)
    }
}

// ============================================================================
// SIMD-4: simdjson-Style Stage-1 Structural Bitmasks & Quote/Escape/Control
//         Scanner (`crates/sqliters-types/src/json.rs`)
// ============================================================================

/// 64-byte Stage-1 SIMD classification bitmasks for pure printable-ASCII (`0x20..=0x7F` without
/// `'\\'`) JSON windows. For a window of length `L <= 64`, bits `L..64` are guaranteed zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonStage1Masks {
    /// Bit `k` is `1` iff `window[k] == b'"'`.
    pub quote_mask: u64,
    /// Bit `k` is `1` iff `window[k]` is in `b'0'..=b'9'`.
    pub digit_mask: u64,
    /// Bit `k` is `1` iff `window[k] != b' '`.
    pub non_space_mask: u64,
}

#[inline(always)]
fn scan_json_plain_ascii_scalar(rem: &[u8]) -> usize {
    for (i, &c) in rem.iter().enumerate() {
        if !(0x20..0x80).contains(&c) || c == b'"' || c == b'\\' {
            return i;
        }
    }
    rem.len()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn scan_json_plain_ascii_sse2_tail(rem: &[u8], mut i: usize) -> usize {
    let n = rem.len();
    let v20 = _mm_set1_epi8(0x20);
    let vq = _mm_set1_epi8(b'"' as i8);
    let vb = _mm_set1_epi8(b'\\' as i8);
    let ptr = rem.as_ptr();
    while i + 16 <= n {
        // SAFETY: `i + 16 <= rem.len()`, so `ptr.add(i)` is valid for a 16-byte unaligned load.
        let v = unsafe { _mm_loadu_si128(ptr.add(i).cast::<__m128i>()) };
        let stop = _mm_or_si128(
            _mm_cmpgt_epi8(v20, v),
            _mm_or_si128(_mm_cmpeq_epi8(v, vq), _mm_cmpeq_epi8(v, vb)),
        );
        let m = _mm_movemask_epi8(stop) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 16;
    }
    if i < n {
        let tail_i = n - 16;
        // SAFETY: `n >= 16` (callers only enter when `rem.len() >= 16`) and `tail_i = n - 16`.
        let v = unsafe { _mm_loadu_si128(ptr.add(tail_i).cast::<__m128i>()) };
        let stop = _mm_or_si128(
            _mm_cmpgt_epi8(v20, v),
            _mm_or_si128(_mm_cmpeq_epi8(v, vq), _mm_cmpeq_epi8(v, vb)),
        );
        let m = (_mm_movemask_epi8(stop) as u32) & (!0u32 << (i - tail_i));
        if m != 0 {
            return tail_i + m.trailing_zeros() as usize;
        }
    }
    n
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn scan_json_plain_ascii_avx2_tail(rem: &[u8], mut i: usize) -> usize {
    let n = rem.len();
    if n - i < 32 {
        return scan_json_plain_ascii_sse2_tail(rem, i);
    }
    let v20 = _mm256_set1_epi8(0x20);
    let vq = _mm256_set1_epi8(b'"' as i8);
    let vb = _mm256_set1_epi8(b'\\' as i8);
    let ptr = rem.as_ptr();
    while i + 32 <= n {
        // SAFETY: `i + 32 <= rem.len()`, so `ptr.add(i)` is valid for a 32-byte unaligned load.
        let v = unsafe { _mm256_loadu_si256(ptr.add(i).cast::<__m256i>()) };
        let stop = _mm256_or_si256(
            _mm256_cmpgt_epi8(v20, v),
            _mm256_or_si256(_mm256_cmpeq_epi8(v, vq), _mm256_cmpeq_epi8(v, vb)),
        );
        let m = _mm256_movemask_epi8(stop) as u32;
        if m != 0 {
            return i + m.trailing_zeros() as usize;
        }
        i += 32;
    }
    if i < n {
        let tail_i = n - 32;
        // SAFETY: `n >= 32` (`n - initial_i >= 32`) and `tail_i = n - 32`.
        let v = unsafe { _mm256_loadu_si256(ptr.add(tail_i).cast::<__m256i>()) };
        let stop = _mm256_or_si256(
            _mm256_cmpgt_epi8(v20, v),
            _mm256_or_si256(_mm256_cmpeq_epi8(v, vq), _mm256_cmpeq_epi8(v, vb)),
        );
        let m = (_mm256_movemask_epi8(stop) as u32) & (!0u32 << (i - tail_i));
        if m != 0 {
            return tail_i + m.trailing_zeros() as usize;
        }
    }
    n
}

/// Scans `rem` for the first byte outside plain unescaped JSON ASCII (`!(0x20..0x80)` or `b'"'`
/// or `b'\\'`), returning its byte offset or `rem.len()` if all bytes are plain ASCII.
///
/// Even for short 4..8 byte JSON keys, `&doc_bytes[pos..]` in a `>= 16`-byte JSON document still
/// has `>= 16` readable bytes, allowing a single inline 16-byte SSE2 probe to find the closing
/// quote in one vector step without looping.
#[inline(always)]
pub fn scan_json_plain_ascii_string(rem: &[u8]) -> usize {
    #[cfg(target_arch = "x86_64")]
    {
        if rem.len() < 16 {
            return scan_json_plain_ascii_scalar(rem);
        }
        // Baseline `x86_64` SSE2 fast probe on the first 16 bytes (resolves 95%+ of JSON keys and
        // short string values immediately without a function call or `ymm` transition).
        // SAFETY: `rem.len() >= 16`, and `sse2` is baseline on `x86_64`.
        let m = unsafe {
            let v = _mm_loadu_si128(rem.as_ptr().cast::<__m128i>());
            let stop = _mm_or_si128(
                _mm_cmpgt_epi8(_mm_set1_epi8(0x20), v),
                _mm_or_si128(
                    _mm_cmpeq_epi8(v, _mm_set1_epi8(b'"' as i8)),
                    _mm_cmpeq_epi8(v, _mm_set1_epi8(b'\\' as i8)),
                ),
            );
            _mm_movemask_epi8(stop) as u32
        };
        if m != 0 {
            return m.trailing_zeros() as usize;
        }
        if rem.len() == 16 {
            return 16;
        }
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { scan_json_plain_ascii_avx2_tail(rem, 16) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { scan_json_plain_ascii_sse2_tail(rem, 16) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        scan_json_plain_ascii_scalar(rem)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn classify_stage1_16_sse2(ptr: *const u8) -> Option<(u64, u64, u64)> {
    // SAFETY: Caller guarantees `ptr` is valid for a 16-byte unaligned read.
    let v = unsafe { _mm_loadu_si128(ptr.cast::<__m128i>()) };
    let bad = _mm_or_si128(
        _mm_cmpgt_epi8(_mm_set1_epi8(0x20), v),
        _mm_cmpeq_epi8(v, _mm_set1_epi8(b'\\' as i8)),
    );
    if _mm_movemask_epi8(bad) != 0 {
        return None;
    }
    let q = (_mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8(b'"' as i8))) as u32) as u64;
    let dt = _mm_sub_epi8(v, _mm_set1_epi8(b'0' as i8));
    let d =
        (_mm_movemask_epi8(_mm_cmpeq_epi8(_mm_min_epu8(dt, _mm_set1_epi8(9)), dt)) as u32) as u64;
    let sp = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8(b' ' as i8))) as u32;
    let ns = ((!sp) & 0xFFFF) as u64;
    Some((q, d, ns))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn classify_json_stage1_sse2(win: &[u8]) -> Option<JsonStage1Masks> {
    let n = win.len();
    debug_assert!((16..=64).contains(&n));
    let ptr = win.as_ptr();
    if n == 16 {
        let (q, d, ns) = classify_stage1_16_sse2(ptr)?;
        return Some(JsonStage1Masks {
            quote_mask: q,
            digit_mask: d,
            non_space_mask: ns,
        });
    }
    let mut quote_mask = 0u64;
    let mut digit_mask = 0u64;
    let mut non_space_mask = 0u64;
    let mut off = 0usize;
    while off + 16 <= n {
        // SAFETY: `off + 16 <= n == win.len()`.
        let (q, d, ns) = classify_stage1_16_sse2(unsafe { ptr.add(off) })?;
        quote_mask |= q << off;
        digit_mask |= d << off;
        non_space_mask |= ns << off;
        off += 16;
    }
    if off < n {
        let tail = n - 16;
        // SAFETY: `n > 16` and `tail = n - 16`, so `ptr.add(tail)` reads `win[n - 16..n]`.
        let (q, d, ns) = classify_stage1_16_sse2(unsafe { ptr.add(tail) })?;
        quote_mask |= q << tail;
        digit_mask |= d << tail;
        non_space_mask |= ns << tail;
    }
    Some(JsonStage1Masks {
        quote_mask,
        digit_mask,
        non_space_mask,
    })
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn classify_stage1_32_avx2(ptr: *const u8) -> Option<(u64, u64, u64)> {
    // SAFETY: Caller guarantees `ptr` is valid for a 32-byte unaligned read.
    let v = unsafe { _mm256_loadu_si256(ptr.cast::<__m256i>()) };
    let bad = _mm256_or_si256(
        _mm256_cmpgt_epi8(_mm256_set1_epi8(0x20), v),
        _mm256_cmpeq_epi8(v, _mm256_set1_epi8(b'\\' as i8)),
    );
    if _mm256_movemask_epi8(bad) != 0 {
        return None;
    }
    let q =
        (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(b'"' as i8))) as u32) as u64;
    let dt = _mm256_sub_epi8(v, _mm256_set1_epi8(b'0' as i8));
    let d = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(
        _mm256_min_epu8(dt, _mm256_set1_epi8(9)),
        dt,
    )) as u32) as u64;
    let sp = _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8(b' ' as i8))) as u32;
    let ns = (!sp) as u64;
    Some((q, d, ns))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn classify_json_stage1_avx2(win: &[u8]) -> Option<JsonStage1Masks> {
    let n = win.len();
    debug_assert!((16..=64).contains(&n));
    if n < 32 {
        return classify_json_stage1_sse2(win);
    }
    let ptr = win.as_ptr();
    let (q0, d0, ns0) = classify_stage1_32_avx2(ptr)?;
    if n == 32 {
        return Some(JsonStage1Masks {
            quote_mask: q0,
            digit_mask: d0,
            non_space_mask: ns0,
        });
    }
    let tail = n - 32;
    // SAFETY: `32 < n <= 64` and `tail = n - 32`, so `ptr.add(tail)` reads `win[n - 32..n]`.
    let (q1, d1, ns1) = classify_stage1_32_avx2(unsafe { ptr.add(tail) })?;
    Some(JsonStage1Masks {
        quote_mask: q0 | (q1 << tail),
        digit_mask: d0 | (d1 << tail),
        non_space_mask: ns0 | (ns1 << tail),
    })
}

/// Classifies a `16..=64`-byte window starting at `offset` (sliding back to `bytes.len() - 64`
/// near EOF when `bytes.len() > 64`) into `simdjson`-style Stage-1 `quote_mask`, `digit_mask`,
/// and `non_space_mask` bitmasks using AVX2/SSE2.
///
/// Returns `Some((window_base, window_end, masks))` if `bytes.len() >= 16` and every byte in the
/// window is printable unescaped ASCII (`0x20..=0x7F` and `!= b'\\'`); otherwise returns `None`.
#[inline]
pub fn classify_json_stage1_at(
    bytes: &[u8],
    offset: usize,
) -> Option<(usize, usize, JsonStage1Masks)> {
    #[cfg(target_arch = "x86_64")]
    {
        let total = bytes.len();
        if total < 16 || offset >= total {
            return None;
        }
        let (base, end) = if total <= 64 {
            (0usize, total)
        } else if offset + 64 <= total {
            (offset, offset + 64)
        } else {
            (total - 64, total)
        };
        let win = &bytes[base..end];
        let masks = if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            unsafe { classify_json_stage1_avx2(win) }?
        } else {
            // SAFETY: `sse2` is baseline on `x86_64`.
            unsafe { classify_json_stage1_sse2(win) }?
        };
        Some((base, end, masks))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (bytes, offset);
        None
    }
}

// ============================================================================
// SIMD-5: AVX2/SSE2 2D `f32` R-Tree Bounding-Box Intersection, Union,
//         Area Enlargement, Split Metrics, and Big-Endian Codec
// ============================================================================

#[inline(always)]
pub fn rtree_bswap_u32x4(src: &[u8; 16]) -> [u32; 4] {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            #[target_feature(enable = "avx2")]
            fn imp_avx2(src: &[u8; 16]) -> [u32; 4] {
                // SAFETY: `src` is a 16-byte array.
                let v = unsafe { _mm_loadu_si128(src.as_ptr().cast::<__m128i>()) };
                let bs = _mm_setr_epi8(3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12);
                let s = _mm_shuffle_epi8(v, bs);
                let mut out = [0u32; 4];
                // SAFETY: `out` is a 16-byte `[u32; 4]` array.
                unsafe { _mm_storeu_si128(out.as_mut_ptr().cast::<__m128i>(), s) };
                out
            }
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { imp_avx2(src) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`, and `src`/`out` are 16 bytes.
        unsafe {
            let v = _mm_loadu_si128(src.as_ptr().cast::<__m128i>());
            let b = _mm_or_si128(_mm_slli_epi16::<8>(v), _mm_srli_epi16::<8>(v));
            let s = _mm_shufflelo_epi16::<0xB1>(_mm_shufflehi_epi16::<0xB1>(b));
            let mut out = [0u32; 4];
            _mm_storeu_si128(out.as_mut_ptr().cast::<__m128i>(), s);
            out
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        [
            u32::from_be_bytes([src[0], src[1], src[2], src[3]]),
            u32::from_be_bytes([src[4], src[5], src[6], src[7]]),
            u32::from_be_bytes([src[8], src[9], src[10], src[11]]),
            u32::from_be_bytes([src[12], src[13], src[14], src[15]]),
        ]
    }
}

#[inline(always)]
pub fn rtree_encode_u32x4_be(coords: &[u32; 4], dst: &mut [u8; 16]) {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            #[target_feature(enable = "avx2")]
            fn imp_avx2(coords: &[u32; 4], dst: &mut [u8; 16]) {
                // SAFETY: `coords` is a 16-byte `[u32; 4]` array.
                let v = unsafe { _mm_loadu_si128(coords.as_ptr().cast::<__m128i>()) };
                let bs = _mm_setr_epi8(3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12);
                let s = _mm_shuffle_epi8(v, bs);
                // SAFETY: `dst` is a 16-byte `[u8; 16]` array.
                unsafe { _mm_storeu_si128(dst.as_mut_ptr().cast::<__m128i>(), s) };
            }
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            unsafe { imp_avx2(coords, dst) };
            return;
        }
        // SAFETY: `sse2` is baseline on `x86_64`, and `coords`/`dst` are 16 bytes.
        unsafe {
            let v = _mm_loadu_si128(coords.as_ptr().cast::<__m128i>());
            let b = _mm_or_si128(_mm_slli_epi16::<8>(v), _mm_srli_epi16::<8>(v));
            let s = _mm_shufflelo_epi16::<0xB1>(_mm_shufflehi_epi16::<0xB1>(b));
            _mm_storeu_si128(dst.as_mut_ptr().cast::<__m128i>(), s);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        dst[0..4].copy_from_slice(&coords[0].to_be_bytes());
        dst[4..8].copy_from_slice(&coords[1].to_be_bytes());
        dst[8..12].copy_from_slice(&coords[2].to_be_bytes());
        dst[12..16].copy_from_slice(&coords[3].to_be_bytes());
    }
}

/// Computes 2D `f32` bounding-box union `[min(a0,b0), max(a1,b1), min(a2,b2), max(a3,b3)]`
/// in place with IEEE-754 bit-exactness matching `if a0 >= b0 { b0 } else { a0 }` and
/// `if a1 <= b1 { b1 } else { a1 }`.
#[inline(always)]
pub fn rtree_2d_f32_union_in_place(a: &mut [u32; 4], b: &[u32; 4]) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `sse2` is baseline on `x86_64`; `a` and `b` are 16-byte `[u32; 4]` arrays.
        unsafe {
            let va = _mm_loadu_ps(a.as_ptr().cast::<f32>());
            let vb = _mm_loadu_ps(b.as_ptr().cast::<f32>());
            let vmin = _mm_min_ps(va, vb);
            let vmax = _mm_max_ps(va, vb);
            let mask = _mm_castsi128_ps(_mm_setr_epi32(0, -1, 0, -1));
            let res = _mm_or_ps(_mm_andnot_ps(mask, vmin), _mm_and_ps(mask, vmax));
            _mm_storeu_ps(a.as_mut_ptr().cast::<f32>(), res);
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        if f32::from_bits(a[0]) >= f32::from_bits(b[0]) {
            a[0] = b[0];
        }
        if f32::from_bits(a[1]) <= f32::from_bits(b[1]) {
            a[1] = b[1];
        }
        if f32::from_bits(a[2]) >= f32::from_bits(b[2]) {
            a[2] = b[2];
        }
        if f32::from_bits(a[3]) <= f32::from_bits(b[3]) {
            a[3] = b[3];
        }
    }
}

/// Checks whether 2D `f32` box `outer` contains `inner` (`!(inner_min < outer_min || inner_max > outer_max)`).
#[inline(always)]
pub fn rtree_2d_f32_contains(outer: &[u32; 4], inner: &[u32; 4]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `sse2` is baseline on `x86_64`; `outer` and `inner` are 16-byte `[u32; 4]` arrays.
        unsafe {
            let vo = _mm_loadu_ps(outer.as_ptr().cast::<f32>());
            let vi = _mm_loadu_ps(inner.as_ptr().cast::<f32>());
            let mask = _mm_castsi128_ps(_mm_setr_epi32(0, -1, 0, -1));
            let lhs = _mm_or_ps(_mm_andnot_ps(mask, vi), _mm_and_ps(mask, vo));
            let rhs = _mm_or_ps(_mm_andnot_ps(mask, vo), _mm_and_ps(mask, vi));
            _mm_movemask_ps(_mm_cmplt_ps(lhs, rhs)) == 0
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        !(f32::from_bits(inner[0]) < f32::from_bits(outer[0])
            || f32::from_bits(inner[1]) > f32::from_bits(outer[1])
            || f32::from_bits(inner[2]) < f32::from_bits(outer[2])
            || f32::from_bits(inner[3]) > f32::from_bits(outer[3]))
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn rtree_2d_f32_intersects_sse2(coords: &[u32; 4], q: &[f64; 4]) -> bool {
    // SAFETY: `coords` is `[u32; 4]` (16 bytes).
    let vf = unsafe { _mm_loadu_ps(coords.as_ptr().cast::<f32>()) };
    let vx = _mm_cvtps_pd(vf);
    let vy = _mm_cvtps_pd(_mm_movehl_ps(vf, vf));
    // `vx = [min_x, max_x]`, `vy = [min_y, max_y]`, `q = [q_min_x, q_max_x, q_min_y, q_max_y]`.
    let lx = _mm_shuffle_pd::<0b10>(_mm_set1_pd(q[1]), vx); // `[q_max_x, max_x]`
    let rx = _mm_shuffle_pd::<0b10>(vx, _mm_set1_pd(q[0])); // `[min_x, q_min_x]`
    let ly = _mm_shuffle_pd::<0b10>(_mm_set1_pd(q[3]), vy); // `[q_max_y, max_y]`
    let ry = _mm_shuffle_pd::<0b10>(vy, _mm_set1_pd(q[2])); // `[min_y, q_min_y]`
    let mx = _mm_movemask_pd(_mm_cmpge_pd(lx, rx));
    let my = _mm_movemask_pd(_mm_cmpge_pd(ly, ry));
    (mx & my) == 0b11
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn rtree_2d_f32_intersects_avx2(coords: &[u32; 4], q: &[f64; 4]) -> bool {
    // SAFETY: `coords` is `[u32; 4]` (16 bytes).
    let vf = unsafe { _mm_loadu_ps(coords.as_ptr().cast::<f32>()) };
    let vc = _mm256_cvtps_pd(vf);
    let q_max = _mm256_setr_pd(q[1], 0.0, q[3], 0.0);
    let q_min = _mm256_setr_pd(0.0, q[0], 0.0, q[2]);
    let lhs = _mm256_blend_pd::<0b1010>(q_max, vc);
    let rhs = _mm256_blend_pd::<0b1010>(vc, q_min);
    _mm256_movemask_pd(_mm256_cmp_pd::<_CMP_GE_OQ>(lhs, rhs)) == 0xF
}

/// Evaluates 2D R-Tree query overlap `max_x >= q[0] && min_x <= q[1] && max_y >= q[2] && min_y <= q[3]`
/// in a single vector `f32 -> f64` widening comparison without per-axis branches.
#[inline(always)]
pub fn rtree_2d_f32_intersects(coords: &[u32; 4], q_bounds: &[f64; 4]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            // SAFETY: `has_avx2()` verified runtime `avx2` support.
            return unsafe { rtree_2d_f32_intersects_avx2(coords, q_bounds) };
        }
        // SAFETY: `sse2` is baseline on `x86_64`.
        unsafe { rtree_2d_f32_intersects_sse2(coords, q_bounds) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let min_x = f64::from(f32::from_bits(coords[0]));
        let max_x = f64::from(f32::from_bits(coords[1]));
        let min_y = f64::from(f32::from_bits(coords[2]));
        let max_y = f64::from(f32::from_bits(coords[3]));
        max_x >= q_bounds[0] && min_x <= q_bounds[1] && max_y >= q_bounds[2] && min_y <= q_bounds[3]
    }
}

/// Computes `(area, enlargement)` for `choose_subtree` on 2D `f32` cells in `f64` precision.
#[inline(always)]
pub fn rtree_2d_f32_area_and_growth(c: &[u32; 4], target: &[u32; 4]) -> (f64, f64) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `sse2` is baseline on `x86_64`; `c` and `target` are 16-byte `[u32; 4]` arrays.
        unsafe {
            let vc = _mm_loadu_ps(c.as_ptr().cast::<f32>());
            let vt = _mm_loadu_ps(target.as_ptr().cast::<f32>());
            let vmin = _mm_min_ps(vc, vt);
            let vmax = _mm_max_ps(vc, vt);
            let mask = _mm_castsi128_ps(_mm_setr_epi32(0, -1, 0, -1));
            let vu = _mm_or_ps(_mm_andnot_ps(mask, vmin), _mm_and_ps(mask, vmax));

            let cx = _mm_cvtps_pd(vc);
            let cy = _mm_cvtps_pd(_mm_movehl_ps(vc, vc));
            let dx = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(cx, cx), cx));
            let dy = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(cy, cy), cy));
            let area = dx * dy;

            let ux = _mm_cvtps_pd(vu);
            let uy = _mm_cvtps_pd(_mm_movehl_ps(vu, vu));
            let udx = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(ux, ux), ux));
            let udy = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(uy, uy), uy));
            let u_area = udx * udy;

            (area, u_area - area)
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let mut u = *c;
        rtree_2d_f32_union_in_place(&mut u, target);
        let area = (f64::from(f32::from_bits(c[1])) - f64::from(f32::from_bits(c[0])))
            * (f64::from(f32::from_bits(c[3])) - f64::from(f32::from_bits(c[2])));
        let u_area = (f64::from(f32::from_bits(u[1])) - f64::from(f32::from_bits(u[0])))
            * (f64::from(f32::from_bits(u[3])) - f64::from(f32::from_bits(u[2])));
        (area, u_area - area)
    }
}

/// Computes `(margin_sum, overlap_area, area_sum)` for a candidate 2D `f32` R*-Tree split
/// `(left, right)` in a single `f32 -> f64` pass (replacing 6 redundant scalar conversions).
#[inline(always)]
pub fn rtree_2d_f32_split_metrics(left: &[u32; 4], right: &[u32; 4]) -> (f64, f64, f64) {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `sse2` is baseline on `x86_64`; `left` and `right` are 16-byte `[u32; 4]` arrays.
        unsafe {
            let vl_f = _mm_loadu_ps(left.as_ptr().cast::<f32>());
            let vr_f = _mm_loadu_ps(right.as_ptr().cast::<f32>());

            let lx = _mm_cvtps_pd(vl_f);
            let ly = _mm_cvtps_pd(_mm_movehl_ps(vl_f, vl_f));
            let rx = _mm_cvtps_pd(vr_f);
            let ry = _mm_cvtps_pd(_mm_movehl_ps(vr_f, vr_f));

            let ldx = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(lx, lx), lx));
            let ldy = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(ly, ly), ly));
            let rdx = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(rx, rx), rx));
            let rdy = _mm_cvtsd_f64(_mm_sub_sd(_mm_unpackhi_pd(ry, ry), ry));

            let margin = (ldx + ldy) + (rdx + rdy);
            let area = (ldx * ldy) + (rdx * rdy);

            let ox_lo = _mm_cvtsd_f64(_mm_max_sd(lx, rx));
            let ox_hi = _mm_cvtsd_f64(_mm_min_sd(_mm_unpackhi_pd(lx, lx), _mm_unpackhi_pd(rx, rx)));
            let overlap = if ox_hi < ox_lo {
                0.0
            } else {
                let oy_lo = _mm_cvtsd_f64(_mm_max_sd(ly, ry));
                let oy_hi =
                    _mm_cvtsd_f64(_mm_min_sd(_mm_unpackhi_pd(ly, ly), _mm_unpackhi_pd(ry, ry)));
                if oy_hi < oy_lo {
                    0.0
                } else {
                    (ox_hi - ox_lo) * (oy_hi - oy_lo)
                }
            };
            (margin, overlap, area)
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let lx0 = f64::from(f32::from_bits(left[0]));
        let lx1 = f64::from(f32::from_bits(left[1]));
        let ly0 = f64::from(f32::from_bits(left[2]));
        let ly1 = f64::from(f32::from_bits(left[3]));
        let rx0 = f64::from(f32::from_bits(right[0]));
        let rx1 = f64::from(f32::from_bits(right[1]));
        let ry0 = f64::from(f32::from_bits(right[2]));
        let ry1 = f64::from(f32::from_bits(right[3]));
        let ldx = lx1 - lx0;
        let ldy = ly1 - ly0;
        let rdx = rx1 - rx0;
        let rdy = ry1 - ry0;
        let margin = (ldx + ldy) + (rdx + rdy);
        let area = (ldx * ldy) + (rdx * rdy);
        let ox_lo = lx0.max(rx0);
        let ox_hi = lx1.min(rx1);
        let oy_lo = ly0.max(ry0);
        let oy_hi = ly1.min(ry1);
        let overlap = if ox_hi < ox_lo || oy_hi < oy_lo {
            0.0
        } else {
            (ox_hi - ox_lo) * (oy_hi - oy_lo)
        };
        (margin, overlap, area)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xorshift64(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn test_find1_and_find2_exhaustive_alignments_and_lengths() {
        let mut buf = vec![b'x'; 320];
        for align in 0..35usize {
            for len in 0..=200usize {
                let base = &mut buf[align..align + len];
                base.fill(b'.');
                assert_eq!(find1_byte(base, b'%'), len);
                assert_eq!(find2_byte(base, b'%', b'_'), len);
                assert_eq!(find2_byte(base, b'%', b'%'), len);

                for hit in 0..len {
                    base[hit] = if hit % 2 == 0 { b'%' } else { b'_' };
                    let want1 = find1_scalar(base, b'%');
                    let want2 = find2_scalar(base, b'%', b'_');
                    assert_eq!(find1_byte(base, b'%'), want1, "find1 len={len} hit={hit}");
                    assert_eq!(
                        find2_byte(base, b'%', b'_'),
                        want2,
                        "find2 len={len} hit={hit}"
                    );
                    #[cfg(target_arch = "x86_64")]
                    {
                        // SAFETY: `sse2` is baseline on `x86_64`.
                        unsafe {
                            assert_eq!(find1_sse2(base, b'%'), want1);
                            assert_eq!(find2_sse2(base, b'%', b'_'), want2);
                        }
                        if has_avx2() {
                            // SAFETY: `has_avx2()` checked above.
                            unsafe {
                                assert_eq!(find1_avx2(base, b'%'), want1);
                                assert_eq!(find2_avx2(base, b'%', b'_'), want2);
                            }
                        }
                    }
                    base[hit] = b'.';
                }
            }
        }
    }

    #[test]
    fn test_eq_and_contains_ascii_nocase_all_bytes_and_boundaries() {
        // Verify ASCII case-fold boundaries (`@`, `A..=Z`, `[`, `` ` ``, `a..=z`, `{`, `0x80..=0xFF`)
        // never false-match non-letters that differ by `0x20` (e.g. `@` vs `` ` ``, `[` vs `{`).
        for b1 in 0u8..=255 {
            let lower_b1 = b1.to_ascii_lowercase();
            for b2 in 0u8..=255 {
                let want = b2.to_ascii_lowercase() == lower_b1;
                let mut hay = [b'.'; 43];
                let mut needle = [b'.'; 43];
                hay[42] = b2;
                needle[42] = lower_b1;
                assert_eq!(
                    eq_ascii_nocase(&hay, &needle),
                    want,
                    "eq_ascii_nocase boundary b1={b1:#04x} b2={b2:#04x}"
                );
                let short_n = [b't', lower_b1, b'k'];
                let mut hay2 = [b'-'; 45];
                hay2[42] = b'T';
                hay2[43] = b2;
                hay2[44] = b'K';
                assert_eq!(
                    contains_ascii_nocase(&hay2, &short_n),
                    want,
                    "contains_ascii_nocase tail boundary b1={b1:#04x} b2={b2:#04x}"
                );
            }
        }

        // Sweep alignments, haystack lengths (including 43B W09 length and overlapping tails),
        // and needle lengths `0..=40` comparing SSE2, AVX2, and public dispatch vs scalar.
        let mut raw = vec![0u8; 320];
        let mut st = 0x1234_5678_9ABC_DEF0u64;
        for b in &mut raw {
            *b = (xorshift64(&mut st) & 0xFF) as u8;
        }
        let needles: &[&[u8]] = &[
            b"",
            b"a",
            b"7",
            b"ab",
            b"token42",
            b"@`[{",
            b"0123456789abcdef012",
            b"0123456789abcdef0123456789abcdef0123",
        ];
        for align in [0usize, 1, 7, 15, 16, 31] {
            for len in [
                0usize, 1, 2, 7, 15, 16, 17, 31, 32, 33, 38, 43, 63, 64, 65, 129,
            ] {
                let base = &mut raw[align..align + len];
                for &ndl in needles {
                    let want_nc = contains_ascii_nocase_scalar(base, ndl);
                    let want_cs = contains_bytes_scalar(base, ndl);
                    assert_eq!(contains_ascii_nocase(base, ndl), want_nc);
                    assert_eq!(contains_bytes(base, ndl), want_cs);
                    #[cfg(target_arch = "x86_64")]
                    {
                        // SAFETY: `sse2` is baseline on `x86_64`.
                        unsafe {
                            assert_eq!(contains_ascii_nocase_sse2(base, ndl), want_nc);
                            assert_eq!(contains_bytes_sse2(base, ndl), want_cs);
                        }
                        if has_avx2() {
                            // SAFETY: `has_avx2()` checked above.
                            unsafe {
                                assert_eq!(contains_ascii_nocase_avx2(base, ndl), want_nc);
                                assert_eq!(contains_bytes_avx2(base, ndl), want_cs);
                            }
                        }
                    }

                    if !ndl.is_empty() && ndl.len() <= len {
                        for pos in [0usize, (len - ndl.len()) / 2, len - ndl.len()] {
                            let saved = base[pos..pos + ndl.len()].to_vec();
                            for (k, &nb) in ndl.iter().enumerate() {
                                base[pos + k] = if k % 2 == 0 {
                                    nb.to_ascii_uppercase()
                                } else {
                                    nb
                                };
                            }
                            assert!(contains_ascii_nocase(base, ndl));
                            let want_cs_planted = contains_bytes_scalar(base, ndl);
                            assert_eq!(contains_bytes(base, ndl), want_cs_planted);
                            base[pos..pos + ndl.len()].copy_from_slice(&saved);

                            base[pos..pos + ndl.len()].copy_from_slice(ndl);
                            assert!(contains_bytes(base, ndl));
                            #[cfg(target_arch = "x86_64")]
                            {
                                // SAFETY: `sse2` is baseline on `x86_64`.
                                unsafe {
                                    assert!(contains_ascii_nocase_sse2(base, ndl));
                                    assert!(contains_bytes_sse2(base, ndl));
                                }
                                if has_avx2() {
                                    // SAFETY: `has_avx2()` checked above.
                                    unsafe {
                                        assert!(contains_ascii_nocase_avx2(base, ndl));
                                        assert!(contains_bytes_avx2(base, ndl));
                                    }
                                }
                            }
                            base[pos..pos + ndl.len()].copy_from_slice(&saved);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_wal_checksum_exhaustive() {
        let mut raw = vec![0u8; 8256];
        let mut st = 0x9E37_79B9_7F4A_7C15u64;
        for b in &mut raw {
            *b = (xorshift64(&mut st) >> 24) as u8;
        }
        let seeds = [
            [0, 0],
            [1, 2],
            [0xDEAD_BEEF, 0xCAFE_BABE],
            [u32::MAX, u32::MAX],
        ];
        for swap in [false, true] {
            for seed in seeds {
                for align in [0usize, 1, 3, 7, 13, 31] {
                    for len in (8..=8192).step_by(8) {
                        let slice = &raw[align..align + len];
                        let want = wal_checksum_scalar(slice, seed, swap);
                        let got = wal_checksum_simd(slice, seed, swap);
                        assert_eq!(
                            got, want,
                            "simd mismatch len={len} align={align} swap={swap}"
                        );
                        #[cfg(target_arch = "x86_64")]
                        {
                            // SAFETY: `sse2` is baseline on `x86_64`.
                            let got_sse2 = unsafe { wal_checksum_sse2(slice, seed, swap) };
                            assert_eq!(
                                got_sse2, want,
                                "sse2 mismatch len={len} align={align} swap={swap}"
                            );
                            if has_avx2() {
                                // SAFETY: `has_avx2()` checked above.
                                let got_avx2 = unsafe { wal_checksum_avx2(slice, seed, swap) };
                                assert_eq!(
                                    got_avx2, want,
                                    "avx2 mismatch len={len} align={align} swap={swap}"
                                );
                            }
                        }
                    }
                }
                // Verify cumulative seed chaining (`chunk0` then `chunk1` == single 4096B call).
                let page = &raw[5..5 + 4096];
                for split in [24usize, 32, 64, 128, 136, 1024, 3968] {
                    let mid = wal_checksum_simd(&page[..split], seed, swap);
                    let chained = wal_checksum_simd(&page[split..], mid, swap);
                    let whole = wal_checksum_scalar(page, seed, swap);
                    assert_eq!(chained, whole, "chained mismatch split={split} swap={swap}");
                }
            }
        }
    }

    #[test]
    fn test_json_string_scanner_and_stage1_masks_exhaustive() {
        let mut buf = vec![b'a'; 220];
        for align in [0usize, 1, 7, 15, 19, 31] {
            for len in 0..=150usize {
                let slice = &mut buf[align..align + len];
                slice.fill(b'a');
                assert_eq!(scan_json_plain_ascii_string(slice), len);
                for pos in 0..len {
                    for test_b in [
                        0x00u8, 0x09, 0x0A, 0x1F, 0x20, b'"', b'\\', b'0', b'9', 0x7F, 0x80, 0xFF,
                    ] {
                        slice[pos] = test_b;
                        let want = scan_json_plain_ascii_scalar(slice);
                        assert_eq!(
                            scan_json_plain_ascii_string(slice),
                            want,
                            "json scan len={len} pos={pos} b={test_b:#04x}"
                        );
                        #[cfg(target_arch = "x86_64")]
                        if len >= 16 {
                            // SAFETY: `sse2` is baseline on `x86_64` and `len >= 16`.
                            assert_eq!(unsafe { scan_json_plain_ascii_sse2_tail(slice, 0) }, want);
                            if has_avx2() {
                                // SAFETY: `has_avx2()` checked above.
                                assert_eq!(
                                    unsafe { scan_json_plain_ascii_avx2_tail(slice, 0) },
                                    want
                                );
                            }
                        }
                        slice[pos] = b'a';
                    }
                }
            }
        }

        // Stage-1 JSON mask parity against scalar reference across window sizes `16..=64`,
        // sliding offsets in `> 64`-byte documents, and rejection of `< 0x20`, `'\\'`, `>= 0x80`.
        let doc = br#"{"code":"J-0042","grp":17,"active":true,"tags":[10,205,3],"meta":{"tier":"gold","v":99}}"#;
        for total_len in 0..=doc.len() {
            let sub = &doc[..total_len];
            for off in 0..=total_len {
                let got = classify_json_stage1_at(sub, off);
                #[cfg(target_arch = "x86_64")]
                {
                    if total_len < 16 || off >= total_len {
                        assert!(got.is_none());
                    } else {
                        let (base, end, masks) = got.expect("pure ASCII JSON must classify");
                        let win = &sub[base..end];
                        let mut want_q = 0u64;
                        let mut want_d = 0u64;
                        let mut want_ns = 0u64;
                        for (i, &c) in win.iter().enumerate() {
                            if c == b'"' {
                                want_q |= 1u64 << i;
                            }
                            if c.is_ascii_digit() {
                                want_d |= 1u64 << i;
                            }
                            if c != b' ' {
                                want_ns |= 1u64 << i;
                            }
                        }
                        assert_eq!(masks.quote_mask, want_q);
                        assert_eq!(masks.digit_mask, want_d);
                        assert_eq!(masks.non_space_mask, want_ns);
                        // SAFETY: `sse2` is baseline on `x86_64`, `16 <= win.len() <= 64`.
                        let sse2_m = unsafe { classify_json_stage1_sse2(win) }.unwrap();
                        assert_eq!(sse2_m, masks);
                        if has_avx2() {
                            // SAFETY: `has_avx2()` checked above.
                            let avx2_m = unsafe { classify_json_stage1_avx2(win) }.unwrap();
                            assert_eq!(avx2_m, masks);
                        }
                    }
                }
            }
        }

        #[cfg(target_arch = "x86_64")]
        for bad_byte in [0x00u8, 0x0A, 0x1F, b'\\', 0x80, 0xFF] {
            for len in [16usize, 23, 32, 47, 64] {
                for pos in 0..len {
                    let mut win = vec![b' '; len];
                    win[pos] = bad_byte;
                    assert!(classify_json_stage1_at(&win, 0).is_none());
                    // SAFETY: `sse2` is baseline on `x86_64`.
                    assert!(unsafe { classify_json_stage1_sse2(&win) }.is_none());
                    if has_avx2() {
                        // SAFETY: `has_avx2()` checked above.
                        assert!(unsafe { classify_json_stage1_avx2(&win) }.is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn test_rtree_2d_f32_kernels_parity_and_signed_zero_exactness() {
        // Signed zero (`-0.0` vs `+0.0`) bit-exactness check for `rtree_2d_f32_union_in_place`.
        let pz = 0.0f32.to_bits();
        let nz = (-0.0f32).to_bits();
        for &(a0, b0) in &[(pz, nz), (nz, pz), (pz, pz), (nz, nz)] {
            let mut simd_box = [a0, a0, b0, b0];
            let rhs = [b0, b0, a0, a0];
            let mut want = simd_box;
            if f32::from_bits(want[0]) >= f32::from_bits(rhs[0]) {
                want[0] = rhs[0];
            }
            if f32::from_bits(want[1]) <= f32::from_bits(rhs[1]) {
                want[1] = rhs[1];
            }
            if f32::from_bits(want[2]) >= f32::from_bits(rhs[2]) {
                want[2] = rhs[2];
            }
            if f32::from_bits(want[3]) <= f32::from_bits(rhs[3]) {
                want[3] = rhs[3];
            }
            rtree_2d_f32_union_in_place(&mut simd_box, &rhs);
            assert_eq!(simd_box, want);
        }

        let mut st = 0xCAFE_BABE_1234_5678u64;
        let gen_box = |st: &mut u64| -> [u32; 4] {
            let x0 = ((xorshift64(st) % 20_000) as f32 - 10_000.0) * 0.125;
            let dx = ((xorshift64(st) % 5_000) as f32) * 0.125;
            let y0 = ((xorshift64(st) % 20_000) as f32 - 10_000.0) * 0.125;
            let dy = ((xorshift64(st) % 5_000) as f32) * 0.125;
            [
                x0.to_bits(),
                (x0 + dx).to_bits(),
                y0.to_bits(),
                (y0 + dy).to_bits(),
            ]
        };

        for _ in 0..2_000 {
            let a = gen_box(&mut st);
            let b = gen_box(&mut st);

            // Big-endian roundtrip
            let mut be = [0u8; 16];
            rtree_encode_u32x4_be(&a, &mut be);
            let want_be = [
                a[0].to_be_bytes(),
                a[1].to_be_bytes(),
                a[2].to_be_bytes(),
                a[3].to_be_bytes(),
            ]
            .concat();
            assert_eq!(&be[..], want_be.as_slice());
            assert_eq!(rtree_bswap_u32x4(&be), a);

            // Union
            let mut u = a;
            rtree_2d_f32_union_in_place(&mut u, &b);
            let want_u = [
                if f32::from_bits(a[0]) >= f32::from_bits(b[0]) {
                    b[0]
                } else {
                    a[0]
                },
                if f32::from_bits(a[1]) <= f32::from_bits(b[1]) {
                    b[1]
                } else {
                    a[1]
                },
                if f32::from_bits(a[2]) >= f32::from_bits(b[2]) {
                    b[2]
                } else {
                    a[2]
                },
                if f32::from_bits(a[3]) <= f32::from_bits(b[3]) {
                    b[3]
                } else {
                    a[3]
                },
            ];
            assert_eq!(u, want_u);
            assert!(rtree_2d_f32_contains(&u, &a));
            assert!(rtree_2d_f32_contains(&u, &b));

            // Contains
            let want_contains = !(f32::from_bits(b[0]) < f32::from_bits(a[0])
                || f32::from_bits(b[1]) > f32::from_bits(a[1])
                || f32::from_bits(b[2]) < f32::from_bits(a[2])
                || f32::from_bits(b[3]) > f32::from_bits(a[3]));
            assert_eq!(rtree_2d_f32_contains(&a, &b), want_contains);

            // Intersects (`f32` coords vs `f64` query bounds, including exact-edge touching)
            let q = [
                f64::from(f32::from_bits(b[0])),
                f64::from(f32::from_bits(b[1])),
                f64::from(f32::from_bits(b[2])),
                f64::from(f32::from_bits(b[3])),
            ];
            let want_inter = f64::from(f32::from_bits(a[1])) >= q[0]
                && f64::from(f32::from_bits(a[0])) <= q[1]
                && f64::from(f32::from_bits(a[3])) >= q[2]
                && f64::from(f32::from_bits(a[2])) <= q[3];
            assert_eq!(rtree_2d_f32_intersects(&a, &q), want_inter);
            #[cfg(target_arch = "x86_64")]
            {
                // SAFETY: `sse2` is baseline on `x86_64`.
                assert_eq!(unsafe { rtree_2d_f32_intersects_sse2(&a, &q) }, want_inter);
                if has_avx2() {
                    // SAFETY: `has_avx2()` checked above.
                    assert_eq!(unsafe { rtree_2d_f32_intersects_avx2(&a, &q) }, want_inter);
                }
            }

            // Area & growth + split metrics
            let (area, growth) = rtree_2d_f32_area_and_growth(&a, &b);
            let want_area = (f64::from(f32::from_bits(a[1])) - f64::from(f32::from_bits(a[0])))
                * (f64::from(f32::from_bits(a[3])) - f64::from(f32::from_bits(a[2])));
            let want_u_area = (f64::from(f32::from_bits(u[1])) - f64::from(f32::from_bits(u[0])))
                * (f64::from(f32::from_bits(u[3])) - f64::from(f32::from_bits(u[2])));
            assert_eq!(area.to_bits(), want_area.to_bits());
            assert_eq!(growth.to_bits(), (want_u_area - want_area).to_bits());

            let (margin, overlap, area_sum) = rtree_2d_f32_split_metrics(&a, &b);
            let lx0 = f64::from(f32::from_bits(a[0]));
            let lx1 = f64::from(f32::from_bits(a[1]));
            let ly0 = f64::from(f32::from_bits(a[2]));
            let ly1 = f64::from(f32::from_bits(a[3]));
            let rx0 = f64::from(f32::from_bits(b[0]));
            let rx1 = f64::from(f32::from_bits(b[1]));
            let ry0 = f64::from(f32::from_bits(b[2]));
            let ry1 = f64::from(f32::from_bits(b[3]));
            let want_margin = ((lx1 - lx0) + (ly1 - ly0)) + ((rx1 - rx0) + (ry1 - ry0));
            let want_area_sum = ((lx1 - lx0) * (ly1 - ly0)) + ((rx1 - rx0) * (ry1 - ry0));
            let ox_lo = lx0.max(rx0);
            let ox_hi = lx1.min(rx1);
            let oy_lo = ly0.max(ry0);
            let oy_hi = ly1.min(ry1);
            let want_overlap = if ox_hi < ox_lo || oy_hi < oy_lo {
                0.0
            } else {
                (ox_hi - ox_lo) * (oy_hi - oy_lo)
            };
            assert_eq!(margin.to_bits(), want_margin.to_bits());
            assert_eq!(overlap.to_bits(), want_overlap.to_bits());
            assert_eq!(area_sum.to_bits(), want_area_sum.to_bits());
        }
    }
}
