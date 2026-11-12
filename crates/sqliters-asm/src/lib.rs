#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
//! Zero-dependency leaf crate providing hand-written inline `x86-64` assembly (`core::arch::asm!`)
//! kernels behind 100% safe, slice- and value-validated Rust APIs, with plain idiomatic scalar
//! Rust fallbacks for sub-vector tails and non-`x86_64` targets.

#[cfg(target_arch = "x86_64")]
use core::arch::asm;
#[cfg(target_arch = "x86_64")]
use std::sync::atomic::{AtomicU8, Ordering};

#[cfg(target_arch = "x86_64")]
const CAP_SSE2: u8 = 1 << 0;
#[cfg(target_arch = "x86_64")]
const CAP_SSSE3: u8 = 1 << 1;
#[cfg(target_arch = "x86_64")]
const CAP_AVX2: u8 = 1 << 2;
#[cfg(target_arch = "x86_64")]
const CAP_BMI2: u8 = 1 << 3;
#[cfg(target_arch = "x86_64")]
const CAP_MOVBE: u8 = 1 << 4;
#[cfg(target_arch = "x86_64")]
const CAP_SSE41: u8 = 1 << 5;
#[cfg(target_arch = "x86_64")]
const CAP_INIT: u8 = 1 << 7;

#[cfg(target_arch = "x86_64")]
static CPU_CAPS: AtomicU8 = AtomicU8::new(0);

#[cfg(target_arch = "x86_64")]
#[cold]
#[inline(never)]
fn init_cpu_caps() -> u8 {
    let sse2 = std::arch::is_x86_feature_detected!("sse2");
    let ssse3 = sse2 && std::arch::is_x86_feature_detected!("ssse3");
    let sse41 = ssse3 && std::arch::is_x86_feature_detected!("sse4.1");
    let movbe = std::arch::is_x86_feature_detected!("movbe");
    let bmi1_lzcnt =
        std::arch::is_x86_feature_detected!("bmi1") && std::arch::is_x86_feature_detected!("lzcnt");
    let avx2 = sse41 && movbe && bmi1_lzcnt && std::arch::is_x86_feature_detected!("avx2");
    let bmi2 = movbe && bmi1_lzcnt && std::arch::is_x86_feature_detected!("bmi2");

    let mut mask = CAP_INIT;
    if sse2 {
        mask |= CAP_SSE2;
    }
    if ssse3 {
        mask |= CAP_SSSE3;
    }
    if avx2 {
        mask |= CAP_AVX2;
    }
    if bmi2 {
        mask |= CAP_BMI2;
    }
    if movbe {
        mask |= CAP_MOVBE;
    }
    if sse41 {
        mask |= CAP_SSE41;
    }
    CPU_CAPS.store(mask, Ordering::Relaxed);
    mask
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub(crate) fn cpu_caps() -> u8 {
    let c = CPU_CAPS.load(Ordering::Relaxed);
    if c != 0 {
        return c;
    }
    init_cpu_caps()
}

/// Returns the cached `x86_64` runtime feature bitmask (or `0` on non-`x86_64` targets).
#[inline]
pub fn detected_feature_mask() -> u8 {
    #[cfg(target_arch = "x86_64")]
    {
        cpu_caps() & !CAP_INIT
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

// ============================================================================
// ASM-1: Text / LIKE / GLOB / Folded Equality & Single/Dual-Byte Search
// ============================================================================

/// Plain scalar fallback for single-byte search (`memchr1`).
#[inline]
pub fn memchr1_scalar(haystack: &[u8], b0: u8) -> Option<usize> {
    for (i, &b) in haystack.iter().enumerate() {
        if b == b0 {
            return Some(i);
        }
    }
    None
}

/// Plain scalar fallback for dual-byte search (`memchr2`).
#[inline]
pub fn memchr2_scalar(haystack: &[u8], b0: u8, b1: u8) -> Option<usize> {
    for (i, &b) in haystack.iter().enumerate() {
        if b == b0 || b == b1 {
            return Some(i);
        }
    }
    None
}

/// Plain scalar fallback for exact slice equality.
#[inline]
pub fn eq_exact_scalar(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a[i] != b[i] {
            return false;
        }
    }
    true
}

/// Plain scalar fallback for ASCII case-folded equality where `lower_b` is already ASCII-lowercased.
#[inline]
pub fn eq_ascii_nocase_scalar(a: &[u8], lower_b: &[u8]) -> bool {
    if a.len() != lower_b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a[i].to_ascii_lowercase() != lower_b[i] {
            return false;
        }
    }
    true
}

/// Plain scalar fallback for exact substring containment (`memmem_exact`).
#[inline]
pub fn memmem_exact_scalar(haystack: &[u8], needle: &[u8]) -> bool {
    let m = needle.len();
    let n = haystack.len();
    if m == 0 {
        return true;
    }
    if n < m {
        return false;
    }
    let last = m - 1;
    let first_b = needle[0];
    let last_b = needle[last];
    let limit = n - m;
    for pos in 0..=limit {
        if haystack[pos + last] == last_b && haystack[pos] == first_b {
            let mut ok = true;
            for k in 1..last {
                if haystack[pos + k] != needle[k] {
                    ok = false;
                    break;
                }
            }
            if ok {
                return true;
            }
        }
    }
    false
}

/// Plain scalar fallback for ASCII case-insensitive substring containment where `lower_needle`
/// is already ASCII-lowercased (`memmem_ascii_nocase`).
#[inline]
pub fn memmem_ascii_nocase_scalar(haystack: &[u8], lower_needle: &[u8]) -> bool {
    let m = lower_needle.len();
    let n = haystack.len();
    if m == 0 {
        return true;
    }
    if n < m {
        return false;
    }
    let last = m - 1;
    let first_b = lower_needle[0];
    let last_b = lower_needle[last];
    let limit = n - m;
    for pos in 0..=limit {
        if haystack[pos + last].to_ascii_lowercase() == last_b
            && haystack[pos].to_ascii_lowercase() == first_b
        {
            let mut ok = true;
            for k in 1..last {
                if haystack[pos + k].to_ascii_lowercase() != lower_needle[k] {
                    ok = false;
                    break;
                }
            }
            if ok {
                return true;
            }
        }
    }
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1")]
unsafe fn memchr1_avx2(ptr: *const u8, len: usize, b0: u8) -> usize {
    debug_assert!(len >= 32);
    let limit32 = len - 32;
    let off = 0usize;
    let found: usize;
    let mask_tmp: u32;
    // SAFETY: Caller verified `CAP_AVX2` (`avx2 + bmi1`) and `len >= 32`, so `[ptr + off .. ptr + off + 32]`
    // is in-bounds for all `0 <= off <= len - 32`. All written GP and YMM registers are declared.
    unsafe {
        asm!(
            "vmovd {vb:x}, {b0:e}",
            "vpbroadcastb {vb}, {vb:x}",
            "2:",
            "vmovdqu {vt}, ymmword ptr [{ptr} + {off}]",
            "vpcmpeqb {vt}, {vt}, {vb}",
            "vpmovmskb {m:e}, {vt}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 32",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "tzcnt {m:e}, {m:e}",
            "lea {res}, [{off} + {m:r}]",
            "jmp 5f",
            "4:",
            "mov {res}, -1",
            "5:",
            "vzeroupper",
            ptr = in(reg) ptr,
            lim = in(reg) limit32,
            b0 = in(reg) u32::from(b0),
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) found,
            vb = out(ymm_reg) _,
            vt = out(ymm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    found
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn memchr1_sse2(ptr: *const u8, len: usize, b0: u8) -> usize {
    debug_assert!(len >= 16);
    let limit16 = len - 16;
    let off = 0usize;
    let found: usize;
    let mask_tmp: u32;
    let splat32 = u32::from(b0) * 0x0101_0101;
    // SAFETY: Caller verified `CAP_SSE2` and `len >= 16`, so `[ptr + off .. ptr + off + 16]`
    // is in-bounds for `0 <= off <= len - 16`. `bsf` only executes when `mask_tmp != 0`.
    unsafe {
        asm!(
            "movd {vb}, {b0:e}",
            "pshufd {vb}, {vb}, 0",
            "2:",
            "movdqu {vt}, xmmword ptr [{ptr} + {off}]",
            "pcmpeqb {vt}, {vb}",
            "pmovmskb {m:e}, {vt}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 16",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "bsf {m:e}, {m:e}",
            "lea {res}, [{off} + {m:r}]",
            "jmp 5f",
            "4:",
            "mov {res}, -1",
            "5:",
            ptr = in(reg) ptr,
            lim = in(reg) limit16,
            b0 = in(reg) splat32,
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) found,
            vb = out(xmm_reg) _,
            vt = out(xmm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    found
}

/// Finds the first index of `b0` in `haystack`.
#[inline]
pub fn memchr1(haystack: &[u8], b0: u8) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        let len = haystack.len();
        if len >= 32 {
            let caps = cpu_caps();
            if (caps & CAP_AVX2) != 0 {
                // SAFETY: `CAP_AVX2` is verified and `len >= 32`.
                let idx = unsafe { memchr1_avx2(haystack.as_ptr(), len, b0) };
                return (idx != usize::MAX).then_some(idx);
            }
            if (caps & CAP_SSE2) != 0 {
                // SAFETY: `CAP_SSE2` is verified and `len >= 16`.
                let idx = unsafe { memchr1_sse2(haystack.as_ptr(), len, b0) };
                return (idx != usize::MAX).then_some(idx);
            }
        } else if len >= 16 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified and `len >= 16`.
            let idx = unsafe { memchr1_sse2(haystack.as_ptr(), len, b0) };
            return (idx != usize::MAX).then_some(idx);
        }
    }
    memchr1_scalar(haystack, b0)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1")]
unsafe fn memchr2_avx2(ptr: *const u8, len: usize, b0: u8, b1: u8) -> usize {
    debug_assert!(len >= 32);
    let limit32 = len - 32;
    let off = 0usize;
    let found: usize;
    let mask_tmp: u32;
    // SAFETY: Caller verified `CAP_AVX2` (`avx2 + bmi1`) and `len >= 32`.
    unsafe {
        asm!(
            "vmovd {vb0:x}, {b0:e}",
            "vpbroadcastb {vb0}, {vb0:x}",
            "vmovd {vb1:x}, {b1:e}",
            "vpbroadcastb {vb1}, {vb1:x}",
            "2:",
            "vmovdqu {vt0}, ymmword ptr [{ptr} + {off}]",
            "vpcmpeqb {vt1}, {vt0}, {vb1}",
            "vpcmpeqb {vt0}, {vt0}, {vb0}",
            "vpor {vt0}, {vt0}, {vt1}",
            "vpmovmskb {m:e}, {vt0}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 32",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "tzcnt {m:e}, {m:e}",
            "lea {res}, [{off} + {m:r}]",
            "jmp 5f",
            "4:",
            "mov {res}, -1",
            "5:",
            "vzeroupper",
            ptr = in(reg) ptr,
            lim = in(reg) limit32,
            b0 = in(reg) u32::from(b0),
            b1 = in(reg) u32::from(b1),
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) found,
            vb0 = out(ymm_reg) _,
            vb1 = out(ymm_reg) _,
            vt0 = out(ymm_reg) _,
            vt1 = out(ymm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    found
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn memchr2_sse2(ptr: *const u8, len: usize, b0: u8, b1: u8) -> usize {
    debug_assert!(len >= 16);
    let limit16 = len - 16;
    let off = 0usize;
    let found: usize;
    let mask_tmp: u32;
    let splat0 = u32::from(b0) * 0x0101_0101;
    let splat1 = u32::from(b1) * 0x0101_0101;
    // SAFETY: Caller verified `CAP_SSE2` and `len >= 16`. `bsf` executes only when `mask_tmp != 0`.
    unsafe {
        asm!(
            "movd {vb0}, {b0:e}",
            "pshufd {vb0}, {vb0}, 0",
            "movd {vb1}, {b1:e}",
            "pshufd {vb1}, {vb1}, 0",
            "2:",
            "movdqu {vt0}, xmmword ptr [{ptr} + {off}]",
            "movdqa {vt1}, {vt0}",
            "pcmpeqb {vt0}, {vb0}",
            "pcmpeqb {vt1}, {vb1}",
            "por {vt0}, {vt1}",
            "pmovmskb {m:e}, {vt0}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 16",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "bsf {m:e}, {m:e}",
            "lea {res}, [{off} + {m:r}]",
            "jmp 5f",
            "4:",
            "mov {res}, -1",
            "5:",
            ptr = in(reg) ptr,
            lim = in(reg) limit16,
            b0 = in(reg) splat0,
            b1 = in(reg) splat1,
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) found,
            vb0 = out(xmm_reg) _,
            vb1 = out(xmm_reg) _,
            vt0 = out(xmm_reg) _,
            vt1 = out(xmm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    found
}

/// Finds the first index of either `b0` or `b1` in `haystack`.
#[inline]
pub fn memchr2(haystack: &[u8], b0: u8, b1: u8) -> Option<usize> {
    if b0 == b1 {
        return memchr1(haystack, b0);
    }
    #[cfg(target_arch = "x86_64")]
    {
        let len = haystack.len();
        if len >= 32 {
            let caps = cpu_caps();
            if (caps & CAP_AVX2) != 0 {
                // SAFETY: `CAP_AVX2` is verified and `len >= 32`.
                let idx = unsafe { memchr2_avx2(haystack.as_ptr(), len, b0, b1) };
                return (idx != usize::MAX).then_some(idx);
            }
            if (caps & CAP_SSE2) != 0 {
                // SAFETY: `CAP_SSE2` is verified and `len >= 16`.
                let idx = unsafe { memchr2_sse2(haystack.as_ptr(), len, b0, b1) };
                return (idx != usize::MAX).then_some(idx);
            }
        } else if len >= 16 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified and `len >= 16`.
            let idx = unsafe { memchr2_sse2(haystack.as_ptr(), len, b0, b1) };
            return (idx != usize::MAX).then_some(idx);
        }
    }
    memchr2_scalar(haystack, b0, b1)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn eq_exact_avx2(a: *const u8, b: *const u8, len: usize) -> bool {
    debug_assert!(len >= 32);
    let limit32 = len - 32;
    let off = 0usize;
    let res: u32;
    let mask_tmp: u32;
    // SAFETY: Caller verified `CAP_AVX2` and `len >= 32` for both `a` and `b`.
    unsafe {
        asm!(
            "2:",
            "vmovdqu {va}, ymmword ptr [{a} + {off}]",
            "vpcmpeqb {va}, {va}, ymmword ptr [{b} + {off}]",
            "vpmovmskb {m:e}, {va}",
            "cmp {m:e}, -1",
            "jne 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 32",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "xor {res:e}, {res:e}",
            "jmp 5f",
            "4:",
            "mov {res:e}, 1",
            "5:",
            "vzeroupper",
            a = in(reg) a,
            b = in(reg) b,
            lim = in(reg) limit32,
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) res,
            va = out(ymm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    res != 0
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn eq_exact_sse2(a: *const u8, b: *const u8, len: usize) -> bool {
    debug_assert!(len >= 16);
    let limit16 = len - 16;
    let off = 0usize;
    let res: u32;
    let mask_tmp: u32;
    // SAFETY: Caller verified `CAP_SSE2` and `len >= 16` for both `a` and `b`.
    unsafe {
        asm!(
            "2:",
            "movdqu {va}, xmmword ptr [{a} + {off}]",
            "movdqu {vb}, xmmword ptr [{b} + {off}]",
            "pcmpeqb {va}, {vb}",
            "pmovmskb {m:e}, {va}",
            "cmp {m:e}, 0xffff",
            "jne 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 16",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "xor {res:e}, {res:e}",
            "jmp 5f",
            "4:",
            "mov {res:e}, 1",
            "5:",
            a = in(reg) a,
            b = in(reg) b,
            lim = in(reg) limit16,
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) res,
            va = out(xmm_reg) _,
            vb = out(xmm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    res != 0
}

/// Exact byte-slice equality using inline `AVX2` / `SSE2` vector comparisons with plain scalar tail.
#[inline]
pub fn eq_exact(a: &[u8], b: &[u8]) -> bool {
    let len = a.len();
    if len != b.len() {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if len >= 32 {
            let caps = cpu_caps();
            if (caps & CAP_AVX2) != 0 {
                // SAFETY: `CAP_AVX2` is verified and both slices have length `len >= 32`.
                return unsafe { eq_exact_avx2(a.as_ptr(), b.as_ptr(), len) };
            }
            if (caps & CAP_SSE2) != 0 {
                // SAFETY: `CAP_SSE2` is verified and both slices have length `len >= 16`.
                return unsafe { eq_exact_sse2(a.as_ptr(), b.as_ptr(), len) };
            }
        } else if len >= 16 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified and both slices have length `len >= 16`.
            return unsafe { eq_exact_sse2(a.as_ptr(), b.as_ptr(), len) };
        }
    }
    eq_exact_scalar(a, b)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn eq_ascii_nocase_avx2(a: *const u8, lower_b: *const u8, len: usize) -> bool {
    debug_assert!(len >= 32);
    let limit32 = len - 32;
    let off = 0usize;
    let res: u32;
    let mask_tmp: u32;
    // Exact branchless `'A'..='Z'` lowercase fold:
    // `(byte + 0x3f)` maps `0x41..=0x5a` (`'A'..='Z'`) into signed `[-128, -103]`,
    // so signed `vpcmpgtb(0x9a /* -102 */, byte + 0x3f)` yields `0xFF` iff `'A' <= byte <= 'Z'`.
    // SAFETY: Caller verified `CAP_AVX2` and `len >= 32` for both `a` and `lower_b`.
    unsafe {
        asm!(
            "mov {m:e}, 0x3f3f3f3f",
            "vmovd {c3f:x}, {m:e}",
            "vpbroadcastd {c3f}, {c3f:x}",
            "mov {m:e}, 0x9a9a9a9a",
            "vmovd {c9a:x}, {m:e}",
            "vpbroadcastd {c9a}, {c9a:x}",
            "mov {m:e}, 0x20202020",
            "vmovd {c20:x}, {m:e}",
            "vpbroadcastd {c20}, {c20:x}",
            "2:",
            "vmovdqu {va}, ymmword ptr [{a} + {off}]",
            "vpaddb {vt}, {va}, {c3f}",
            "vpcmpgtb {vt}, {c9a}, {vt}",
            "vpand {vt}, {vt}, {c20}",
            "vpor {va}, {va}, {vt}",
            "vpcmpeqb {va}, {va}, ymmword ptr [{b} + {off}]",
            "vpmovmskb {m:e}, {va}",
            "cmp {m:e}, -1",
            "jne 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 32",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "xor {res:e}, {res:e}",
            "jmp 5f",
            "4:",
            "mov {res:e}, 1",
            "5:",
            "vzeroupper",
            a = in(reg) a,
            b = in(reg) lower_b,
            lim = in(reg) limit32,
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) res,
            va = out(ymm_reg) _,
            vt = out(ymm_reg) _,
            c3f = out(ymm_reg) _,
            c9a = out(ymm_reg) _,
            c20 = out(ymm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    res != 0
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn eq_ascii_nocase_sse2(a: *const u8, lower_b: *const u8, len: usize) -> bool {
    debug_assert!(len >= 16);
    let limit16 = len - 16;
    let off = 0usize;
    let res: u32;
    let mask_tmp: u32;
    // SAFETY: Caller verified `CAP_SSE2` and `len >= 16` for both `a` and `lower_b`.
    unsafe {
        asm!(
            "mov {m:e}, 0x3f3f3f3f",
            "movd {c3f}, {m:e}",
            "pshufd {c3f}, {c3f}, 0",
            "mov {m:e}, 0x9a9a9a9a",
            "movd {c9a}, {m:e}",
            "pshufd {c9a}, {c9a}, 0",
            "mov {m:e}, 0x20202020",
            "movd {c20}, {m:e}",
            "pshufd {c20}, {c20}, 0",
            "2:",
            "movdqu {va}, xmmword ptr [{a} + {off}]",
            "movdqa {vt}, {va}",
            "paddb {vt}, {c3f}",
            "movdqa {vu}, {c9a}",
            "pcmpgtb {vu}, {vt}",
            "pand {vu}, {c20}",
            "por {va}, {vu}",
            "movdqu {vt}, xmmword ptr [{b} + {off}]",
            "pcmpeqb {va}, {vt}",
            "pmovmskb {m:e}, {va}",
            "cmp {m:e}, 0xffff",
            "jne 3f",
            "cmp {off}, {lim}",
            "je 4f",
            "add {off}, 16",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "xor {res:e}, {res:e}",
            "jmp 5f",
            "4:",
            "mov {res:e}, 1",
            "5:",
            a = in(reg) a,
            b = in(reg) lower_b,
            lim = in(reg) limit16,
            off = inout(reg) off => _,
            m = out(reg) mask_tmp,
            res = out(reg) res,
            va = out(xmm_reg) _,
            vt = out(xmm_reg) _,
            vu = out(xmm_reg) _,
            c3f = out(xmm_reg) _,
            c9a = out(xmm_reg) _,
            c20 = out(xmm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = mask_tmp;
    res != 0
}

/// ASCII case-folded equality where `lower_b` already has `'A'..='Z'` mapped to `'a'..='z'`.
#[inline]
pub fn eq_ascii_nocase(a: &[u8], lower_b: &[u8]) -> bool {
    let len = a.len();
    if len != lower_b.len() {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if len >= 32 {
            let caps = cpu_caps();
            if (caps & CAP_AVX2) != 0 {
                // SAFETY: `CAP_AVX2` is verified and both slices have length `len >= 32`.
                return unsafe { eq_ascii_nocase_avx2(a.as_ptr(), lower_b.as_ptr(), len) };
            }
            if (caps & CAP_SSE2) != 0 {
                // SAFETY: `CAP_SSE2` is verified and both slices have length `len >= 16`.
                return unsafe { eq_ascii_nocase_sse2(a.as_ptr(), lower_b.as_ptr(), len) };
            }
        } else if len >= 16 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified and both slices have length `len >= 16`.
            return unsafe { eq_ascii_nocase_sse2(a.as_ptr(), lower_b.as_ptr(), len) };
        }
    }
    eq_ascii_nocase_scalar(a, lower_b)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1")]
unsafe fn memmem_exact_avx2(
    hay: *const u8,
    hay_len: usize,
    needle: *const u8,
    needle_len: usize,
) -> bool {
    debug_assert!(needle_len >= 2 && hay_len >= needle_len + 31);
    let positions = hay_len - needle_len + 1;
    let limit32 = positions - 32;
    let last_off = needle_len - 1;
    let mid_len = needle_len - 2;
    // SAFETY: `needle_len >= 2` is checked by caller.
    let first_b = unsafe { u32::from(*needle) };
    // SAFETY: `last_off = needle_len - 1` is in-bounds of `needle`.
    let last_b = unsafe { u32::from(*needle.add(last_off)) };
    let packed = first_b | (last_b << 8);
    // SAFETY: `needle + 1` is in-bounds (points to interior or one-past-first when `needle_len == 2`).
    let mid_ptr = unsafe { needle.add(1) };
    let hay_last = unsafe { hay.add(last_off) };

    let off = 0usize;
    let res: u32;
    let cand_ptr: *const u8;
    let k_idx: usize;
    let byte_tmp: u32;

    // SAFETY: Caller verified `CAP_AVX2` (`avx2 + bmi1`) and `positions >= 32`.
    // For every `0 <= off <= limit32`, `[hay + off .. hay + off + 32]` and
    // `[hay_last + off .. hay_last + off + 32]` lie strictly within `hay[..hay_len]`.
    unsafe {
        asm!(
            "vmovd {vf:x}, {m:e}",
            "vpbroadcastb {vf}, {vf:x}",
            "shr {m:e}, 8",
            "vmovd {vl:x}, {m:e}",
            "vpbroadcastb {vl}, {vl:x}",
            "2:",
            "vmovdqu {vt0}, ymmword ptr [{hay} + {off}]",
            "vmovdqu {vt1}, ymmword ptr [{hay_last} + {off}]",
            "vpcmpeqb {vt0}, {vt0}, {vf}",
            "vpcmpeqb {vt1}, {vt1}, {vl}",
            "vpand {vt0}, {vt0}, {vt1}",
            "vpmovmskb {m:e}, {vt0}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "4:",
            "cmp {off}, {lim}",
            "je 7f",
            "add {off}, 32",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "test {mid_len}, {mid_len}",
            "jz 8f",
            "5:",
            "tzcnt {cand:e}, {m:e}",
            "lea {cand}, [{hay} + {cand} + 1]",
            "add {cand}, {off}",
            "xor {k:e}, {k:e}",
            "6:",
            "movzx {bt:e}, byte ptr [{cand} + {k}]",
            "cmp {bt:l}, byte ptr [{mid_ptr} + {k}]",
            "jne 9f",
            "inc {k}",
            "cmp {k}, {mid_len}",
            "jb 6b",
            "jmp 8f",
            "9:",
            "blsr {m:e}, {m:e}",
            "jnz 5b",
            "jmp 4b",
            "7:",
            "xor {m:e}, {m:e}",
            "jmp 20f",
            "8:",
            "mov {m:e}, 1",
            "20:",
            "vzeroupper",
            hay = in(reg) hay,
            hay_last = in(reg) hay_last,
            mid_ptr = in(reg) mid_ptr,
            mid_len = in(reg) mid_len,
            lim = in(reg) limit32,
            off = inout(reg) off => _,
            m = inout(reg) packed => res,
            cand = out(reg) cand_ptr,
            k = out(reg) k_idx,
            bt = out(reg) byte_tmp,
            vf = out(ymm_reg) _,
            vl = out(ymm_reg) _,
            vt0 = out(ymm_reg) _,
            vt1 = out(ymm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = (cand_ptr, k_idx, byte_tmp);
    res != 0
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn memmem_exact_sse2(
    hay: *const u8,
    hay_len: usize,
    needle: *const u8,
    needle_len: usize,
) -> bool {
    debug_assert!(needle_len >= 2 && hay_len >= needle_len + 15);
    let positions = hay_len - needle_len + 1;
    let limit16 = positions - 16;
    let last_off = needle_len - 1;
    let mid_len = needle_len - 2;
    // SAFETY: `needle_len >= 2` is checked by caller.
    let first_b = unsafe { u32::from(*needle) };
    let last_b = unsafe { u32::from(*needle.add(last_off)) };
    let packed = first_b | (last_b << 8);
    let mid_ptr = unsafe { needle.add(1) };
    let hay_last = unsafe { hay.add(last_off) };

    let off = 0usize;
    let res: u32;
    let cand_ptr: *const u8;
    let k_idx: usize;
    let byte_tmp: u32;

    // SAFETY: Caller verified `CAP_SSE2` and `positions >= 16`. `bsf` runs only when `m != 0`.
    unsafe {
        asm!(
            "movzx {bt:e}, {m:l}",
            "imul {bt:e}, {bt:e}, 0x01010101",
            "movd {vf}, {bt:e}",
            "pshufd {vf}, {vf}, 0",
            "shr {m:e}, 8",
            "imul {m:e}, {m:e}, 0x01010101",
            "movd {vl}, {m:e}",
            "pshufd {vl}, {vl}, 0",
            "2:",
            "movdqu {vt0}, xmmword ptr [{hay} + {off}]",
            "movdqu {vt1}, xmmword ptr [{hay_last} + {off}]",
            "pcmpeqb {vt0}, {vf}",
            "pcmpeqb {vt1}, {vl}",
            "pand {vt0}, {vt1}",
            "pmovmskb {m:e}, {vt0}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "4:",
            "cmp {off}, {lim}",
            "je 7f",
            "add {off}, 16",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "test {mid_len}, {mid_len}",
            "jz 8f",
            "5:",
            "bsf {cand:e}, {m:e}",
            "lea {cand}, [{hay} + {cand} + 1]",
            "add {cand}, {off}",
            "xor {k:e}, {k:e}",
            "6:",
            "movzx {bt:e}, byte ptr [{cand} + {k}]",
            "cmp {bt:l}, byte ptr [{mid_ptr} + {k}]",
            "jne 9f",
            "inc {k}",
            "cmp {k}, {mid_len}",
            "jb 6b",
            "jmp 8f",
            "9:",
            "lea {bt:e}, [{m:r} - 1]",
            "and {m:e}, {bt:e}",
            "jnz 5b",
            "jmp 4b",
            "7:",
            "xor {m:e}, {m:e}",
            "jmp 20f",
            "8:",
            "mov {m:e}, 1",
            "20:",
            hay = in(reg) hay,
            hay_last = in(reg) hay_last,
            mid_ptr = in(reg) mid_ptr,
            mid_len = in(reg) mid_len,
            lim = in(reg) limit16,
            off = inout(reg) off => _,
            m = inout(reg) packed => res,
            cand = out(reg) cand_ptr,
            k = out(reg) k_idx,
            bt = out(reg) byte_tmp,
            vf = out(xmm_reg) _,
            vl = out(xmm_reg) _,
            vt0 = out(xmm_reg) _,
            vt1 = out(xmm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = (cand_ptr, k_idx, byte_tmp);
    res != 0
}

/// Exact substring containment (`GLOB '*lit*'` and case-sensitive `LIKE '%lit%'`) using
/// Muła first+last inline `AVX2` (32B + overlapping tail) / `SSE2` (16B + overlapping tail).
#[inline]
pub fn memmem_exact(haystack: &[u8], needle: &[u8]) -> bool {
    let m = needle.len();
    let n = haystack.len();
    if m == 0 {
        return true;
    }
    if n < m {
        return false;
    }
    if m == 1 {
        return memchr1(haystack, needle[0]).is_some();
    }
    #[cfg(target_arch = "x86_64")]
    {
        let positions = n - m + 1;
        if positions >= 32 {
            let caps = cpu_caps();
            if (caps & CAP_AVX2) != 0 {
                // SAFETY: `CAP_AVX2` is verified, `m >= 2`, and `positions >= 32`.
                return unsafe { memmem_exact_avx2(haystack.as_ptr(), n, needle.as_ptr(), m) };
            }
            if (caps & CAP_SSE2) != 0 {
                // SAFETY: `CAP_SSE2` is verified, `m >= 2`, and `positions >= 16`.
                return unsafe { memmem_exact_sse2(haystack.as_ptr(), n, needle.as_ptr(), m) };
            }
        } else if positions >= 16 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified, `m >= 2`, and `positions >= 16`.
            return unsafe { memmem_exact_sse2(haystack.as_ptr(), n, needle.as_ptr(), m) };
        }
    }
    memmem_exact_scalar(haystack, needle)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1")]
unsafe fn memmem_ascii_nocase_avx2(
    hay: *const u8,
    hay_len: usize,
    lower_needle: *const u8,
    needle_len: usize,
) -> bool {
    debug_assert!(needle_len >= 2 && hay_len >= needle_len + 31);
    let positions = hay_len - needle_len + 1;
    let limit32 = positions - 32;
    let last_off = needle_len - 1;
    let mid_len = needle_len - 2;
    // SAFETY: `needle_len >= 2` is checked by caller.
    let first_b = unsafe { *lower_needle };
    let last_b = unsafe { *lower_needle.add(last_off) };
    let fm_b = if first_b.is_ascii_lowercase() {
        0x20u32
    } else {
        0u32
    };
    let lm_b = if last_b.is_ascii_lowercase() {
        0x20u32
    } else {
        0u32
    };
    let packed = u32::from(first_b) | (u32::from(last_b) << 8) | (fm_b << 16) | (lm_b << 24);
    // SAFETY: `mlen >= 2` and `hlen >= mlen`, so `lower_needle.add(1)` and `hay.add(mlen - 1)`
    // remain within their respective allocated slices.
    let (mid_ptr, hay_last) = unsafe { (lower_needle.add(1), hay.add(last_off)) };

    let off = 0usize;
    let res: u32;
    let cand_ptr: *const u8;
    let k_idx: usize;
    let byte_tmp: u32;
    let fold_tmp: u32;

    // SAFETY: Caller verified `CAP_AVX2` (`avx2 + bmi1`) and `positions >= 32`.
    // For ASCII lowercase targets `'a'..='z'`, `(b | 0x20) == c` holds iff `b == c` or `b == c - 0x20`;
    // for non-alpha targets, `fm_b / lm_b == 0` checks exact equality with zero false positives.
    unsafe {
        asm!(
            "vmovd {vf:x}, {m:e}",
            "vpbroadcastb {vf}, {vf:x}",
            "shr {m:e}, 8",
            "vmovd {vl:x}, {m:e}",
            "vpbroadcastb {vl}, {vl:x}",
            "shr {m:e}, 8",
            "vmovd {vfm:x}, {m:e}",
            "vpbroadcastb {vfm}, {vfm:x}",
            "shr {m:e}, 8",
            "vmovd {vlm:x}, {m:e}",
            "vpbroadcastb {vlm}, {vlm:x}",
            "2:",
            "vmovdqu {vt0}, ymmword ptr [{hay} + {off}]",
            "vmovdqu {vt1}, ymmword ptr [{hay_last} + {off}]",
            "vpor {vt0}, {vt0}, {vfm}",
            "vpor {vt1}, {vt1}, {vlm}",
            "vpcmpeqb {vt0}, {vt0}, {vf}",
            "vpcmpeqb {vt1}, {vt1}, {vl}",
            "vpand {vt0}, {vt0}, {vt1}",
            "vpmovmskb {m:e}, {vt0}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "4:",
            "cmp {off}, {lim}",
            "je 7f",
            "add {off}, 32",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "test {mid_len}, {mid_len}",
            "jz 8f",
            "5:",
            "tzcnt {cand:e}, {m:e}",
            "lea {cand}, [{hay} + {cand} + 1]",
            "add {cand}, {off}",
            "xor {k:e}, {k:e}",
            "6:",
            "movzx {bt:e}, byte ptr [{cand} + {k}]",
            "lea {ft:e}, [{bt:r} - 65]",
            "cmp {ft:l}, 26",
            "lea {ft:e}, [{bt:r} + 32]",
            "cmovb {bt:e}, {ft:e}",
            "cmp {bt:l}, byte ptr [{mid_ptr} + {k}]",
            "jne 9f",
            "inc {k}",
            "cmp {k}, {mid_len}",
            "jb 6b",
            "jmp 8f",
            "9:",
            "blsr {m:e}, {m:e}",
            "jnz 5b",
            "jmp 4b",
            "7:",
            "xor {m:e}, {m:e}",
            "jmp 20f",
            "8:",
            "mov {m:e}, 1",
            "20:",
            "vzeroupper",
            hay = in(reg) hay,
            hay_last = in(reg) hay_last,
            mid_ptr = in(reg) mid_ptr,
            mid_len = in(reg) mid_len,
            lim = in(reg) limit32,
            off = inout(reg) off => _,
            m = inout(reg) packed => res,
            cand = out(reg) cand_ptr,
            k = out(reg) k_idx,
            bt = out(reg) byte_tmp,
            ft = out(reg) fold_tmp,
            vf = out(ymm_reg) _,
            vl = out(ymm_reg) _,
            vfm = out(ymm_reg) _,
            vlm = out(ymm_reg) _,
            vt0 = out(ymm_reg) _,
            vt1 = out(ymm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = (cand_ptr, k_idx, byte_tmp, fold_tmp);
    res != 0
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn memmem_ascii_nocase_sse2(
    hay: *const u8,
    hay_len: usize,
    lower_needle: *const u8,
    needle_len: usize,
) -> bool {
    debug_assert!(needle_len >= 2 && hay_len >= needle_len + 15);
    let positions = hay_len - needle_len + 1;
    let limit16 = positions - 16;
    let last_off = needle_len - 1;
    let mid_len = needle_len - 2;
    // SAFETY: `needle_len >= 2` is checked by caller.
    let first_b = unsafe { *lower_needle };
    let last_b = unsafe { *lower_needle.add(last_off) };
    let fm_b = if first_b.is_ascii_lowercase() {
        0x20u32
    } else {
        0u32
    };
    let lm_b = if last_b.is_ascii_lowercase() {
        0x20u32
    } else {
        0u32
    };
    let packed = u32::from(first_b) | (u32::from(last_b) << 8) | (fm_b << 16) | (lm_b << 24);
    // SAFETY: `mlen >= 2` and `hlen >= mlen`, so `lower_needle.add(1)` and `hay.add(mlen - 1)`
    // remain within their respective allocated slices.
    let (mid_ptr, hay_last) = unsafe { (lower_needle.add(1), hay.add(last_off)) };

    let off = 0usize;
    let res: u32;
    let cand_ptr: *const u8;
    let k_idx: usize;
    let byte_tmp: u32;
    let fold_tmp: u32;

    // SAFETY: Caller verified `CAP_SSE2` and `positions >= 16`. `bsf` runs only when `m != 0`.
    unsafe {
        asm!(
            "movzx {bt:e}, {m:l}",
            "imul {bt:e}, {bt:e}, 0x01010101",
            "movd {vf}, {bt:e}",
            "pshufd {vf}, {vf}, 0",
            "shr {m:e}, 8",
            "movzx {bt:e}, {m:l}",
            "imul {bt:e}, {bt:e}, 0x01010101",
            "movd {vl}, {bt:e}",
            "pshufd {vl}, {vl}, 0",
            "shr {m:e}, 8",
            "movzx {bt:e}, {m:l}",
            "imul {bt:e}, {bt:e}, 0x01010101",
            "movd {vfm}, {bt:e}",
            "pshufd {vfm}, {vfm}, 0",
            "shr {m:e}, 8",
            "imul {m:e}, {m:e}, 0x01010101",
            "movd {vlm}, {m:e}",
            "pshufd {vlm}, {vlm}, 0",
            "2:",
            "movdqu {vt0}, xmmword ptr [{hay} + {off}]",
            "movdqu {vt1}, xmmword ptr [{hay_last} + {off}]",
            "por {vt0}, {vfm}",
            "por {vt1}, {vlm}",
            "pcmpeqb {vt0}, {vf}",
            "pcmpeqb {vt1}, {vl}",
            "pand {vt0}, {vt1}",
            "pmovmskb {m:e}, {vt0}",
            "test {m:e}, {m:e}",
            "jnz 3f",
            "4:",
            "cmp {off}, {lim}",
            "je 7f",
            "add {off}, 16",
            "cmp {off}, {lim}",
            "cmova {off}, {lim}",
            "jmp 2b",
            "3:",
            "test {mid_len}, {mid_len}",
            "jz 8f",
            "5:",
            "bsf {cand:e}, {m:e}",
            "lea {cand}, [{hay} + {cand} + 1]",
            "add {cand}, {off}",
            "xor {k:e}, {k:e}",
            "6:",
            "movzx {bt:e}, byte ptr [{cand} + {k}]",
            "lea {ft:e}, [{bt:r} - 65]",
            "cmp {ft:l}, 26",
            "lea {ft:e}, [{bt:r} + 32]",
            "cmovb {bt:e}, {ft:e}",
            "cmp {bt:l}, byte ptr [{mid_ptr} + {k}]",
            "jne 9f",
            "inc {k}",
            "cmp {k}, {mid_len}",
            "jb 6b",
            "jmp 8f",
            "9:",
            "lea {ft:e}, [{m:r} - 1]",
            "and {m:e}, {ft:e}",
            "jnz 5b",
            "jmp 4b",
            "7:",
            "xor {m:e}, {m:e}",
            "jmp 20f",
            "8:",
            "mov {m:e}, 1",
            "20:",
            hay = in(reg) hay,
            hay_last = in(reg) hay_last,
            mid_ptr = in(reg) mid_ptr,
            mid_len = in(reg) mid_len,
            lim = in(reg) limit16,
            off = inout(reg) off => _,
            m = inout(reg) packed => res,
            cand = out(reg) cand_ptr,
            k = out(reg) k_idx,
            bt = out(reg) byte_tmp,
            ft = out(reg) fold_tmp,
            vf = out(xmm_reg) _,
            vl = out(xmm_reg) _,
            vfm = out(xmm_reg) _,
            vlm = out(xmm_reg) _,
            vt0 = out(xmm_reg) _,
            vt1 = out(xmm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = (cand_ptr, k_idx, byte_tmp, fold_tmp);
    res != 0
}

/// ASCII case-insensitive substring containment (`LIKE '%lit%'`) where `lower_needle` already
/// has ASCII `'A'..='Z'` mapped to `'a'..='z'`.
#[inline]
pub fn memmem_ascii_nocase(haystack: &[u8], lower_needle: &[u8]) -> bool {
    let m = lower_needle.len();
    let n = haystack.len();
    if m == 0 {
        return true;
    }
    if n < m {
        return false;
    }
    if m == 1 {
        let t = lower_needle[0];
        return if t.is_ascii_lowercase() {
            memchr2(haystack, t, t.to_ascii_uppercase()).is_some()
        } else {
            memchr1(haystack, t).is_some()
        };
    }
    #[cfg(target_arch = "x86_64")]
    {
        let positions = n - m + 1;
        if positions >= 32 {
            let caps = cpu_caps();
            if (caps & CAP_AVX2) != 0 {
                // SAFETY: `CAP_AVX2` is verified, `m >= 2`, and `positions >= 32`.
                return unsafe {
                    memmem_ascii_nocase_avx2(haystack.as_ptr(), n, lower_needle.as_ptr(), m)
                };
            }
            if (caps & CAP_SSE2) != 0 {
                // SAFETY: `CAP_SSE2` is verified, `m >= 2`, and `positions >= 16`.
                return unsafe {
                    memmem_ascii_nocase_sse2(haystack.as_ptr(), n, lower_needle.as_ptr(), m)
                };
            }
        } else if positions >= 16 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified, `m >= 2`, and `positions >= 16`.
            return unsafe {
                memmem_ascii_nocase_sse2(haystack.as_ptr(), n, lower_needle.as_ptr(), m)
            };
        }
    }
    memmem_ascii_nocase_scalar(haystack, lower_needle)
}

// ============================================================================
// ASM-2: Hand-Scheduled Inline AVX2 4-Chain T^16 (128B/iter) + SSE2 T^4 (32B/iter)
//        + Overlapped GP/MOVBE Pair WAL Cumulative Checksum
// ============================================================================

/// Plain scalar fallback for extending the cumulative WAL/SHM checksum over an 8-byte-aligned slice.
#[inline]
pub fn wal_checksum_extend_scalar(byte_swap: bool, data: &[u8], initial: [u32; 2]) -> [u32; 2] {
    let [mut s1, mut s2] = initial;
    for pair in data.as_chunks::<8>().0 {
        let raw0 = u32::from_ne_bytes([pair[0], pair[1], pair[2], pair[3]]);
        let raw1 = u32::from_ne_bytes([pair[4], pair[5], pair[6], pair[7]]);
        let (w0, w1) = if byte_swap {
            (raw0.swap_bytes(), raw1.swap_bytes())
        } else {
            (raw0, raw1)
        };
        s1 = s1.wrapping_add(w0).wrapping_add(s2);
        s2 = s2.wrapping_add(w1).wrapping_add(s1);
    }
    [s1, s2]
}

#[cfg(target_arch = "x86_64")]
#[repr(C, align(32))]
struct WalAvx2Consts {
    c1: [u32; 8],
    q: [u32; 8],
    odd: [u32; 8],
    bswap: [u8; 32],
    m1: [u32; 32],
    m2: [u32; 32],
}

#[cfg(target_arch = "x86_64")]
static WAL_AVX2_CONSTS: WalAvx2Consts = WalAvx2Consts {
    c1: [
        1_346_269, 3_524_578, 1_346_269, 3_524_578, 1_346_269, 3_524_578, 1_346_269, 3_524_578,
    ],
    q: [2_178_309; 8],
    odd: [
        0,
        0xffff_ffff,
        0,
        0xffff_ffff,
        0,
        0xffff_ffff,
        0,
        0xffff_ffff,
    ],
    bswap: [
        3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12, 3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8,
        15, 14, 13, 12,
    ],
    m1: [
        514_229, 832_040, 196_418, 317_811, 75_025, 121_393, 28_657, 46_368, 10_946, 17_711, 4_181,
        6_765, 1_597, 2_584, 610, 987, 233, 377, 89, 144, 34, 55, 13, 21, 5, 8, 2, 3, 1, 1, 1, 0,
    ],
    m2: [
        832_040, 1_346_269, 317_811, 514_229, 121_393, 196_418, 46_368, 75_025, 17_711, 28_657,
        6_765, 10_946, 2_584, 4_181, 987, 1_597, 377, 610, 144, 233, 55, 89, 21, 34, 8, 13, 3, 5,
        1, 2, 0, 1,
    ],
};

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn wal_checksum_pairs_gp_native(
    ptr: *const u8,
    byte_len: usize,
    init: [u32; 2],
) -> [u32; 2] {
    debug_assert!(byte_len >= 8 && byte_len.is_multiple_of(8));
    // SAFETY: Caller verified `byte_len >= 8` and `ptr[..byte_len]` is valid.
    let end = unsafe { ptr.add(byte_len) };
    let mut s1 = init[0];
    let mut s2 = init[1];
    let cur = ptr;
    let w0: u32;
    // Schedule `add s2, [cur + 4]` in parallel with `add s1, s2; add s1, [cur]` so each 8B pair
    // has a 3-cycle recurrence latency instead of 4 cycles.
    // SAFETY: `cur < end` in multiples of 8 bytes; all modified registers are declared.
    unsafe {
        asm!(
            "2:",
            "mov {w0:e}, dword ptr [{cur}]",
            "add {s1:e}, {s2:e}",
            "add {s2:e}, dword ptr [{cur} + 4]",
            "add {s1:e}, {w0:e}",
            "add {s2:e}, {s1:e}",
            "add {cur}, 8",
            "cmp {cur}, {end}",
            "jb 2b",
            cur = inout(reg) cur => _,
            end = in(reg) end,
            s1 = inout(reg) s1,
            s2 = inout(reg) s2,
            w0 = out(reg) w0,
            options(nostack, readonly),
        );
    }
    let _ = w0;
    [s1, s2]
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn wal_checksum_pairs_gp_movbe(ptr: *const u8, byte_len: usize, init: [u32; 2]) -> [u32; 2] {
    debug_assert!(byte_len >= 8 && byte_len.is_multiple_of(8));
    // SAFETY: Caller verified `CAP_MOVBE`, `byte_len >= 8`, and `ptr[..byte_len]` is valid.
    let end = unsafe { ptr.add(byte_len) };
    let mut s1 = init[0];
    let mut s2 = init[1];
    let cur = ptr;
    let w0: u32;
    let w1: u32;
    // SAFETY: `cur < end` in multiples of 8 bytes; `movbe` executes on 32-bit words.
    unsafe {
        asm!(
            "2:",
            "movbe {w0:e}, dword ptr [{cur}]",
            "movbe {w1:e}, dword ptr [{cur} + 4]",
            "add {s1:e}, {s2:e}",
            "add {s2:e}, {w1:e}",
            "add {s1:e}, {w0:e}",
            "add {s2:e}, {s1:e}",
            "add {cur}, 8",
            "cmp {cur}, {end}",
            "jb 2b",
            cur = inout(reg) cur => _,
            end = in(reg) end,
            s1 = inout(reg) s1,
            s2 = inout(reg) s2,
            w0 = out(reg) w0,
            w1 = out(reg) w1,
            options(nostack, readonly),
        );
    }
    let _ = (w0, w1);
    [s1, s2]
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn wal_checksum_avx2_blocks(
    ptr: *const u8,
    vec_bytes: usize,
    byte_swap: bool,
    init: [u32; 2],
) -> [u32; 2] {
    debug_assert!(vec_bytes >= 128 && vec_bytes.is_multiple_of(128));
    // SAFETY: `vec_bytes` is a positive multiple of 128 within caller's slice.
    let end = unsafe { ptr.add(vec_bytes) };
    let init_qword = u64::from(init[0]) | (u64::from(init[1]) << 32);
    let consts_ptr = std::ptr::addr_of!(WAL_AVX2_CONSTS);
    let s1_out: u32;
    let s2_out: u32;
    let bswap_flag = u32::from(byte_swap);

    // SAFETY: Caller verified `CAP_AVX2` and `ptr[..vec_bytes]` is valid for reads.
    // All 16 YMM registers (`ymm0..ymm15`) and GP operands are explicitly declared, and LLVM emits
    // `vzeroupper` at the exit of this `#[target_feature(enable = "avx2")]` function.
    unsafe {
        asm!(
            "vmovdqa ymm12, ymmword ptr [{consts} + 0]",
            "vmovdqa ymm13, ymmword ptr [{consts} + 32]",
            "vmovdqa ymm14, ymmword ptr [{consts} + 64]",
            "vpxor xmm0, xmm0, xmm0",
            "vpxor xmm1, xmm1, xmm1",
            "vpxor xmm2, xmm2, xmm2",
            "vmovq xmm7, {init_q}",
            "vpermq ymm3, ymm7, 0x15",
            "test {bswap:e}, {bswap:e}",
            "jnz 3f",
            ".p2align 5",
            "2:",
            "vmovdqu ymm4, ymmword ptr [{cur}]",
            "vmovdqu ymm5, ymmword ptr [{cur} + 32]",
            "vmovdqu ymm6, ymmword ptr [{cur} + 64]",
            "vmovdqu ymm7, ymmword ptr [{cur} + 96]",
            "vpshufd ymm8,  ymm4, 0xb1",
            "vpshufd ymm9,  ymm5, 0xb1",
            "vpshufd ymm10, ymm6, 0xb1",
            "vpshufd ymm11, ymm7, 0xb1",
            "vpand ymm8,  ymm8,  ymm14",
            "vpand ymm9,  ymm9,  ymm14",
            "vpand ymm10, ymm10, ymm14",
            "vpand ymm11, ymm11, ymm14",
            "vpaddd ymm4, ymm4, ymm8",
            "vpaddd ymm5, ymm5, ymm9",
            "vpaddd ymm6, ymm6, ymm10",
            "vpaddd ymm7, ymm7, ymm11",
            "vpshufd ymm8,  ymm0, 0xb1",
            "vpshufd ymm9,  ymm1, 0xb1",
            "vpshufd ymm10, ymm2, 0xb1",
            "vpshufd ymm11, ymm3, 0xb1",
            "vpmulld ymm8,  ymm8,  ymm13",
            "vpmulld ymm9,  ymm9,  ymm13",
            "vpmulld ymm10, ymm10, ymm13",
            "vpmulld ymm11, ymm11, ymm13",
            "vpmulld ymm0, ymm0, ymm12",
            "vpmulld ymm1, ymm1, ymm12",
            "vpmulld ymm2, ymm2, ymm12",
            "vpmulld ymm3, ymm3, ymm12",
            "vpaddd ymm0, ymm0, ymm8",
            "vpaddd ymm1, ymm1, ymm9",
            "vpaddd ymm2, ymm2, ymm10",
            "vpaddd ymm3, ymm3, ymm11",
            "vpaddd ymm0, ymm0, ymm4",
            "vpaddd ymm1, ymm1, ymm5",
            "vpaddd ymm2, ymm2, ymm6",
            "vpaddd ymm3, ymm3, ymm7",
            "sub {cur}, -128",
            "cmp {cur}, {end}",
            "jb 2b",
            "jmp 4f",
            "3:",
            "vmovdqa ymm15, ymmword ptr [{consts} + 96]",
            ".p2align 5",
            "5:",
            "vmovdqu ymm4, ymmword ptr [{cur}]",
            "vmovdqu ymm5, ymmword ptr [{cur} + 32]",
            "vmovdqu ymm6, ymmword ptr [{cur} + 64]",
            "vmovdqu ymm7, ymmword ptr [{cur} + 96]",
            "vpshufb ymm4, ymm4, ymm15",
            "vpshufb ymm5, ymm5, ymm15",
            "vpshufb ymm6, ymm6, ymm15",
            "vpshufb ymm7, ymm7, ymm15",
            "vpshufd ymm8,  ymm4, 0xb1",
            "vpshufd ymm9,  ymm5, 0xb1",
            "vpshufd ymm10, ymm6, 0xb1",
            "vpshufd ymm11, ymm7, 0xb1",
            "vpand ymm8,  ymm8,  ymm14",
            "vpand ymm9,  ymm9,  ymm14",
            "vpand ymm10, ymm10, ymm14",
            "vpand ymm11, ymm11, ymm14",
            "vpaddd ymm4, ymm4, ymm8",
            "vpaddd ymm5, ymm5, ymm9",
            "vpaddd ymm6, ymm6, ymm10",
            "vpaddd ymm7, ymm7, ymm11",
            "vpshufd ymm8,  ymm0, 0xb1",
            "vpshufd ymm9,  ymm1, 0xb1",
            "vpshufd ymm10, ymm2, 0xb1",
            "vpshufd ymm11, ymm3, 0xb1",
            "vpmulld ymm8,  ymm8,  ymm13",
            "vpmulld ymm9,  ymm9,  ymm13",
            "vpmulld ymm10, ymm10, ymm13",
            "vpmulld ymm11, ymm11, ymm13",
            "vpmulld ymm0, ymm0, ymm12",
            "vpmulld ymm1, ymm1, ymm12",
            "vpmulld ymm2, ymm2, ymm12",
            "vpmulld ymm3, ymm3, ymm12",
            "vpaddd ymm0, ymm0, ymm8",
            "vpaddd ymm1, ymm1, ymm9",
            "vpaddd ymm2, ymm2, ymm10",
            "vpaddd ymm3, ymm3, ymm11",
            "vpaddd ymm0, ymm0, ymm4",
            "vpaddd ymm1, ymm1, ymm5",
            "vpaddd ymm2, ymm2, ymm6",
            "vpaddd ymm3, ymm3, ymm7",
            "sub {cur}, -128",
            "cmp {cur}, {end}",
            "jb 5b",
            "4:",
            "vpmulld ymm4,  ymm0, ymmword ptr [{consts} + 128]",
            "vpmulld ymm5,  ymm1, ymmword ptr [{consts} + 160]",
            "vpmulld ymm6,  ymm2, ymmword ptr [{consts} + 192]",
            "vpmulld ymm7,  ymm3, ymmword ptr [{consts} + 224]",
            "vpmulld ymm8,  ymm0, ymmword ptr [{consts} + 256]",
            "vpmulld ymm9,  ymm1, ymmword ptr [{consts} + 288]",
            "vpmulld ymm10, ymm2, ymmword ptr [{consts} + 320]",
            "vpmulld ymm11, ymm3, ymmword ptr [{consts} + 352]",
            "vpaddd ymm4, ymm4, ymm5",
            "vpaddd ymm6, ymm6, ymm7",
            "vpaddd ymm4, ymm4, ymm6",
            "vpaddd ymm8, ymm8, ymm9",
            "vpaddd ymm10, ymm10, ymm11",
            "vpaddd ymm8, ymm8, ymm10",
            "vextracti128 xmm5, ymm4, 1",
            "vpaddd xmm4, xmm4, xmm5",
            "vextracti128 xmm9, ymm8, 1",
            "vpaddd xmm8, xmm8, xmm9",
            "vpshufd xmm5, xmm4, 0x4e",
            "vpaddd xmm4, xmm4, xmm5",
            "vpshufd xmm5, xmm4, 0xb1",
            "vpaddd xmm4, xmm4, xmm5",
            "vpshufd xmm9, xmm8, 0x4e",
            "vpaddd xmm8, xmm8, xmm9",
            "vpshufd xmm9, xmm8, 0xb1",
            "vpaddd xmm8, xmm8, xmm9",
            "vmovd {s1:e}, xmm4",
            "vmovd {s2:e}, xmm8",
            "vzeroupper",
            cur = inout(reg) ptr => _,
            end = in(reg) end,
            consts = in(reg) consts_ptr,
            init_q = in(reg) init_qword,
            bswap = in(reg) bswap_flag,
            s1 = out(reg) s1_out,
            s2 = out(reg) s2_out,
            out("ymm0") _,
            out("ymm1") _,
            out("ymm2") _,
            out("ymm3") _,
            out("ymm4") _,
            out("ymm5") _,
            out("ymm6") _,
            out("ymm7") _,
            out("ymm8") _,
            out("ymm9") _,
            out("ymm10") _,
            out("ymm11") _,
            out("ymm12") _,
            out("ymm13") _,
            out("ymm14") _,
            out("ymm15") _,
            options(nostack, readonly),
        );
    }
    [s1_out, s2_out]
}

/// Inline `SSE2` `T^4` (`32B/iter`) WAL checksum block reducer using deinterleaved `(S1, S2)`
/// `xmm` accumulators and Horner matrix reduction (zero `ymm` dirtying, zero lookup tables).
#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn wal_checksum_sse2_blocks(
    ptr: *const u8,
    vec_bytes: usize,
    byte_swap: bool,
    init: [u32; 2],
) -> [u32; 2] {
    debug_assert!(vec_bytes >= 32 && vec_bytes.is_multiple_of(32));
    // SAFETY: `vec_bytes` is a positive multiple of 32 within caller's slice.
    let end = unsafe { ptr.add(vec_bytes) };
    let mut s1 = init[0];
    let mut s2 = init[1];
    let bswap_flag = u32::from(byte_swap);
    let y1_lane: u32;
    let y2_lane: u32;

    // SAFETY: Caller verified `CAP_SSE2` and `ptr[..vec_bytes]` is valid for reads.
    unsafe {
        asm!(
            "movd {vs1}, {s1:e}",
            "pslldq {vs1}, 12",
            "movd {vs2}, {s2:e}",
            "pslldq {vs2}, 12",
            "test {bswap:e}, {bswap:e}",
            "jnz 3f",
            "2:",
            "movdqu {vw0}, xmmword ptr [{cur}]",
            "movdqu {vw1}, xmmword ptr [{cur} + 16]",
            "pshufd {vw0}, {vw0}, 0xd8",
            "pshufd {vw1}, {vw1}, 0xd8",
            "movdqa {vt}, {vw0}",
            "punpcklqdq {vt}, {vw1}",
            "punpckhqdq {vw0}, {vw1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vt}",
            "paddd {vw0}, {vt}",
            "paddd {vs2}, {vw0}",
            "add {cur}, 32",
            "cmp {cur}, {end}",
            "jb 2b",
            "jmp 4f",
            "3:",
            "movdqu {vw0}, xmmword ptr [{cur}]",
            "movdqu {vw1}, xmmword ptr [{cur} + 16]",
            "pshuflw {vw0}, {vw0}, 0xb1",
            "pshufhw {vw0}, {vw0}, 0xb1",
            "movdqa {vt}, {vw0}",
            "psllw {vw0}, 8",
            "psrlw {vt}, 8",
            "por {vw0}, {vt}",
            "pshuflw {vw1}, {vw1}, 0xb1",
            "pshufhw {vw1}, {vw1}, 0xb1",
            "movdqa {vt}, {vw1}",
            "psllw {vw1}, 8",
            "psrlw {vt}, 8",
            "por {vw1}, {vt}",
            "pshufd {vw0}, {vw0}, 0xd8",
            "pshufd {vw1}, {vw1}, 0xd8",
            "movdqa {vt}, {vw0}",
            "punpcklqdq {vt}, {vw1}",
            "punpckhqdq {vw0}, {vw1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vs2}",
            "paddd {vs2}, {vs1}",
            "paddd {vs1}, {vt}",
            "paddd {vw0}, {vt}",
            "paddd {vs2}, {vw0}",
            "add {cur}, 32",
            "cmp {cur}, {end}",
            "jb 3b",
            "4:",
            // Horner reduction across lanes 0..3: z = T(T(T(y0) + y1) + y2) + y3
            "movd {s1:e}, {vs1}",
            "movd {s2:e}, {vs2}",
            "pshufd {vt}, {vs1}, 0x55",
            "movd {y1:e}, {vt}",
            "pshufd {vt}, {vs2}, 0x55",
            "movd {y2:e}, {vt}",
            "add {s1:e}, {s2:e}",
            "add {s2:e}, {s1:e}",
            "add {s1:e}, {y1:e}",
            "add {s2:e}, {y2:e}",
            "pshufd {vt}, {vs1}, 0xaa",
            "movd {y1:e}, {vt}",
            "pshufd {vt}, {vs2}, 0xaa",
            "movd {y2:e}, {vt}",
            "add {s1:e}, {s2:e}",
            "add {s2:e}, {s1:e}",
            "add {s1:e}, {y1:e}",
            "add {s2:e}, {y2:e}",
            "pshufd {vt}, {vs1}, 0xff",
            "movd {y1:e}, {vt}",
            "pshufd {vt}, {vs2}, 0xff",
            "movd {y2:e}, {vt}",
            "add {s1:e}, {s2:e}",
            "add {s2:e}, {s1:e}",
            "add {s1:e}, {y1:e}",
            "add {s2:e}, {y2:e}",
            cur = inout(reg) ptr => _,
            end = in(reg) end,
            bswap = in(reg) bswap_flag,
            s1 = inout(reg) s1,
            s2 = inout(reg) s2,
            y1 = out(reg) y1_lane,
            y2 = out(reg) y2_lane,
            vs1 = out(xmm_reg) _,
            vs2 = out(xmm_reg) _,
            vw0 = out(xmm_reg) _,
            vw1 = out(xmm_reg) _,
            vt = out(xmm_reg) _,
            options(nostack, readonly),
        );
    }
    let _ = (y1_lane, y2_lane);
    [s1, s2]
}

/// Extends the 8-byte cumulative WAL/SHM checksum over `data` (whose length must be a multiple
/// of 8 bytes; any trailing `< 8` remainder is ignored just like `as_chunks::<8>().0`).
#[inline]
pub fn wal_checksum_extend(byte_swap: bool, data: &[u8], initial: [u32; 2]) -> [u32; 2] {
    let aligned_len = data.len() & !7;
    if aligned_len == 0 {
        return initial;
    }
    let slice = &data[..aligned_len];
    #[cfg(target_arch = "x86_64")]
    {
        let caps = cpu_caps();
        let mut state = initial;
        let mut offset = 0usize;
        if aligned_len >= 128 {
            if (caps & CAP_AVX2) != 0 {
                let vec_len = aligned_len & !127;
                // SAFETY: `CAP_AVX2` is verified and `vec_len` is a non-zero multiple of 128 <= `slice.len()`.
                state =
                    unsafe { wal_checksum_avx2_blocks(slice.as_ptr(), vec_len, byte_swap, state) };
                offset = vec_len;
            } else if (caps & CAP_SSE2) != 0 {
                let vec_len = aligned_len & !31;
                // SAFETY: `CAP_SSE2` is verified and `vec_len` is a non-zero multiple of 32 <= `slice.len()`.
                state =
                    unsafe { wal_checksum_sse2_blocks(slice.as_ptr(), vec_len, byte_swap, state) };
                offset = vec_len;
            }
        }
        let rem = aligned_len - offset;
        if rem == 0 {
            return state;
        }
        let tail = &slice[offset..];
        if !byte_swap {
            // SAFETY: `rem >= 8` is a multiple of 8 and `tail.len() == rem`.
            return unsafe { wal_checksum_pairs_gp_native(tail.as_ptr(), rem, state) };
        }
        if (caps & CAP_MOVBE) != 0 {
            // SAFETY: `CAP_MOVBE` is verified, `rem >= 8` is a multiple of 8, and `tail.len() == rem`.
            return unsafe { wal_checksum_pairs_gp_movbe(tail.as_ptr(), rem, state) };
        }
        wal_checksum_extend_scalar(byte_swap, tail, state)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        wal_checksum_extend_scalar(byte_swap, slice, initial)
    }
}

/// Explicit `SSE2` `T^4` (`32B/iter`) + scalar tail WAL checksum entry point (for parity testing
/// and `SSE2`-only `x86_64` verification).
#[inline]
pub fn wal_checksum_extend_sse2(byte_swap: bool, data: &[u8], initial: [u32; 2]) -> [u32; 2] {
    let aligned_len = data.len() & !7;
    if aligned_len == 0 {
        return initial;
    }
    let slice = &data[..aligned_len];
    #[cfg(target_arch = "x86_64")]
    {
        if aligned_len >= 32 && (cpu_caps() & CAP_SSE2) != 0 {
            let vec_len = aligned_len & !31;
            // SAFETY: `CAP_SSE2` is verified and `vec_len` is a positive multiple of 32 <= `slice.len()`.
            let state =
                unsafe { wal_checksum_sse2_blocks(slice.as_ptr(), vec_len, byte_swap, initial) };
            if vec_len == aligned_len {
                return state;
            }
            return wal_checksum_extend_scalar(byte_swap, &slice[vec_len..], state);
        }
    }
    wal_checksum_extend_scalar(byte_swap, slice, initial)
}

// ============================================================================
// ASM-4: 2D `f32`/`f64` R-Tree Whole-Node Scanners, IEEE-754 Exact Union,
//        `choose_subtree`, & `movbe` + `pshufb` Big-Endian Cell Codec
// ============================================================================

/// Fixed-size 48-byte R-Tree cell (`id: i64` + up to 5D `[min, max]` 32-bit coordinate words).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RTreeCell10 {
    pub id: i64,
    pub coords: [u32; 10],
}

impl RTreeCell10 {
    /// Constructs a 2D `f32` cell with bit-exact coordinate words and zeroed trailing slots.
    #[inline]
    pub const fn from_2d_f32(id: i64, min_x: f32, max_x: f32, min_y: f32, max_y: f32) -> Self {
        Self {
            id,
            coords: [
                min_x.to_bits(),
                max_x.to_bits(),
                min_y.to_bits(),
                max_y.to_bits(),
                0,
                0,
                0,
                0,
                0,
                0,
            ],
        }
    }
}

const _: () = assert!(core::mem::size_of::<RTreeCell10>() == 48);

#[cfg(target_arch = "x86_64")]
#[repr(C, align(32))]
struct RTreeConsts {
    bswap32: [u8; 16],
    flip_f32_max: [u32; 4],
    flip_f64_min: [u64; 4],
}

#[cfg(target_arch = "x86_64")]
static RTREE_CONSTS: RTreeConsts = RTreeConsts {
    bswap32: [3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12],
    flip_f32_max: [0, 0x8000_0000, 0, 0x8000_0000],
    flip_f64_min: [0x8000_0000_0000_0000, 0, 0x8000_0000_0000_0000, 0],
};

/// Plain scalar fallback decoding `n_cell` 24-byte big-endian 2D R-Tree cells from `data[4..]`.
#[inline]
pub fn rtree_decode_2d_cells_scalar(data: &[u8], n_cell: usize, out: &mut Vec<RTreeCell10>) {
    out.clear();
    let Some(end_bytes) = n_cell.checked_mul(24).and_then(|b| b.checked_add(4)) else {
        return;
    };
    if data.len() < end_bytes {
        return;
    }
    out.reserve(n_cell);
    for i in 0..n_cell {
        let base = 4 + i * 24;
        let id = i64::from_be_bytes([
            data[base],
            data[base + 1],
            data[base + 2],
            data[base + 3],
            data[base + 4],
            data[base + 5],
            data[base + 6],
            data[base + 7],
        ]);
        let mut coords = [0u32; 10];
        for (j, slot) in coords.iter_mut().enumerate().take(4) {
            let c = base + 8 + j * 4;
            *slot = u32::from_be_bytes([data[c], data[c + 1], data[c + 2], data[c + 3]]);
        }
        out.push(RTreeCell10 { id, coords });
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn rtree_decode_2d_ssse3_movbe(src: *const u8, dst: *mut RTreeCell10, n_cell: usize) {
    debug_assert!(n_cell > 0);
    // SAFETY: Caller verified `CAP_SSSE3` and `CAP_MOVBE`, `src` has `n_cell * 24` readable
    // bytes, and `dst` points to `n_cell` writable `RTreeCell10` slots (`n_cell * 48` bytes).
    unsafe {
        let end = src.add(n_cell * 24);
        let dst_u8 = dst.cast::<u8>();
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        let id_tmp: u64;
        asm!(
            "movdqa {bswap}, xmmword ptr [{k}]",
            "pxor {vz}, {vz}",
            "2:",
            "movbe {id}, qword ptr [{src}]",
            "movdqu {v}, xmmword ptr [{src} + 8]",
            "pshufb {v}, {bswap}",
            "mov qword ptr [{dst}], {id}",
            "movdqu xmmword ptr [{dst} + 8], {v}",
            "movdqu xmmword ptr [{dst} + 24], {vz}",
            "mov qword ptr [{dst} + 40], 0",
            "add {src}, 24",
            "add {dst}, 48",
            "cmp {src}, {end}",
            "jb 2b",
            src = inout(reg) src => _,
            dst = inout(reg) dst_u8 => _,
            end = in(reg) end,
            k = in(reg) k,
            id = out(reg) id_tmp,
            bswap = out(xmm_reg) _,
            vz = out(xmm_reg) _,
            v = out(xmm_reg) _,
            options(nostack),
        );
        let _ = id_tmp;
    }
}

/// Decodes `n_cell` 24-byte big-endian 2D R-Tree cells from `data[4..]` into `out`,
/// using inline `movbe` + `SSSE3` `pshufb` on `x86_64`.
#[inline]
pub fn rtree_decode_2d_cells(data: &[u8], n_cell: usize, out: &mut Vec<RTreeCell10>) {
    let Some(end_bytes) = n_cell.checked_mul(24).and_then(|b| b.checked_add(4)) else {
        out.clear();
        return;
    };
    if data.len() < end_bytes {
        out.clear();
        return;
    }
    if n_cell == 0 {
        out.clear();
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let caps = cpu_caps();
        if (caps & (CAP_SSSE3 | CAP_MOVBE)) == (CAP_SSSE3 | CAP_MOVBE) {
            out.clear();
            out.reserve(n_cell);
            // SAFETY: `data.len() >= 4 + n_cell * 24`, `out` was cleared and reserved for `n_cell`
            // elements, and `rtree_decode_2d_ssse3_movbe` initializes all 48 bytes of each of the
            // `n_cell` contiguous `RTreeCell10` slots.
            unsafe {
                rtree_decode_2d_ssse3_movbe(data.as_ptr().add(4), out.as_mut_ptr(), n_cell);
                out.set_len(n_cell);
            }
            return;
        }
    }
    rtree_decode_2d_cells_scalar(data, n_cell, out);
}

/// Plain scalar fallback encoding `cells` as 24-byte big-endian 2D R-Tree cells into `buf`.
#[inline]
pub fn rtree_encode_2d_cells_scalar(depth: u16, cells: &[RTreeCell10], buf: &mut [u8]) {
    let Some(req) = cells.len().checked_mul(24).and_then(|b| b.checked_add(4)) else {
        return;
    };
    if buf.len() < req {
        return;
    }
    buf[0..2].copy_from_slice(&depth.to_be_bytes());
    buf[2..4].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    for (i, cell) in cells.iter().enumerate() {
        let base = 4 + i * 24;
        buf[base..base + 8].copy_from_slice(&cell.id.to_be_bytes());
        for j in 0..4 {
            let c = base + 8 + j * 4;
            buf[c..c + 4].copy_from_slice(&cell.coords[j].to_be_bytes());
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn rtree_encode_2d_ssse3_movbe(src: *const RTreeCell10, dst: *mut u8, n_cell: usize) {
    debug_assert!(n_cell > 0);
    // SAFETY: Caller verified `CAP_SSSE3` and `CAP_MOVBE`, `src` has `n_cell * 48` readable
    // bytes, and `dst` has `n_cell * 24` writable bytes.
    unsafe {
        let src_u8 = src.cast::<u8>();
        let end = src_u8.add(n_cell * 48);
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        let id_tmp: u64;
        asm!(
            "movdqa {bswap}, xmmword ptr [{k}]",
            "2:",
            "mov {id}, qword ptr [{src}]",
            "movdqu {v}, xmmword ptr [{src} + 8]",
            "movbe qword ptr [{dst}], {id}",
            "pshufb {v}, {bswap}",
            "movdqu xmmword ptr [{dst} + 8], {v}",
            "add {src}, 48",
            "add {dst}, 24",
            "cmp {src}, {end}",
            "jb 2b",
            src = inout(reg) src_u8 => _,
            dst = inout(reg) dst => _,
            end = in(reg) end,
            k = in(reg) k,
            id = out(reg) id_tmp,
            bswap = out(xmm_reg) _,
            v = out(xmm_reg) _,
            options(nostack),
        );
        let _ = id_tmp;
    }
}

/// Encodes `cells` as 24-byte big-endian 2D R-Tree cells into `buf` (writing the 4-byte header),
/// using inline `movbe` + `SSSE3` `pshufb` on `x86_64`.
#[inline]
pub fn rtree_encode_2d_cells(depth: u16, cells: &[RTreeCell10], buf: &mut [u8]) {
    let n_cell = cells.len();
    let Some(req) = n_cell.checked_mul(24).and_then(|b| b.checked_add(4)) else {
        return;
    };
    if buf.len() < req {
        return;
    }
    buf[0..2].copy_from_slice(&depth.to_be_bytes());
    buf[2..4].copy_from_slice(&(n_cell as u16).to_be_bytes());
    if n_cell == 0 {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let caps = cpu_caps();
        if (caps & (CAP_SSSE3 | CAP_MOVBE)) == (CAP_SSSE3 | CAP_MOVBE) {
            // SAFETY: `buf.len() >= 4 + n_cell * 24` and `cells.len() == n_cell > 0`.
            unsafe {
                rtree_encode_2d_ssse3_movbe(cells.as_ptr(), buf.as_mut_ptr().add(4), n_cell);
            }
            return;
        }
    }
    rtree_encode_2d_cells_scalar(depth, cells, buf);
}

/// Plain scalar fallback computing the 2D `f32` bounding-box union of `other` into `acc`
/// with exact IEEE-754 `NaN` and signed-zero (`±0.0`) parity.
#[inline]
pub fn rtree_cell_union_2d_f32_in_place_scalar(acc: &mut RTreeCell10, other: &RTreeCell10) {
    for d in 0..2 {
        let i0 = d * 2;
        let i1 = i0 + 1;
        let a0 = f32::from_bits(acc.coords[i0]);
        let b0 = f32::from_bits(other.coords[i0]);
        if a0 >= b0 {
            acc.coords[i0] = other.coords[i0];
        }
        let a1 = f32::from_bits(acc.coords[i1]);
        let b1 = f32::from_bits(other.coords[i1]);
        if a1 <= b1 {
            acc.coords[i1] = other.coords[i1];
        }
    }
}

/// Plain scalar fallback computing the 2D `f32` node bounding box across `cells`.
#[inline]
pub fn rtree_bbox_union_2d_f32_scalar(cells: &[RTreeCell10], node_id: i64) -> RTreeCell10 {
    let mut bbox = cells[0];
    for cell in &cells[1..] {
        rtree_cell_union_2d_f32_in_place_scalar(&mut bbox, cell);
    }
    bbox.id = node_id;
    bbox
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
unsafe fn rtree_bbox_union_2d_f32_avx128(cells: &[RTreeCell10], node_id: i64) -> RTreeCell10 {
    debug_assert!(!cells.is_empty());
    let mut out = cells[0];
    out.id = node_id;
    if cells.len() == 1 {
        return out;
    }
    // SAFETY: Caller verified `CAP_AVX2` (implying `AVX`), `cells.len() >= 2`, and uses 128-bit
    // VEX instructions exclusively (zero `ymm` dirty state). Sign-flip + ordered `vcmpps(..., 2)`
    // + `vblendvps` matches `if a0 >= b0 { b0 } else { a0 }` / `if a1 <= b1 { b1 } else { a1 }`
    // bit-for-bit across all 32-bit IEEE-754 patterns including `±0.0`, `±Inf`, and `NaN`.
    unsafe {
        let c0 = cells.as_ptr().cast::<u8>();
        let ptr = c0.add(48);
        let end = c0.add(cells.len() * 48);
        let out_coords = (&raw mut out.coords).cast::<u8>();
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        asm!(
            "vmovaps {flip}, xmmword ptr [{k} + 16]",
            "vxorps {va}, {flip}, xmmword ptr [{c0} + 8]",
            "2:",
            "vxorps {vb}, {flip}, xmmword ptr [{ptr} + 8]",
            "vcmpps {vm}, {vb}, {va}, 2",
            "vblendvps {va}, {va}, {vb}, {vm}",
            "add {ptr}, 48",
            "cmp {ptr}, {end}",
            "jb 2b",
            "vxorps {va}, {va}, {flip}",
            "vmovups xmmword ptr [{out_c}], {va}",
            c0 = in(reg) c0,
            ptr = inout(reg) ptr => _,
            end = in(reg) end,
            out_c = in(reg) out_coords,
            k = in(reg) k,
            flip = out(xmm_reg) _,
            va = out(xmm_reg) _,
            vb = out(xmm_reg) _,
            vm = out(xmm_reg) _,
            options(nostack),
        );
    }
    out
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn rtree_bbox_union_2d_f32_sse2(cells: &[RTreeCell10], node_id: i64) -> RTreeCell10 {
    debug_assert!(!cells.is_empty());
    let mut out = cells[0];
    out.id = node_id;
    if cells.len() == 1 {
        return out;
    }
    // SAFETY: Caller verified `CAP_SSE2` and `cells.len() >= 2`.
    unsafe {
        let c0 = cells.as_ptr().cast::<u8>();
        let ptr = c0.add(48);
        let end = c0.add(cells.len() * 48);
        let out_coords = (&raw mut out.coords).cast::<u8>();
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        asm!(
            "movaps {flip}, xmmword ptr [{k} + 16]",
            "movups {va}, xmmword ptr [{c0} + 8]",
            "xorps {va}, {flip}",
            "2:",
            "movups {vb}, xmmword ptr [{ptr} + 8]",
            "xorps {vb}, {flip}",
            "movaps {vm}, {vb}",
            "cmpleps {vm}, {va}",
            "andps {vb}, {vm}",
            "andnps {vm}, {va}",
            "orps {vb}, {vm}",
            "movaps {va}, {vb}",
            "add {ptr}, 48",
            "cmp {ptr}, {end}",
            "jb 2b",
            "xorps {va}, {flip}",
            "movups xmmword ptr [{out_c}], {va}",
            c0 = in(reg) c0,
            ptr = inout(reg) ptr => _,
            end = in(reg) end,
            out_c = in(reg) out_coords,
            k = in(reg) k,
            flip = out(xmm_reg) _,
            va = out(xmm_reg) _,
            vb = out(xmm_reg) _,
            vm = out(xmm_reg) _,
            options(nostack),
        );
    }
    out
}

/// Computes the 2D `f32` bounding box over a non-empty node slice `cells`, keeping the
/// running `[min_x, max_x, min_y, max_y]` accumulator in a single `xmm` register across the
/// entire node with exact IEEE-754 signed-zero and `NaN` parity.
#[inline]
pub fn rtree_bbox_union_2d_f32(cells: &[RTreeCell10], node_id: i64) -> RTreeCell10 {
    assert!(
        !cells.is_empty(),
        "rtree_bbox_union_2d_f32 requires non-empty cells"
    );
    if cells.len() == 1 {
        let mut out = cells[0];
        out.id = node_id;
        return out;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let caps = cpu_caps();
        if (caps & CAP_AVX2) != 0 {
            // SAFETY: `CAP_AVX2` is verified and `cells` is non-empty.
            return unsafe { rtree_bbox_union_2d_f32_avx128(cells, node_id) };
        }
        if (caps & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified and `cells` is non-empty.
            return unsafe { rtree_bbox_union_2d_f32_sse2(cells, node_id) };
        }
    }
    rtree_bbox_union_2d_f32_scalar(cells, node_id)
}

/// Plain scalar fallback for 2D `f32` `choose_subtree` over a non-empty internal node `cells`.
#[inline]
pub fn rtree_choose_subtree_2d_f32_scalar(cells: &[RTreeCell10], cand: &RTreeCell10) -> i64 {
    assert!(
        !cells.is_empty(),
        "rtree_choose_subtree_2d_f32_scalar requires non-empty cells"
    );
    let b0 = f32::from_bits(cand.coords[0]);
    let b1 = f32::from_bits(cand.coords[1]);
    let b2 = f32::from_bits(cand.coords[2]);
    let b3 = f32::from_bits(cand.coords[3]);

    let mut best_id = cells[0].id;
    let mut found_containing = false;
    let mut min_area = 0.0_f64;

    for c in cells {
        let a0 = f32::from_bits(c.coords[0]);
        let a1 = f32::from_bits(c.coords[1]);
        let a2 = f32::from_bits(c.coords[2]);
        let a3 = f32::from_bits(c.coords[3]);
        if !(b0 < a0 || b1 > a1 || b2 < a2 || b3 > a3) {
            let area = (f64::from(a1) - f64::from(a0)) * (f64::from(a3) - f64::from(a2));
            if !found_containing || area < min_area {
                best_id = c.id;
                min_area = area;
                found_containing = true;
            }
        }
    }

    if !found_containing {
        let mut min_growth = 0.0_f64;
        for (idx, c) in cells.iter().enumerate() {
            let a0 = f32::from_bits(c.coords[0]);
            let a1 = f32::from_bits(c.coords[1]);
            let a2 = f32::from_bits(c.coords[2]);
            let a3 = f32::from_bits(c.coords[3]);
            let area = (f64::from(a1) - f64::from(a0)) * (f64::from(a3) - f64::from(a2));
            let u0 = if a0 >= b0 { b0 } else { a0 };
            let u1 = if a1 <= b1 { b1 } else { a1 };
            let u2 = if a2 >= b2 { b2 } else { a2 };
            let u3 = if a3 <= b3 { b3 } else { a3 };
            let u_area = (f64::from(u1) - f64::from(u0)) * (f64::from(u3) - f64::from(u2));
            let growth = u_area - area;
            if idx == 0 || growth < min_growth || (growth == min_growth && area < min_area) {
                min_growth = growth;
                min_area = area;
                best_id = c.id;
            }
        }
    }
    best_id
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rtree_choose_subtree_2d_f32_avx2(cells: &[RTreeCell10], cand: &RTreeCell10) -> i64 {
    debug_assert!(!cells.is_empty());
    let ptr = cells.as_ptr().cast::<u8>();
    // SAFETY: Caller verified `CAP_AVX2`, `!cells.is_empty()`, and that `cand.coords[0..4]` are
    // non-NaN `f32` values (note: if any cell in `cells` contains `NaN`, check `b0 < a0` in Rust
    // `!(b0 < a0 || ...)` would evaluate `false < NaN = false` -> `true`! So we verify no `NaN`
    // in `cells` via an ordered self-compare mask in the loop and fall back if a corrupt `NaN`
    // cell is encountered).
    unsafe {
        let end = ptr.add(cells.len() * 48);
        let cand_ptr = (&raw const cand.coords).cast::<u8>();
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        let best_c: i64;
        let best_g: i64;
        let flags: u64;
        let m_tmp: u32;
        asm!(
            "vmovapd {ysign}, ymmword ptr [{k} + 32]",
            "vcvtps2pd {yb}, xmmword ptr [{cand}]",
            "vxorpd {ybf}, {yb}, {ysign}",
            "xor {flags:e}, {flags:e}",
            "xor {best_c}, {best_c}",
            "xor {best_g}, {best_g}",
            ".p2align 4",
            "2:",
            "vcvtps2pd {ya}, xmmword ptr [{ptr} + 8]",
            "vcmppd {yt0}, {ya}, {ya}, 3",
            "vmovmskpd {m:e}, {yt0}",
            "test {m:e}, {m:e}",
            "jnz 8f",
            "vxorpd {yaf}, {ya}, {ysign}",
            "vcmppd {yt0}, {yaf}, {ybf}, 0x1d",
            "vmovmskpd {m:e}, {yt0}",
            "cmp {m:e}, 15",
            "je 3f",

            "test {flags:l}, 1",
            "jnz 7f",

            "vextractf128 {yt0:x}, {ya}, 1",
            "vunpcklpd {yt1:x}, {ya:x}, {yt0:x}",
            "vunpckhpd {yt2:x}, {ya:x}, {yt0:x}",
            "vsubpd {yt1:x}, {yt2:x}, {yt1:x}",
            "vpermilpd {yt2:x}, {yt1:x}, 1",
            "vmulsd {yt1:x}, {yt1:x}, {yt2:x}",

            "vcmppd {yt0}, {yaf}, {ybf}, 0x12",
            "vblendvpd {yt0}, {ya}, {yb}, {yt0}",
            "vextractf128 {yaf:x}, {yt0}, 1",
            "vunpcklpd {yt2:x}, {yt0:x}, {yaf:x}",
            "vunpckhpd {yt0:x}, {yt0:x}, {yaf:x}",
            "vsubpd {yt0:x}, {yt0:x}, {yt2:x}",
            "vpermilpd {yt2:x}, {yt0:x}, 1",
            "vmulsd {yt0:x}, {yt0:x}, {yt2:x}",
            "vsubsd {yt0:x}, {yt0:x}, {yt1:x}",

            "test {flags:l}, 2",
            "jz 5f",
            "vucomisd {min_g:x}, {yt0:x}",
            "ja 5f",
            "jne 7f",
            "jp 7f",
            "vucomisd {min_ga:x}, {yt1:x}",
            "jbe 7f",
            "5:",
            "vmovapd {min_g:x}, {yt0:x}",
            "vmovapd {min_ga:x}, {yt1:x}",
            "mov {best_g}, qword ptr [{ptr}]",
            "or {flags:l}, 2",
            "jmp 7f",

            "3:",
            "vextractf128 {yt0:x}, {ya}, 1",
            "vunpcklpd {yt1:x}, {ya:x}, {yt0:x}",
            "vunpckhpd {yt2:x}, {ya:x}, {yt0:x}",
            "vsubpd {yt1:x}, {yt2:x}, {yt1:x}",
            "vpermilpd {yt2:x}, {yt1:x}, 1",
            "vmulsd {yt1:x}, {yt1:x}, {yt2:x}",
            "test {flags:l}, 1",
            "jz 6f",
            "vucomisd {min_ca:x}, {yt1:x}",
            "jbe 7f",
            "6:",
            "vmovapd {min_ca:x}, {yt1:x}",
            "mov {best_c}, qword ptr [{ptr}]",
            "or {flags:l}, 1",

            "7:",
            "add {ptr}, 48",
            "cmp {ptr}, {end}",
            "jb 2b",
            "jmp 9f",

            "8:",
            "mov {flags:e}, 4",
            "9:",
            "vzeroupper",
            ptr = inout(reg) ptr => _,
            end = in(reg) end,
            cand = in(reg) cand_ptr,
            k = in(reg) k,
            best_c = out(reg) best_c,
            best_g = out(reg) best_g,
            flags = out(reg) flags,
            m = out(reg) m_tmp,
            ysign = out(ymm_reg) _,
            yb = out(ymm_reg) _,
            ybf = out(ymm_reg) _,
            min_ca = out(ymm_reg) _,
            min_g = out(ymm_reg) _,
            min_ga = out(ymm_reg) _,
            ya = out(ymm_reg) _,
            yaf = out(ymm_reg) _,
            yt0 = out(ymm_reg) _,
            yt1 = out(ymm_reg) _,
            yt2 = out(ymm_reg) _,
            options(nostack, readonly),
        );
        let _ = m_tmp;
        if (flags & 4) != 0 {
            return rtree_choose_subtree_2d_f32_scalar(cells, cand);
        }
        if (flags & 1) != 0 { best_c } else { best_g }
    }
}

/// Selects the best child cell ID in a non-empty 2D `f32` internal R-Tree node for inserting `cand`,
/// fusing containment and minimum-area-enlargement passes into a single register-resident `AVX2` sweep.
#[inline]
pub fn rtree_choose_subtree_2d_f32(cells: &[RTreeCell10], cand: &RTreeCell10) -> i64 {
    assert!(
        !cells.is_empty(),
        "rtree_choose_subtree_2d_f32 requires non-empty cells"
    );
    if cells.len() == 1 {
        return cells[0].id;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let b0 = f32::from_bits(cand.coords[0]);
        let b1 = f32::from_bits(cand.coords[1]);
        let b2 = f32::from_bits(cand.coords[2]);
        let b3 = f32::from_bits(cand.coords[3]);
        if !b0.is_nan()
            && !b1.is_nan()
            && !b2.is_nan()
            && !b3.is_nan()
            && (cpu_caps() & CAP_AVX2) != 0
        {
            // SAFETY: `CAP_AVX2` is verified, `cells.len() >= 2`, and `b0..b3` are non-NaN.
            return unsafe { rtree_choose_subtree_2d_f32_avx2(cells, cand) };
        }
    }
    rtree_choose_subtree_2d_f32_scalar(cells, cand)
}

/// Plain scalar fallback filtering internal-node 2D `f32` cells overlapping `[q_min_x, q_max_x] x [q_min_y, q_max_y]`,
/// writing matching child `id`s into `out_ids` and returning the match count.
#[inline]
pub fn rtree_filter_2d_f32_ids_scalar(
    cells: &[RTreeCell10],
    q_min_x: f64,
    q_max_x: f64,
    q_min_y: f64,
    q_max_y: f64,
    out_ids: &mut [i64],
) -> usize {
    assert!(
        out_ids.len() >= cells.len(),
        "out_ids buffer must be at least cells.len()"
    );
    let mut n = 0usize;
    for cell in cells {
        let min_x = f64::from(f32::from_bits(cell.coords[0]));
        let max_x = f64::from(f32::from_bits(cell.coords[1]));
        let min_y = f64::from(f32::from_bits(cell.coords[2]));
        let max_y = f64::from(f32::from_bits(cell.coords[3]));
        if max_x >= q_min_x && min_x <= q_max_x && max_y >= q_min_y && min_y <= q_max_y {
            out_ids[n] = cell.id;
            n += 1;
        }
    }
    n
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rtree_filter_2d_f32_ids_avx2(
    cells: &[RTreeCell10],
    q_flipped: &[f64; 4],
    out_ids: &mut [i64],
) -> usize {
    let len = cells.len();
    debug_assert!(len > 0 && out_ids.len() >= len);
    // SAFETY: Caller verified `CAP_AVX2`, `len > 0`, and `out_ids.len() >= len`. Before inspecting
    // cell `i` (`0 <= i < len`), `cnt <= i < len <= out_ids.len()`, so writing `out_ids[cnt]`
    // unconditionally before branchless `cnt += (mask == 15)` is always strictly within `out_ids`.
    unsafe {
        let ptr = cells.as_ptr().cast::<u8>();
        let end = ptr.add(len * 48);
        let end2 = ptr.add((len & !1) * 48);
        let q_ptr = q_flipped.as_ptr().cast::<u8>();
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        let out_ptr = out_ids.as_mut_ptr();
        let cnt: usize;
        let m0: u32;
        let m1: u32;
        let id_tmp: i64;
        let inc_tmp: usize;
        asm!(
            "vmovapd {ysign}, ymmword ptr [{k} + 32]",
            "vmovupd {yq}, ymmword ptr [{qp}]",
            "xor {cnt:e}, {cnt:e}",
            "cmp {ptr}, {end2}",
            "jae 3f",
            ".p2align 4",
            "2:",
            "vcvtps2pd {a0}, xmmword ptr [{ptr} + 8]",
            "vcvtps2pd {a1}, xmmword ptr [{ptr} + 56]",
            "vxorpd {a0}, {a0}, {ysign}",
            "vxorpd {a1}, {a1}, {ysign}",
            "vcmppd {a0}, {a0}, {yq}, 0x1d",
            "vcmppd {a1}, {a1}, {yq}, 0x1d",
            "vmovmskpd {m0:e}, {a0}",
            "vmovmskpd {m1:e}, {a1}",

            "mov {id}, qword ptr [{ptr}]",
            "mov qword ptr [{out} + {cnt}*8], {id}",
            "xor {inc:e}, {inc:e}",
            "cmp {m0:e}, 15",
            "sete {inc:l}",
            "add {cnt}, {inc}",

            "mov {id}, qword ptr [{ptr} + 48]",
            "mov qword ptr [{out} + {cnt}*8], {id}",
            "xor {inc:e}, {inc:e}",
            "cmp {m1:e}, 15",
            "sete {inc:l}",
            "add {cnt}, {inc}",

            "add {ptr}, 96",
            "cmp {ptr}, {end2}",
            "jb 2b",

            "3:",
            "cmp {ptr}, {end}",
            "jae 4f",
            "vcvtps2pd {a0}, xmmword ptr [{ptr} + 8]",
            "vxorpd {a0}, {a0}, {ysign}",
            "vcmppd {a0}, {a0}, {yq}, 0x1d",
            "vmovmskpd {m0:e}, {a0}",
            "mov {id}, qword ptr [{ptr}]",
            "mov qword ptr [{out} + {cnt}*8], {id}",
            "xor {inc:e}, {inc:e}",
            "cmp {m0:e}, 15",
            "sete {inc:l}",
            "add {cnt}, {inc}",
            "4:",
            "vzeroupper",
            ptr = inout(reg) ptr => _,
            end2 = in(reg) end2,
            end = in(reg) end,
            qp = in(reg) q_ptr,
            k = in(reg) k,
            out = in(reg) out_ptr,
            cnt = out(reg) cnt,
            m0 = out(reg) m0,
            m1 = out(reg) m1,
            id = out(reg) id_tmp,
            inc = out(reg) inc_tmp,
            ysign = out(ymm_reg) _,
            yq = out(ymm_reg) _,
            a0 = out(ymm_reg) _,
            a1 = out(ymm_reg) _,
            options(nostack),
        );
        let _ = (m0, m1, id_tmp, inc_tmp);
        cnt
    }
}

/// Filters internal-node 2D `f32` cells overlapping `[q_min_x, q_max_x] x [q_min_y, q_max_y]`,
/// writing matching child `id`s into `out_ids` via a branchless 2-wide `AVX2` sweep.
#[inline]
pub fn rtree_filter_2d_f32_ids(
    cells: &[RTreeCell10],
    q_min_x: f64,
    q_max_x: f64,
    q_min_y: f64,
    q_max_y: f64,
    out_ids: &mut [i64],
) -> usize {
    assert!(
        out_ids.len() >= cells.len(),
        "out_ids buffer must be at least cells.len()"
    );
    if cells.is_empty() {
        return 0;
    }
    if q_min_x.is_nan() || q_max_x.is_nan() || q_min_y.is_nan() || q_max_y.is_nan() {
        return 0;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if (cpu_caps() & CAP_AVX2) != 0 {
            let q_flipped = [-q_max_x, q_min_x, -q_max_y, q_min_y];
            // SAFETY: `CAP_AVX2` is verified, `!cells.is_empty()`, `out_ids.len() >= cells.len()`,
            // and query bounds are non-NaN.
            return unsafe { rtree_filter_2d_f32_ids_avx2(cells, &q_flipped, out_ids) };
        }
    }
    rtree_filter_2d_f32_ids_scalar(cells, q_min_x, q_max_x, q_min_y, q_max_y, out_ids)
}

/// Plain scalar fallback appending leaf 2D `f32` cells overlapping `[q_min_x, q_max_x] x [q_min_y, q_max_y]` to `out`.
#[inline]
pub fn rtree_filter_2d_f32_cells_scalar(
    cells: &[RTreeCell10],
    q_min_x: f64,
    q_max_x: f64,
    q_min_y: f64,
    q_max_y: f64,
    out: &mut Vec<RTreeCell10>,
) {
    out.reserve(cells.len());
    for cell in cells {
        let min_x = f64::from(f32::from_bits(cell.coords[0]));
        let max_x = f64::from(f32::from_bits(cell.coords[1]));
        let min_y = f64::from(f32::from_bits(cell.coords[2]));
        let max_y = f64::from(f32::from_bits(cell.coords[3]));
        if max_x >= q_min_x && min_x <= q_max_x && max_y >= q_min_y && min_y <= q_max_y {
            out.push(*cell);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rtree_filter_2d_f32_cells_avx2(
    cells: &[RTreeCell10],
    q_flipped: &[f64; 4],
    dst_start: *mut RTreeCell10,
) -> usize {
    let len = cells.len();
    debug_assert!(len > 0);
    // SAFETY: Caller verified `CAP_AVX2`, `len > 0`, and reserved at least `len` spare
    // `RTreeCell10` slots (`len * 48` writable bytes) starting at `dst_start`. Before inspecting
    // cell `i` (`0 <= i < len`), `(dst - dst_start) / 48 <= i < len`, so writing 48 bytes at
    // `dst` before branchless `dst += (mask == 15) * 48` is always strictly in bounds.
    unsafe {
        let ptr = cells.as_ptr().cast::<u8>();
        let end = ptr.add(len * 48);
        let end2 = ptr.add((len & !1) * 48);
        let dst0 = dst_start.cast::<u8>();
        let dst_out: *mut u8;
        let q_ptr = q_flipped.as_ptr().cast::<u8>();
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        let m0: u32;
        let m1: u32;
        let inc_tmp: usize;
        asm!(
            "vmovapd {ysign}, ymmword ptr [{k} + 32]",
            "vmovupd {yq}, ymmword ptr [{qp}]",
            "cmp {ptr}, {end2}",
            "jae 3f",
            ".p2align 4",
            "2:",
            "vcvtps2pd {a0}, xmmword ptr [{ptr} + 8]",
            "vcvtps2pd {a1}, xmmword ptr [{ptr} + 56]",
            "vxorpd {a0}, {a0}, {ysign}",
            "vxorpd {a1}, {a1}, {ysign}",
            "vcmppd {a0}, {a0}, {yq}, 0x1d",
            "vcmppd {a1}, {a1}, {yq}, 0x1d",
            "vmovmskpd {m0:e}, {a0}",
            "vmovmskpd {m1:e}, {a1}",

            "vmovdqu {c0}, ymmword ptr [{ptr}]",
            "vmovdqu {c1:x}, xmmword ptr [{ptr} + 32]",
            "vmovdqu ymmword ptr [{dst}], {c0}",
            "vmovdqu xmmword ptr [{dst} + 32], {c1:x}",
            "xor {inc:e}, {inc:e}",
            "cmp {m0:e}, 15",
            "sete {inc:l}",
            "lea {inc}, [{inc} + {inc}*2]",
            "shl {inc}, 4",
            "add {dst}, {inc}",

            "vmovdqu {c0}, ymmword ptr [{ptr} + 48]",
            "vmovdqu {c1:x}, xmmword ptr [{ptr} + 80]",
            "vmovdqu ymmword ptr [{dst}], {c0}",
            "vmovdqu xmmword ptr [{dst} + 32], {c1:x}",
            "xor {inc:e}, {inc:e}",
            "cmp {m1:e}, 15",
            "sete {inc:l}",
            "lea {inc}, [{inc} + {inc}*2]",
            "shl {inc}, 4",
            "add {dst}, {inc}",

            "add {ptr}, 96",
            "cmp {ptr}, {end2}",
            "jb 2b",

            "3:",
            "cmp {ptr}, {end}",
            "jae 4f",
            "vcvtps2pd {a0}, xmmword ptr [{ptr} + 8]",
            "vxorpd {a0}, {a0}, {ysign}",
            "vcmppd {a0}, {a0}, {yq}, 0x1d",
            "vmovmskpd {m0:e}, {a0}",
            "vmovdqu {c0}, ymmword ptr [{ptr}]",
            "vmovdqu {c1:x}, xmmword ptr [{ptr} + 32]",
            "vmovdqu ymmword ptr [{dst}], {c0}",
            "vmovdqu xmmword ptr [{dst} + 32], {c1:x}",
            "xor {inc:e}, {inc:e}",
            "cmp {m0:e}, 15",
            "sete {inc:l}",
            "lea {inc}, [{inc} + {inc}*2]",
            "shl {inc}, 4",
            "add {dst}, {inc}",
            "4:",
            "vzeroupper",
            ptr = inout(reg) ptr => _,
            dst = inout(reg) dst0 => dst_out,
            end2 = in(reg) end2,
            end = in(reg) end,
            qp = in(reg) q_ptr,
            k = in(reg) k,
            m0 = out(reg) m0,
            m1 = out(reg) m1,
            inc = out(reg) inc_tmp,
            ysign = out(ymm_reg) _,
            yq = out(ymm_reg) _,
            a0 = out(ymm_reg) _,
            a1 = out(ymm_reg) _,
            c0 = out(ymm_reg) _,
            c1 = out(ymm_reg) _,
            options(nostack),
        );
        let _ = (m0, m1, inc_tmp);
        (dst_out.offset_from(dst0) as usize) / 48
    }
}

/// Appends leaf 2D `f32` cells overlapping `[q_min_x, q_max_x] x [q_min_y, q_max_y]` directly
/// into `out`'s spare capacity via a branchless 2-wide `AVX2` sweep.
#[inline]
pub fn rtree_filter_2d_f32_cells(
    cells: &[RTreeCell10],
    q_min_x: f64,
    q_max_x: f64,
    q_min_y: f64,
    q_max_y: f64,
    out: &mut Vec<RTreeCell10>,
) {
    if cells.is_empty() {
        return;
    }
    if q_min_x.is_nan() || q_max_x.is_nan() || q_min_y.is_nan() || q_max_y.is_nan() {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if (cpu_caps() & CAP_AVX2) != 0 {
            let n = cells.len();
            out.reserve(n);
            let old_len = out.len();
            let q_flipped = [-q_max_x, q_min_x, -q_max_y, q_min_y];
            // SAFETY: `CAP_AVX2` is verified, `out.reserve(n)` guarantees at least `n` writable
            // `RTreeCell10` slots at `out.as_mut_ptr().add(old_len)`, and `added <= n` contiguous
            // slots are initialized by `rtree_filter_2d_f32_cells_avx2`.
            unsafe {
                let dst = out.as_mut_ptr().add(old_len);
                let added = rtree_filter_2d_f32_cells_avx2(cells, &q_flipped, dst);
                out.set_len(old_len + added);
            }
            return;
        }
    }
    rtree_filter_2d_f32_cells_scalar(cells, q_min_x, q_max_x, q_min_y, q_max_y, out);
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn rtree_cell_union_2d_f32_sse2_one(acc: &mut RTreeCell10, other: &RTreeCell10) {
    // SAFETY: Caller verified `SSE2`; `acc.coords[0..4]` and `other.coords[0..4]` are valid
    // 16-byte regions.
    unsafe {
        let a_ptr = (&raw mut acc.coords).cast::<u8>();
        let b_ptr = (&raw const other.coords).cast::<u8>();
        let k = (&raw const RTREE_CONSTS).cast::<u8>();
        asm!(
            "movaps {flip}, xmmword ptr [{k} + 16]",
            "movups {va}, xmmword ptr [{ap}]",
            "movups {vb}, xmmword ptr [{bp}]",
            "xorps {va}, {flip}",
            "xorps {vb}, {flip}",
            "movaps {vm}, {vb}",
            "cmpleps {vm}, {va}",
            "andps {vb}, {vm}",
            "andnps {vm}, {va}",
            "orps {vb}, {vm}",
            "xorps {vb}, {flip}",
            "movups xmmword ptr [{ap}], {vb}",
            ap = in(reg) a_ptr,
            bp = in(reg) b_ptr,
            k = in(reg) k,
            flip = out(xmm_reg) _,
            va = out(xmm_reg) _,
            vb = out(xmm_reg) _,
            vm = out(xmm_reg) _,
            options(nostack),
        );
    }
}

/// Branchless single-cell 2D `f32` bounding-box union into `acc` using inline `SSE2`.
#[inline(always)]
pub fn rtree_cell_union_2d_f32_in_place(acc: &mut RTreeCell10, other: &RTreeCell10) {
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    {
        // SAFETY: `target_feature = "sse2"` is enabled at compile time on `x86_64`.
        unsafe {
            rtree_cell_union_2d_f32_sse2_one(acc, other);
        }
        return;
    }
    #[cfg(all(target_arch = "x86_64", not(target_feature = "sse2")))]
    {
        if (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified.
            unsafe {
                rtree_cell_union_2d_f32_sse2_one(acc, other);
            }
            return;
        }
    }
    #[allow(unreachable_code)]
    rtree_cell_union_2d_f32_in_place_scalar(acc, other);
}

// ============================================================================
// ASM-6a: Inline `movbe` / `SSE2` Raw Binary & ASCII `NOCASE` Key Comparison
// ============================================================================

#[cfg(target_arch = "x86_64")]
#[repr(C, align(16))]
struct AsciiNoCaseConsts {
    c40: [u8; 16],
    c5b: [u8; 16],
    c20: [u8; 16],
}

#[cfg(target_arch = "x86_64")]
static ASCII_NOCASE_CONSTS: AsciiNoCaseConsts = AsciiNoCaseConsts {
    c40: [0x40; 16],
    c5b: [0x5b; 16],
    c20: [0x20; 16],
};

/// Plain scalar fallback for binary byte-slice ordering (`memcmp`).
#[inline]
pub fn cmp_bytes_binary_scalar(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let min_len = a.len().min(b.len());
    let mut i = 0usize;
    while i < min_len {
        let ba = a[i];
        let bb = b[i];
        if ba != bb {
            return ba.cmp(&bb);
        }
        i += 1;
    }
    a.len().cmp(&b.len())
}

/// Plain scalar fallback for SQLite `NOCASE` byte-slice ordering (folds ASCII `'A'..='Z'` only).
#[inline]
pub fn cmp_bytes_ascii_nocase_scalar(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let min_len = a.len().min(b.len());
    let mut i = 0usize;
    while i < min_len {
        let la = a[i].to_ascii_lowercase();
        let lb = b[i].to_ascii_lowercase();
        if la != lb {
            return la.cmp(&lb);
        }
        i += 1;
    }
    a.len().cmp(&b.len())
}

/// Plain scalar fallback for fused ASCII-UTF8 validation + `BINARY` comparison on raw slices.
#[inline]
pub fn cmp_utf8_ascii_binary_scalar(a: &[u8], b: &[u8]) -> Option<std::cmp::Ordering> {
    let min_len = a.len().min(b.len());
    let mut i = 0usize;
    while i < min_len {
        let ba = a[i];
        let bb = b[i];
        if (ba | bb) >= 0x80 {
            return None;
        }
        if ba != bb {
            return Some(ba.cmp(&bb));
        }
        i += 1;
    }
    Some(a.len().cmp(&b.len()))
}

/// Plain scalar fallback for fused ASCII-UTF8 validation + `NOCASE` comparison on raw slices.
#[inline]
pub fn cmp_utf8_ascii_nocase_scalar(a: &[u8], b: &[u8]) -> Option<std::cmp::Ordering> {
    let min_len = a.len().min(b.len());
    let mut i = 0usize;
    while i < min_len {
        let ba = a[i];
        let bb = b[i];
        if (ba | bb) >= 0x80 {
            return None;
        }
        let la = ba.to_ascii_lowercase();
        let lb = bb.to_ascii_lowercase();
        if la != lb {
            return Some(la.cmp(&lb));
        }
        i += 1;
    }
    Some(a.len().cmp(&b.len()))
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn cmp_binary_movbe_4_7(ap: *const u8, bp: *const u8, min_len: usize) -> i32 {
    debug_assert!((4..8).contains(&min_len));
    let off = min_len - 4;
    let res: i32;
    let wa: u32;
    let wb: u32;
    let gt: u32;
    let lt: u32;
    // SAFETY: Caller verified `CAP_MOVBE` and `min_len in 4..8` readable bytes at `ap` and `bp`.
    unsafe {
        asm!(
            "movbe {wa:e}, dword ptr [{ap}]",
            "movbe {wb:e}, dword ptr [{bp}]",
            "cmp {wa:e}, {wb:e}",
            "jne 2f",
            "movbe {wa:e}, dword ptr [{ap} + {off}]",
            "movbe {wb:e}, dword ptr [{bp} + {off}]",
            "cmp {wa:e}, {wb:e}",
            "2:",
            "seta {gt:l}",
            "setb {lt:l}",
            "movzx {res:e}, {gt:l}",
            "movzx {wb:e}, {lt:l}",
            "sub {res:e}, {wb:e}",
            ap = in(reg) ap,
            bp = in(reg) bp,
            off = in(reg) off,
            wa = out(reg) wa,
            wb = out(reg) wb,
            gt = out(reg) gt,
            lt = out(reg) lt,
            res = out(reg) res,
            options(nostack, readonly),
        );
    }
    let _ = (wa, wb, gt, lt);
    res
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn cmp_binary_movbe_8plus(ap: *const u8, bp: *const u8, min_len: usize) -> i32 {
    debug_assert!(min_len >= 8);
    let res: i32;
    let wa: u64;
    let wb: u64;
    let idx: usize;
    let lim: usize;
    let gt: u32;
    // SAFETY: Caller verified `CAP_MOVBE` and `min_len >= 8` readable bytes at `ap` and `bp`.
    unsafe {
        asm!(
            "movbe {wa}, qword ptr [{ap}]",
            "movbe {wb}, qword ptr [{bp}]",
            "cmp {wa}, {wb}",
            "jne 4f",
            "cmp {len}, 8",
            "je 5f",
            "xor {idx:e}, {idx:e}",
            "lea {lim}, [{len} - 8]",
            "2:",
            "add {idx}, 8",
            "cmp {idx}, {lim}",
            "jae 3f",
            "movbe {wa}, qword ptr [{ap} + {idx}]",
            "movbe {wb}, qword ptr [{bp} + {idx}]",
            "cmp {wa}, {wb}",
            "je 2b",
            "jmp 4f",
            "3:",
            "movbe {wa}, qword ptr [{ap} + {lim}]",
            "movbe {wb}, qword ptr [{bp} + {lim}]",
            "cmp {wa}, {wb}",
            "jne 4f",
            "5:",
            "xor {res:e}, {res:e}",
            "jmp 6f",
            "4:",
            "seta {gt:l}",
            "setb {idx:l}",
            "movzx {res:e}, {gt:l}",
            "movzx {idx:e}, {idx:l}",
            "sub {res:e}, {idx:e}",
            "6:",
            ap = in(reg) ap,
            bp = in(reg) bp,
            len = in(reg) min_len,
            wa = out(reg) wa,
            wb = out(reg) wb,
            idx = out(reg) idx,
            lim = out(reg) lim,
            gt = out(reg) gt,
            res = out(reg) res,
            options(nostack, readonly),
        );
    }
    let _ = (wa, wb, idx, lim, gt);
    res
}

/// Lexicographical binary comparison (`memcmp`) accelerated with inline `x86-64` `movbe`.
#[inline(never)]
pub fn cmp_bytes_binary(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let min_len = a.len().min(b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if min_len >= 4 && (cpu_caps() & CAP_MOVBE) != 0 {
            // SAFETY: `CAP_MOVBE` is verified and `min_len <= a.len(), b.len()`.
            let diff = unsafe {
                if min_len < 8 {
                    cmp_binary_movbe_4_7(a.as_ptr(), b.as_ptr(), min_len)
                } else {
                    cmp_binary_movbe_8plus(a.as_ptr(), b.as_ptr(), min_len)
                }
            };
            return if diff < 0 {
                std::cmp::Ordering::Less
            } else if diff > 0 {
                std::cmp::Ordering::Greater
            } else {
                a.len().cmp(&b.len())
            };
        }
    }
    let _ = min_len;
    cmp_bytes_binary_scalar(a, b)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn cmp_utf8_ascii_binary_movbe_4_7(ap: *const u8, bp: *const u8, min_len: usize) -> i32 {
    debug_assert!((4..8).contains(&min_len));
    let off = min_len - 4;
    let res: i32;
    let wa: u32;
    let wb: u32;
    let tmp: u32;
    let gt: u32;
    // Returns `-1` (Less), `0` (Equal), `+1` (Greater), or `2` (non-ASCII byte encountered).
    // SAFETY: Caller verified `CAP_MOVBE` and `min_len in 4..8` readable bytes at `ap` and `bp`.
    unsafe {
        asm!(
            "movbe {wa:e}, dword ptr [{ap}]",
            "movbe {wb:e}, dword ptr [{bp}]",
            "mov {tmp:e}, {wa:e}",
            "or {tmp:e}, {wb:e}",
            "test {tmp:e}, -2139062144", // 0x8080_8080 as i32
            "jnz 4f",
            "cmp {wa:e}, {wb:e}",
            "jne 2f",
            "movbe {wa:e}, dword ptr [{ap} + {off}]",
            "movbe {wb:e}, dword ptr [{bp} + {off}]",
            "mov {tmp:e}, {wa:e}",
            "or {tmp:e}, {wb:e}",
            "test {tmp:e}, -2139062144",
            "jnz 4f",
            "cmp {wa:e}, {wb:e}",
            "jne 2f",
            "xor {res:e}, {res:e}",
            "jmp 5f",
            "2:",
            "seta {gt:l}",
            "setb {tmp:l}",
            "movzx {res:e}, {gt:l}",
            "movzx {tmp:e}, {tmp:l}",
            "sub {res:e}, {tmp:e}",
            "jmp 5f",
            "4:",
            "mov {res:e}, 2",
            "5:",
            ap = in(reg) ap,
            bp = in(reg) bp,
            off = in(reg) off,
            wa = out(reg) wa,
            wb = out(reg) wb,
            tmp = out(reg) tmp,
            gt = out(reg) gt,
            res = out(reg) res,
            options(nostack, readonly),
        );
    }
    let _ = (wa, wb, tmp, gt);
    res
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn cmp_utf8_ascii_binary_movbe_8plus(ap: *const u8, bp: *const u8, min_len: usize) -> i32 {
    debug_assert!(min_len >= 8);
    let msbs: u64 = 0x8080_8080_8080_8080;
    let res: i32;
    let wa: u64;
    let wb: u64;
    let idx: usize;
    let lim: usize;
    let tmp: u64;
    // SAFETY: Caller verified `CAP_MOVBE` and `min_len >= 8` readable bytes at `ap` and `bp`.
    unsafe {
        asm!(
            "movbe {wa}, qword ptr [{ap}]",
            "movbe {wb}, qword ptr [{bp}]",
            "mov {tmp}, {wa}",
            "or {tmp}, {wb}",
            "test {tmp}, {msbs}",
            "jnz 6f",
            "cmp {wa}, {wb}",
            "jne 4f",
            "cmp {len}, 8",
            "je 5f",
            "xor {idx:e}, {idx:e}",
            "lea {lim}, [{len} - 8]",
            "2:",
            "add {idx}, 8",
            "cmp {idx}, {lim}",
            "jae 3f",
            "movbe {wa}, qword ptr [{ap} + {idx}]",
            "movbe {wb}, qword ptr [{bp} + {idx}]",
            "mov {tmp}, {wa}",
            "or {tmp}, {wb}",
            "test {tmp}, {msbs}",
            "jnz 6f",
            "cmp {wa}, {wb}",
            "je 2b",
            "jmp 4f",
            "3:",
            "movbe {wa}, qword ptr [{ap} + {lim}]",
            "movbe {wb}, qword ptr [{bp} + {lim}]",
            "mov {tmp}, {wa}",
            "or {tmp}, {wb}",
            "test {tmp}, {msbs}",
            "jnz 6f",
            "cmp {wa}, {wb}",
            "jne 4f",
            "5:",
            "xor {res:e}, {res:e}",
            "jmp 7f",
            "4:",
            "seta {tmp:l}",
            "setb {idx:l}",
            "movzx {res:e}, {tmp:l}",
            "movzx {idx:e}, {idx:l}",
            "sub {res:e}, {idx:e}",
            "jmp 7f",
            "6:",
            "mov {res:e}, 2",
            "7:",
            ap = in(reg) ap,
            bp = in(reg) bp,
            len = in(reg) min_len,
            msbs = in(reg) msbs,
            wa = out(reg) wa,
            wb = out(reg) wb,
            idx = out(reg) idx,
            lim = out(reg) lim,
            tmp = out(reg) tmp,
            res = out(reg) res,
            options(nostack, readonly),
        );
    }
    let _ = (wa, wb, idx, lim, tmp);
    res
}

/// Fused ASCII UTF-8 validation and `BINARY` key comparison on raw byte slices.
/// Returns `Some(Ordering)` if the deciding prefix is pure ASCII (`< 0x80`), or `None` if a
/// non-ASCII byte is encountered so caller can fall back to full UTF-8 decoding.
#[inline(never)]
pub fn cmp_utf8_ascii_binary(a: &[u8], b: &[u8]) -> Option<std::cmp::Ordering> {
    let min_len = a.len().min(b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if min_len >= 4 && (cpu_caps() & CAP_MOVBE) != 0 {
            // SAFETY: `CAP_MOVBE` is verified and `min_len <= a.len(), b.len()`.
            let code = unsafe {
                if min_len < 8 {
                    cmp_utf8_ascii_binary_movbe_4_7(a.as_ptr(), b.as_ptr(), min_len)
                } else {
                    cmp_utf8_ascii_binary_movbe_8plus(a.as_ptr(), b.as_ptr(), min_len)
                }
            };
            return match code {
                -1 => Some(std::cmp::Ordering::Less),
                0 => Some(a.len().cmp(&b.len())),
                1 => Some(std::cmp::Ordering::Greater),
                _ => None,
            };
        }
    }
    let _ = min_len;
    cmp_utf8_ascii_binary_scalar(a, b)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn cmp_nocase_sse2_core(
    ap: *const u8,
    bp: *const u8,
    min_len: usize,
    require_ascii: bool,
) -> isize {
    // Returns:
    //   `>= 0`: byte offset `0..min_len` of first ASCII-nocase mismatch,
    //   `-1`: all `min_len` bytes equal under ASCII-nocase,
    //   `-2`: `require_ascii` was true and a byte `>= 0x80` was encountered.
    debug_assert!(min_len >= 4);
    let k = (&raw const ASCII_NOCASE_CONSTS).cast::<u8>();
    let req = u32::from(require_ascii);
    let out_pos: isize;
    let idx_tmp: usize;
    let lim_tmp: usize;
    let m_tmp: u32;

    // SAFETY: Caller verified `SSE2` and `min_len >= 4` readable bytes at `ap` and `bp`.
    unsafe {
        if min_len < 8 {
            let off = min_len - 4;
            asm!(
                "movdqa {c40}, xmmword ptr [{k}]",
                "movdqa {c5b}, xmmword ptr [{k} + 16]",
                "movdqa {c20}, xmmword ptr [{k} + 32]",

                "movd {va}, dword ptr [{ap}]",
                "movd {vb}, dword ptr [{bp}]",
                "test {req:e}, {req:e}",
                "jz 2f",
                "movdqa {t1}, {va}",
                "por {t1}, {vb}",
                "pmovmskb {m:e}, {t1}",
                "test {m:e}, {m:e}",
                "jnz 6f",
                "2:",
                "movdqa {t1}, {va}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {va}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {va}, {t1}",

                "movdqa {t1}, {vb}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {vb}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {vb}, {t1}",

                "pcmpeqb {va}, {vb}",
                "pmovmskb {m:e}, {va}",
                "xor {m:e}, 0xffff",
                "jnz 4f",

                "movd {va}, dword ptr [{ap} + {off}]",
                "movd {vb}, dword ptr [{bp} + {off}]",
                "test {req:e}, {req:e}",
                "jz 3f",
                "movdqa {t1}, {va}",
                "por {t1}, {vb}",
                "pmovmskb {m:e}, {t1}",
                "test {m:e}, {m:e}",
                "jnz 6f",
                "3:",
                "movdqa {t1}, {va}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {va}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {va}, {t1}",

                "movdqa {t1}, {vb}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {vb}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {vb}, {t1}",

                "pcmpeqb {va}, {vb}",
                "pmovmskb {m:e}, {va}",
                "xor {m:e}, 0xffff",
                "jnz 5f",

                "mov {pos}, -1",
                "jmp 7f",
                "4:",
                "bsf {pos:e}, {m:e}",
                "jmp 7f",
                "5:",
                "bsf {pos:e}, {m:e}",
                "add {pos}, {off}",
                "jmp 7f",
                "6:",
                "mov {pos}, -2",
                "7:",
                ap = in(reg) ap,
                bp = in(reg) bp,
                off = in(reg) off,
                k = in(reg) k,
                req = in(reg) req,
                m = out(reg) m_tmp,
                pos = out(reg) out_pos,
                c40 = out(xmm_reg) _,
                c5b = out(xmm_reg) _,
                c20 = out(xmm_reg) _,
                va = out(xmm_reg) _,
                vb = out(xmm_reg) _,
                t1 = out(xmm_reg) _,
                t2 = out(xmm_reg) _,
                options(nostack, readonly),
            );
            let _ = m_tmp;
            return out_pos;
        }

        if min_len < 16 {
            let off = min_len - 8;
            asm!(
                "movdqa {c40}, xmmword ptr [{k}]",
                "movdqa {c5b}, xmmword ptr [{k} + 16]",
                "movdqa {c20}, xmmword ptr [{k} + 32]",

                "movq {va}, qword ptr [{ap}]",
                "movq {vb}, qword ptr [{bp}]",
                "test {req:e}, {req:e}",
                "jz 2f",
                "movdqa {t1}, {va}",
                "por {t1}, {vb}",
                "pmovmskb {m:e}, {t1}",
                "test {m:e}, {m:e}",
                "jnz 6f",
                "2:",
                "movdqa {t1}, {va}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {va}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {va}, {t1}",

                "movdqa {t1}, {vb}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {vb}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {vb}, {t1}",

                "pcmpeqb {va}, {vb}",
                "pmovmskb {m:e}, {va}",
                "xor {m:e}, 0xffff",
                "jnz 4f",

                "movq {va}, qword ptr [{ap} + {off}]",
                "movq {vb}, qword ptr [{bp} + {off}]",
                "test {req:e}, {req:e}",
                "jz 3f",
                "movdqa {t1}, {va}",
                "por {t1}, {vb}",
                "pmovmskb {m:e}, {t1}",
                "test {m:e}, {m:e}",
                "jnz 6f",
                "3:",
                "movdqa {t1}, {va}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {va}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {va}, {t1}",

                "movdqa {t1}, {vb}",
                "pcmpgtb {t1}, {c40}",
                "movdqa {t2}, {c5b}",
                "pcmpgtb {t2}, {vb}",
                "pand {t1}, {t2}",
                "pand {t1}, {c20}",
                "por {vb}, {t1}",

                "pcmpeqb {va}, {vb}",
                "pmovmskb {m:e}, {va}",
                "xor {m:e}, 0xffff",
                "jnz 5f",

                "mov {pos}, -1",
                "jmp 7f",
                "4:",
                "bsf {pos:e}, {m:e}",
                "jmp 7f",
                "5:",
                "bsf {pos:e}, {m:e}",
                "add {pos}, {off}",
                "jmp 7f",
                "6:",
                "mov {pos}, -2",
                "7:",
                ap = in(reg) ap,
                bp = in(reg) bp,
                off = in(reg) off,
                k = in(reg) k,
                req = in(reg) req,
                m = out(reg) m_tmp,
                pos = out(reg) out_pos,
                c40 = out(xmm_reg) _,
                c5b = out(xmm_reg) _,
                c20 = out(xmm_reg) _,
                va = out(xmm_reg) _,
                vb = out(xmm_reg) _,
                t1 = out(xmm_reg) _,
                t2 = out(xmm_reg) _,
                options(nostack, readonly),
            );
            let _ = m_tmp;
            return out_pos;
        }

        asm!(
            "movdqa {c40}, xmmword ptr [{k}]",
            "movdqa {c5b}, xmmword ptr [{k} + 16]",
            "movdqa {c20}, xmmword ptr [{k} + 32]",
            "xor {idx:e}, {idx:e}",
            "lea {lim}, [{len} - 16]",
            "2:",
            "movdqu {va}, xmmword ptr [{ap} + {idx}]",
            "movdqu {vb}, xmmword ptr [{bp} + {idx}]",
            "test {req:e}, {req:e}",
            "jz 3f",
            "movdqa {t1}, {va}",
            "por {t1}, {vb}",
            "pmovmskb {m:e}, {t1}",
            "test {m:e}, {m:e}",
            "jnz 8f",
            "3:",
            "movdqa {t1}, {va}",
            "pcmpgtb {t1}, {c40}",
            "movdqa {t2}, {c5b}",
            "pcmpgtb {t2}, {va}",
            "pand {t1}, {t2}",
            "pand {t1}, {c20}",
            "por {va}, {t1}",

            "movdqa {t1}, {vb}",
            "pcmpgtb {t1}, {c40}",
            "movdqa {t2}, {c5b}",
            "pcmpgtb {t2}, {vb}",
            "pand {t1}, {t2}",
            "pand {t1}, {c20}",
            "por {vb}, {t1}",

            "pcmpeqb {va}, {vb}",
            "pmovmskb {m:e}, {va}",
            "xor {m:e}, 0xffff",
            "jnz 6f",

            "add {idx}, 16",
            "cmp {idx}, {lim}",
            "jb 2b",

            "cmp {len}, 16",
            "je 5f",
            "movdqu {va}, xmmword ptr [{ap} + {lim}]",
            "movdqu {vb}, xmmword ptr [{bp} + {lim}]",
            "test {req:e}, {req:e}",
            "jz 4f",
            "movdqa {t1}, {va}",
            "por {t1}, {vb}",
            "pmovmskb {m:e}, {t1}",
            "test {m:e}, {m:e}",
            "jnz 8f",
            "4:",
            "movdqa {t1}, {va}",
            "pcmpgtb {t1}, {c40}",
            "movdqa {t2}, {c5b}",
            "pcmpgtb {t2}, {va}",
            "pand {t1}, {t2}",
            "pand {t1}, {c20}",
            "por {va}, {t1}",

            "movdqa {t1}, {vb}",
            "pcmpgtb {t1}, {c40}",
            "movdqa {t2}, {c5b}",
            "pcmpgtb {t2}, {vb}",
            "pand {t1}, {t2}",
            "pand {t1}, {c20}",
            "por {vb}, {t1}",

            "pcmpeqb {va}, {vb}",
            "pmovmskb {m:e}, {va}",
            "xor {m:e}, 0xffff",
            "jnz 7f",

            "5:",
            "mov {pos}, -1",
            "jmp 9f",
            "6:",
            "bsf {pos:e}, {m:e}",
            "add {pos}, {idx}",
            "jmp 9f",
            "7:",
            "bsf {pos:e}, {m:e}",
            "add {pos}, {lim}",
            "jmp 9f",
            "8:",
            "mov {pos}, -2",
            "9:",
            ap = in(reg) ap,
            bp = in(reg) bp,
            len = in(reg) min_len,
            k = in(reg) k,
            req = in(reg) req,
            idx = out(reg) idx_tmp,
            lim = out(reg) lim_tmp,
            m = out(reg) m_tmp,
            pos = out(reg) out_pos,
            c40 = out(xmm_reg) _,
            c5b = out(xmm_reg) _,
            c20 = out(xmm_reg) _,
            va = out(xmm_reg) _,
            vb = out(xmm_reg) _,
            t1 = out(xmm_reg) _,
            t2 = out(xmm_reg) _,
            options(nostack, readonly),
        );
        let _ = (idx_tmp, lim_tmp, m_tmp);
        out_pos
    }
}

/// SQLite `NOCASE` byte-slice comparison accelerated with inline `SSE2` range case-folding.
#[inline(never)]
pub fn cmp_bytes_ascii_nocase(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let min_len = a.len().min(b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if min_len >= 4 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified and `min_len <= a.len(), b.len()`.
            let pos = unsafe { cmp_nocase_sse2_core(a.as_ptr(), b.as_ptr(), min_len, false) };
            return if pos >= 0 {
                let idx = pos as usize;
                a[idx]
                    .to_ascii_lowercase()
                    .cmp(&b[idx].to_ascii_lowercase())
            } else {
                a.len().cmp(&b.len())
            };
        }
    }
    let _ = min_len;
    cmp_bytes_ascii_nocase_scalar(a, b)
}

/// Fused ASCII UTF-8 validation and `NOCASE` comparison on raw byte slices.
#[inline(never)]
pub fn cmp_utf8_ascii_nocase(a: &[u8], b: &[u8]) -> Option<std::cmp::Ordering> {
    let min_len = a.len().min(b.len());
    #[cfg(target_arch = "x86_64")]
    {
        if min_len >= 4 && (cpu_caps() & CAP_SSE2) != 0 {
            // SAFETY: `CAP_SSE2` is verified and `min_len <= a.len(), b.len()`.
            let pos = unsafe { cmp_nocase_sse2_core(a.as_ptr(), b.as_ptr(), min_len, true) };
            return if pos >= 0 {
                let idx = pos as usize;
                Some(
                    a[idx]
                        .to_ascii_lowercase()
                        .cmp(&b[idx].to_ascii_lowercase()),
                )
            } else if pos == -1 {
                Some(a.len().cmp(&b.len()))
            } else {
                None
            };
        }
    }
    let _ = min_len;
    cmp_utf8_ascii_nocase_scalar(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;
    use std::hint::black_box;

    #[derive(Clone, Copy)]
    struct SplitMix64(u64);

    impl SplitMix64 {
        fn new(seed: u64) -> Self {
            Self(seed)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn next_u8(&mut self) -> u8 {
            self.next_u64() as u8
        }
    }

    fn representative_lengths() -> Vec<usize> {
        let mut lens: Vec<usize> = (0..=96).collect();
        for base in [128usize, 192, 256, 384, 512, 1024, 2048, 4096, 8192] {
            for delta in 0..=4 {
                if base >= delta {
                    lens.push(base - delta);
                }
                if base + delta <= 8192 {
                    lens.push(base + delta);
                }
            }
        }
        lens.sort_unstable();
        lens.dedup();
        lens
    }

    #[test]
    fn test_feature_detection_idempotent() {
        let m1 = detected_feature_mask();
        let m2 = detected_feature_mask();
        assert_eq!(m1, m2);
        #[cfg(target_arch = "x86_64")]
        assert_ne!(m1 & CAP_SSE2, 0);
    }

    #[test]
    fn test_asm1_memchr1_memchr2_alignments_lengths_all_bytes() {
        let mut backing = vec![0u8; 8192 + 64];

        // Sweep every length 0..=8192 at unaligned offset 3, plus alignments 0..=35 across representative lengths.
        for len in 0..=8192 {
            let off = len % 36;
            let fill = ((len * 7 + 3) & 0xff) as u8;
            let target = fill.wrapping_add(1);
            let target2 = fill.wrapping_add(2);
            backing[off..off + len].fill(fill);
            assert_eq!(
                memchr1(&backing[off..off + len], target),
                memchr1_scalar(&backing[off..off + len], target),
                "miss len={len}"
            );
            assert_eq!(
                memchr2(&backing[off..off + len], target, target2),
                memchr2_scalar(&backing[off..off + len], target, target2),
                "miss2 len={len}"
            );
            if len > 0 {
                for pos in [0, len / 2, len - 1] {
                    backing[off + pos] = target;
                    assert_eq!(
                        memchr1(&backing[off..off + len], target),
                        Some(pos),
                        "hit1 len={len} pos={pos}"
                    );
                    backing[off + pos] = target2;
                    assert_eq!(
                        memchr2(&backing[off..off + len], target, target2),
                        Some(pos),
                        "hit2 len={len} pos={pos}"
                    );
                    backing[off + pos] = fill;
                }
            }
        }

        // All 256 byte values 0x00..=0xFF and alignments 0..=35.
        for b_val in 0u16..=255 {
            let b = b_val as u8;
            let bg = b.wrapping_add(0x55);
            let b_alt = b.wrapping_add(1);
            for align in 0..=35 {
                let len = 73;
                let s = &mut backing[align..align + len];
                s.fill(bg);
                assert_eq!(memchr1(s, b), None);
                assert_eq!(memchr2(s, b, b_alt), None);
                for pos in [0, 15, 16, 31, 32, 57, 72] {
                    s[pos] = b;
                    assert_eq!(memchr1(s, b), memchr1_scalar(s, b));
                    assert_eq!(memchr2(s, b_alt, b), memchr2_scalar(s, b_alt, b));
                    s[pos] = bg;
                }
            }
        }
    }

    #[test]
    fn test_asm1_eq_exact_and_nocase_alignments_lengths_all_bytes() {
        let mut buf_a = vec![0u8; 8192 + 64];
        let mut buf_b = vec![0u8; 8192 + 64];
        let mut rng = SplitMix64::new(0x1234_5678_9abc_def0);

        for b in &mut buf_a {
            *b = rng.next_u8();
        }
        for len in representative_lengths() {
            for align_a in 0..=35 {
                let align_b = (align_a * 5 + 7) % 36;
                let src = &buf_a[align_a..align_a + len];
                buf_b[align_b..align_b + len].copy_from_slice(src);
                assert!(eq_exact(src, &buf_b[align_b..align_b + len]));

                for idx in 0..len {
                    buf_b[align_b + idx] = src[idx].to_ascii_lowercase();
                }
                assert_eq!(
                    eq_ascii_nocase(src, &buf_b[align_b..align_b + len]),
                    eq_ascii_nocase_scalar(src, &buf_b[align_b..align_b + len]),
                    "eq_nocase match len={len} align_a={align_a}"
                );

                if len > 0 {
                    for pos in [0, len / 3, len.saturating_sub(17), len - 1] {
                        let orig_exact = buf_a[align_a + pos];
                        buf_b[align_b..align_b + len]
                            .copy_from_slice(&buf_a[align_a..align_a + len]);
                        buf_b[align_b + pos] = orig_exact ^ 0x01;
                        assert_eq!(
                            eq_exact(
                                &buf_a[align_a..align_a + len],
                                &buf_b[align_b..align_b + len]
                            ),
                            eq_exact_scalar(
                                &buf_a[align_a..align_a + len],
                                &buf_b[align_b..align_b + len]
                            )
                        );

                        for idx in 0..len {
                            buf_b[align_b + idx] = buf_a[align_a + idx].to_ascii_lowercase();
                        }
                        // Test ASCII case-folding boundary trap (`^ 0x20` on non-letters like `@`/`[`).
                        for cand in [
                            0x00u8, b'@', b'A', b'Z', b'[', b'_', b'`', b'a', b'z', b'{', 0x7f,
                            0x80, 0xff,
                        ] {
                            buf_a[align_a + pos] = cand;
                            for target in [
                                cand.to_ascii_lowercase(),
                                cand ^ 0x20,
                                cand.wrapping_add(1).to_ascii_lowercase(),
                            ] {
                                buf_b[align_b + pos] = target;
                                assert_eq!(
                                    eq_ascii_nocase(
                                        &buf_a[align_a..align_a + len],
                                        &buf_b[align_b..align_b + len]
                                    ),
                                    eq_ascii_nocase_scalar(
                                        &buf_a[align_a..align_a + len],
                                        &buf_b[align_b..align_b + len]
                                    ),
                                    "nocase boundary len={len} pos={pos} cand={cand:#x} target={target:#x}"
                                );
                            }
                        }
                        buf_a[align_a + pos] = orig_exact;
                    }
                }
            }
        }
    }

    #[test]
    fn test_asm1_memmem_exact_and_nocase_alignments_lengths_all_bytes() {
        let mut hay = vec![0u8; 8192 + 64];
        let mut rng = SplitMix64::new(0xfeed_face_cafe_beef);
        for b in &mut hay {
            *b = rng.next_u8();
        }

        // Test all 256 byte values in 1B, 2B, and 5B needles across alignments 0..=35.
        for byte_v in 0u16..=255 {
            let b = byte_v as u8;
            let needle_exact = [b, b.wrapping_add(3), b ^ 0x20];
            let needle_lower: Vec<u8> = needle_exact
                .iter()
                .map(|x| x.to_ascii_lowercase())
                .collect();
            for align in 0..=35 {
                let h = &mut hay[align..align + 69];
                for x in h.iter_mut() {
                    *x = b.wrapping_add(9);
                }
                assert_eq!(
                    memmem_exact(h, &needle_exact),
                    memmem_exact_scalar(h, &needle_exact)
                );
                assert_eq!(
                    memmem_ascii_nocase(h, &needle_lower),
                    memmem_ascii_nocase_scalar(h, &needle_lower)
                );
                for pos in [0, 14, 15, 16, 30, 31, 32, 66] {
                    h[pos..pos + 3].copy_from_slice(&needle_exact);
                    assert!(memmem_exact(h, &needle_exact));
                    assert!(memmem_ascii_nocase(h, &needle_lower));
                    // False positive first+last match with middle mismatch
                    h[pos + 1] = needle_exact[1].wrapping_add(1);
                    assert_eq!(
                        memmem_exact(h, &needle_exact),
                        memmem_exact_scalar(h, &needle_exact)
                    );
                    assert_eq!(
                        memmem_ascii_nocase(h, &needle_lower),
                        memmem_ascii_nocase_scalar(h, &needle_lower)
                    );
                    h[pos..pos + 3].fill(b.wrapping_add(9));
                }
            }
        }

        // Sweep lengths up to 8192 and needle sizes 0..=64.
        for hlen in representative_lengths() {
            for &mlen in &[0usize, 1, 2, 3, 5, 8, 16, 17, 32, 33, 64] {
                let align = hlen % 36;
                let h = &hay[align..align + hlen];
                let needle = &hay[3..3 + mlen];
                let lower_needle: Vec<u8> = needle.iter().map(|x| x.to_ascii_lowercase()).collect();
                assert_eq!(
                    memmem_exact(h, needle),
                    memmem_exact_scalar(h, needle),
                    "memmem_exact random hlen={hlen} mlen={mlen}"
                );
                assert_eq!(
                    memmem_ascii_nocase(h, &lower_needle),
                    memmem_ascii_nocase_scalar(h, &lower_needle),
                    "memmem_nocase random hlen={hlen} mlen={mlen}"
                );
                if hlen >= mlen && mlen > 0 {
                    let mut planted = h.to_vec();
                    let at = hlen - mlen;
                    for k in 0..mlen {
                        planted[at + k] = if k % 2 == 0 {
                            needle[k].to_ascii_uppercase()
                        } else {
                            needle[k]
                        };
                    }
                    assert!(memmem_ascii_nocase(&planted, &lower_needle));
                    planted[at..at + mlen].copy_from_slice(needle);
                    assert!(memmem_exact(&planted, needle));
                }
            }
        }
    }

    #[test]
    fn test_asm2_wal_checksum_alignments_lengths_and_endianness() {
        let mut buf = vec![0u8; 8192 + 64];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((i.wrapping_mul(131).wrapping_add(17)) & 0xff) as u8;
        }
        // Ensure all 256 byte values appear in first 256 bytes.
        for (v, slot) in buf.iter_mut().enumerate().take(256) {
            *slot = v as u8;
        }

        let seeds = [[0u32, 0u32], [0x1234_5678, 0x9abc_def0], [u32::MAX, 1]];
        for &byte_swap in &[false, true] {
            for &init in &seeds {
                // Every 8-byte step from 0..=8192 plus unaligned remainder lengths.
                for step_len in (0..=8192).step_by(8) {
                    let align = (step_len / 8) % 36;
                    let slice = &buf[align..align + step_len];
                    let expected = wal_checksum_extend_scalar(byte_swap, slice, init);
                    assert_eq!(
                        wal_checksum_extend(byte_swap, slice, init),
                        expected,
                        "avx2 wal_cksum swap={byte_swap} len={step_len} align={align}"
                    );
                    assert_eq!(
                        wal_checksum_extend_sse2(byte_swap, slice, init),
                        expected,
                        "sse2 wal_cksum swap={byte_swap} len={step_len} align={align}"
                    );
                }
                // Verify alignments 0..=35 and non-8B trailing bytes (ignored per contract).
                for align in 0..=35 {
                    for len in [
                        0usize, 7, 8, 15, 24, 32, 64, 120, 128, 136, 255, 256, 4096, 4103,
                    ] {
                        let slice = &buf[align..align + len];
                        let expected = wal_checksum_extend_scalar(byte_swap, slice, init);
                        assert_eq!(wal_checksum_extend(byte_swap, slice, init), expected);
                        assert_eq!(wal_checksum_extend_sse2(byte_swap, slice, init), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn test_asm4_rtree_codec_union_choose_subtree_and_filter_ieee754() {
        let mut rng = SplitMix64::new(0x0ddc_0ffe_ebad_f00d);

        // 1. Encode / decode roundtrip across 0..=55 cells and unaligned slices.
        for n in 0..=55usize {
            let mut cells = Vec::with_capacity(n);
            for i in 0..n {
                let x0 = ((rng.next_u64() % 2000) as f32 - 1000.0) * 0.25;
                let dx = (rng.next_u64() % 200) as f32 * 0.25;
                let y0 = ((rng.next_u64() % 2000) as f32 - 1000.0) * 0.25;
                let dy = (rng.next_u64() % 200) as f32 * 0.25;
                cells.push(RTreeCell10::from_2d_f32(
                    (i as i64) * -17 + 5,
                    x0,
                    x0 + dx,
                    y0,
                    y0 + dy,
                ));
            }
            let raw_len = 4 + n * 24 + 16;
            let mut buf_asm = vec![0xAAu8; raw_len + 36];
            let mut buf_scl = vec![0xAAu8; raw_len + 36];
            for align in 0..=35 {
                let sa = &mut buf_asm[align..align + raw_len];
                let ss = &mut buf_scl[align..align + raw_len];
                rtree_encode_2d_cells(n as u16, &cells, sa);
                rtree_encode_2d_cells_scalar(n as u16, &cells, ss);
                assert_eq!(sa, ss, "encode mismatch n={n} align={align}");

                let mut dec_asm = Vec::new();
                let mut dec_scl = Vec::new();
                rtree_decode_2d_cells(sa, n, &mut dec_asm);
                rtree_decode_2d_cells_scalar(ss, n, &mut dec_scl);
                assert_eq!(dec_asm, dec_scl, "decode mismatch n={n} align={align}");
                assert_eq!(dec_asm, cells);
            }
        }

        // 2. IEEE-754 signed zero (`-0.0` vs `+0.0`), `±Inf`, and `NaN` bit-exact union parity.
        let special_vals: [f32; 7] = [
            -0.0f32,
            0.0f32,
            -1.5f32,
            1.5f32,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NAN,
        ];
        for &a0 in &special_vals {
            for &a1 in &special_vals {
                for &b0 in &special_vals {
                    for &b1 in &special_vals {
                        let c_a = RTreeCell10::from_2d_f32(1, a0, a1, b0, b1);
                        let c_b = RTreeCell10::from_2d_f32(2, b0, b1, a0, a1);
                        let pair = [c_a, c_b];
                        let u_asm = rtree_bbox_union_2d_f32(&pair, 99);
                        let u_scl = rtree_bbox_union_2d_f32_scalar(&pair, 99);
                        assert_eq!(
                            u_asm.coords[..4],
                            u_scl.coords[..4],
                            "bbox_union bits mismatch a=({a0:?},{a1:?}) b=({b0:?},{b1:?})"
                        );
                        #[cfg(target_arch = "x86_64")]
                        {
                            // SAFETY: `SSE2` is baseline on `x86_64` and `pair` is non-empty.
                            let u_sse2 = unsafe { rtree_bbox_union_2d_f32_sse2(&pair, 99) };
                            assert_eq!(u_sse2.coords[..4], u_scl.coords[..4]);
                        }

                        let mut acc_asm = c_a;
                        let mut acc_scl = c_a;
                        rtree_cell_union_2d_f32_in_place(&mut acc_asm, &c_b);
                        rtree_cell_union_2d_f32_in_place_scalar(&mut acc_scl, &c_b);
                        assert_eq!(acc_asm.coords[..4], acc_scl.coords[..4]);
                    }
                }
            }
        }

        // 3. `choose_subtree`, `filter_ids`, and `filter_cells` across node sizes 1..=55 (including ties and NaN).
        for n in 1..=55usize {
            let mut cells = Vec::with_capacity(n);
            for i in 0..n {
                // Include duplicate identical boxes to exercise exact tie-breaking.
                let k = i / 2;
                let x0 = (k as f32) * 3.5 - 20.0;
                let y0 = ((k * 3 % 11) as f32) * 2.0 - 10.0;
                cells.push(RTreeCell10::from_2d_f32(
                    (i as i64) + 100,
                    x0,
                    x0 + 10.0,
                    y0,
                    y0 + 8.0,
                ));
            }
            for q in 0..40 {
                let cx = (q as f32) * 2.25 - 25.0;
                let cy = ((q * 5 % 17) as f32) * 1.5 - 12.0;
                let cand = RTreeCell10::from_2d_f32(0, cx, cx + 4.0, cy, cy + 3.0);
                assert_eq!(
                    rtree_choose_subtree_2d_f32(&cells, &cand),
                    rtree_choose_subtree_2d_f32_scalar(&cells, &cand),
                    "choose_subtree n={n} q={q}"
                );

                let mut ids_asm = vec![777i64; n + 2];
                let mut ids_scl = vec![777i64; n + 2];
                let cnt_asm = rtree_filter_2d_f32_ids(
                    &cells,
                    f64::from(cx),
                    f64::from(cx + 9.0),
                    f64::from(cy),
                    f64::from(cy + 7.0),
                    &mut ids_asm,
                );
                let cnt_scl = rtree_filter_2d_f32_ids_scalar(
                    &cells,
                    f64::from(cx),
                    f64::from(cx + 9.0),
                    f64::from(cy),
                    f64::from(cy + 7.0),
                    &mut ids_scl,
                );
                assert_eq!(&ids_asm[..cnt_asm], &ids_scl[..cnt_scl]);

                let mut cls_asm = Vec::new();
                let mut cls_scl = Vec::new();
                rtree_filter_2d_f32_cells(
                    &cells,
                    f64::from(cx),
                    f64::from(cx + 9.0),
                    f64::from(cy),
                    f64::from(cy + 7.0),
                    &mut cls_asm,
                );
                rtree_filter_2d_f32_cells_scalar(
                    &cells,
                    f64::from(cx),
                    f64::from(cx + 9.0),
                    f64::from(cy),
                    f64::from(cy + 7.0),
                    &mut cls_scl,
                );
                assert_eq!(cls_asm, cls_scl);
            }

            // NaN query bounds and NaN cell parity.
            let mut nan_ids_asm = vec![0i64; n];
            let mut nan_ids_scl = vec![0i64; n];
            let nan_cnt_asm =
                rtree_filter_2d_f32_ids(&cells, f64::NAN, 10.0, -10.0, 10.0, &mut nan_ids_asm);
            let nan_cnt_scl = rtree_filter_2d_f32_ids_scalar(
                &cells,
                f64::NAN,
                10.0,
                -10.0,
                10.0,
                &mut nan_ids_scl,
            );
            assert_eq!(nan_cnt_asm, 0);
            assert_eq!(nan_cnt_asm, nan_cnt_scl);

            let mut with_nan = cells.clone();
            with_nan[n / 2] = RTreeCell10::from_2d_f32(999, f32::NAN, 5.0, 0.0, 5.0);
            let cand = RTreeCell10::from_2d_f32(0, 0.0, 1.0, 0.0, 1.0);
            assert_eq!(
                rtree_choose_subtree_2d_f32(&with_nan, &cand),
                rtree_choose_subtree_2d_f32_scalar(&with_nan, &cand)
            );
        }
    }

    #[test]
    fn test_asm6a_cmp_binary_and_nocase_alignments_lengths_all_bytes() {
        let mut buf_a = vec![0u8; 8192 + 64];
        let mut buf_b = vec![0u8; 8192 + 64];
        let mut rng = SplitMix64::new(0x55aa_77cc_33ee_9911);
        for b in &mut buf_a {
            *b = rng.next_u8() & 0x7f; // Pure ASCII first
        }

        // Pure ASCII sweep across alignments 0..=35 and lengths 0..=8192.
        for len in representative_lengths() {
            for align_a in 0..=35 {
                let align_b = (align_a * 3 + 11) % 36;
                let a = &buf_a[align_a..align_a + len];
                buf_b[align_b..align_b + len].copy_from_slice(a);

                assert_eq!(
                    cmp_bytes_binary(a, &buf_b[align_b..align_b + len]),
                    Ordering::Equal
                );
                assert_eq!(
                    cmp_utf8_ascii_binary(a, &buf_b[align_b..align_b + len]),
                    Some(Ordering::Equal)
                );
                assert_eq!(
                    cmp_bytes_ascii_nocase(a, &buf_b[align_b..align_b + len]),
                    Ordering::Equal
                );
                assert_eq!(
                    cmp_utf8_ascii_nocase(a, &buf_b[align_b..align_b + len]),
                    Some(Ordering::Equal)
                );

                // Prefix / unequal length comparisons.
                if len > 0 {
                    let shorter = &buf_b[align_b..align_b + len - 1];
                    assert_eq!(
                        cmp_bytes_binary(a, shorter),
                        cmp_bytes_binary_scalar(a, shorter)
                    );
                    assert_eq!(
                        cmp_utf8_ascii_binary(a, shorter),
                        cmp_utf8_ascii_binary_scalar(a, shorter)
                    );
                    assert_eq!(
                        cmp_bytes_ascii_nocase(a, shorter),
                        cmp_bytes_ascii_nocase_scalar(a, shorter)
                    );
                    assert_eq!(
                        cmp_utf8_ascii_nocase(a, shorter),
                        cmp_utf8_ascii_nocase_scalar(a, shorter)
                    );

                    for pos in [0, len / 2, len.saturating_sub(5), len - 1] {
                        for &cand in &[
                            0x00u8, b'@', b'A', b'Z', b'[', b'_', b'`', b'a', b'z', b'{', 0x7f,
                        ] {
                            buf_b[align_b..align_b + len].copy_from_slice(a);
                            buf_b[align_b + pos] = cand;
                            let b = &buf_b[align_b..align_b + len];
                            assert_eq!(cmp_bytes_binary(a, b), cmp_bytes_binary_scalar(a, b));
                            assert_eq!(
                                cmp_utf8_ascii_binary(a, b),
                                cmp_utf8_ascii_binary_scalar(a, b)
                            );
                            assert_eq!(
                                cmp_bytes_ascii_nocase(a, b),
                                cmp_bytes_ascii_nocase_scalar(a, b)
                            );
                            assert_eq!(
                                cmp_utf8_ascii_nocase(a, b),
                                cmp_utf8_ascii_nocase_scalar(a, b)
                            );
                        }
                    }
                }
            }
        }

        // Full 0x00..=0xFF byte value sweep (including non-ASCII fallback soundness).
        for v1 in 0u16..=255 {
            for v2 in [0u8, b'A', b'a', b'_', 0x7f, 0x80, 0xc3, 0xff] {
                for len in [1usize, 3, 4, 6, 7, 8, 12, 16, 23, 32, 47] {
                    let mut x = vec![b'm'; len];
                    let mut y = vec![b'm'; len];
                    let pos = len - 1;
                    x[pos] = v1 as u8;
                    y[pos] = v2;
                    assert_eq!(cmp_bytes_binary(&x, &y), cmp_bytes_binary_scalar(&x, &y));
                    assert_eq!(
                        cmp_bytes_ascii_nocase(&x, &y),
                        cmp_bytes_ascii_nocase_scalar(&x, &y)
                    );
                    if let Some(ord) = cmp_utf8_ascii_binary(&x, &y) {
                        let sx = String::from_utf8_lossy(&x);
                        let sy = String::from_utf8_lossy(&y);
                        assert_eq!(ord, sx.as_bytes().cmp(sy.as_bytes()));
                    } else {
                        assert!(x.iter().any(|&b| b >= 0x80) || y.iter().any(|&b| b >= 0x80));
                    }
                    if let Some(ord) = cmp_utf8_ascii_nocase(&x, &y) {
                        let sx = String::from_utf8_lossy(&x);
                        let sy = String::from_utf8_lossy(&y);
                        assert_eq!(
                            ord,
                            cmp_bytes_ascii_nocase_scalar(sx.as_bytes(), sy.as_bytes())
                        );
                    } else {
                        assert!(x.iter().any(|&b| b >= 0x80) || y.iter().any(|&b| b >= 0x80));
                    }
                }
            }
        }
    }

    #[test]
    fn test_live_register_clobber_stress_all_kernels() {
        let hay = b"entry-09999-g42-TOKEN63-payload-8765-extra-padding-tail-xyz";
        let pat_low = b"token63";
        let wal_block = [0x5au8; 256];
        let cells = [
            RTreeCell10::from_2d_f32(10, -5.0, 2.0, -3.0, 4.0),
            RTreeCell10::from_2d_f32(20, 1.0, 9.0, 0.0, 8.0),
            RTreeCell10::from_2d_f32(30, -2.0, 6.0, -1.0, 5.0),
        ];
        let cand = RTreeCell10::from_2d_f32(0, 2.0, 3.0, 1.0, 2.0);
        let mut ids = [0i64; 8];

        // Keep 8 GP u64 values and 8 FP f64 values live across interleaved YMM/XMM/GP asm calls.
        let mut g0 = black_box(0x1111_2222_3333_4444u64);
        let mut g1 = black_box(0x5555_6666_7777_8888u64);
        let mut g2 = black_box(0x9999_aaaa_bbbb_ccccu64);
        let mut g3 = black_box(0xdddd_eeee_ffff_0001u64);
        let mut g4 = black_box(0x0123_4567_89ab_cdefu64);
        let mut g5 = black_box(0xfedc_ba98_7654_3210u64);
        let mut g6 = black_box(0x0f0e_0d0c_0b0a_0908u64);
        let mut g7 = black_box(0x8070_6050_4030_2010u64);

        let mut f0 = black_box(1.25f64);
        let mut f1 = black_box(-3.75f64);
        let mut f2 = black_box(7.5f64);
        let mut f3 = black_box(-15.125f64);
        let mut f4 = black_box(31.0625f64);
        let mut f5 = black_box(-63.5f64);
        let mut f6 = black_box(127.25f64);
        let mut f7 = black_box(-255.875f64);

        for iter in 0..200u64 {
            let p1 = memchr1(black_box(hay), b'T').unwrap() as u64;
            let p2 = memchr2(black_box(hay), b'X', b'z').unwrap() as u64;
            let m1 = u64::from(memmem_ascii_nocase(black_box(hay), black_box(pat_low)));
            let m2 = u64::from(eq_ascii_nocase(black_box(&hay[16..23]), black_box(pat_low)));
            let ck = wal_checksum_extend(iter & 1 == 0, black_box(&wal_block), [1, 2]);
            let u = rtree_bbox_union_2d_f32(black_box(&cells), 1);
            let chosen = rtree_choose_subtree_2d_f32(black_box(&cells), black_box(&cand)) as u64;
            let n_ids = rtree_filter_2d_f32_ids(black_box(&cells), 0.0, 5.0, 0.0, 5.0, &mut ids);
            let c1 = cmp_utf8_ascii_binary(black_box(b"C-0042"), black_box(b"C-0199")).unwrap();
            let c2 =
                cmp_utf8_ascii_nocase(black_box(b"NorthWest"), black_box(b"northwest")).unwrap();

            assert_eq!(p1, 16);
            assert_eq!(p2, 58);
            assert_eq!(m1, 1);
            assert_eq!(m2, 1);
            assert_eq!(chosen, 30);
            assert_eq!(n_ids, 3);
            assert_eq!(&ids[..n_ids], &[10, 20, 30]);
            assert_eq!(c1, Ordering::Less);
            assert_eq!(c2, Ordering::Equal);
            assert_eq!(u.id, 1);

            g0 = g0.wrapping_add(p1 ^ u64::from(ck[0]));
            g1 = g1.wrapping_add(p2 ^ u64::from(ck[1]));
            g2 = g2.wrapping_add(m1.wrapping_mul(3));
            g3 = g3.wrapping_add(m2.wrapping_mul(5));
            g4 = g4.wrapping_add(chosen);
            g5 = g5.wrapping_add(n_ids as u64);
            g6 = g6.rotate_left(1) ^ iter;
            g7 = g7.rotate_right(1) ^ (iter.wrapping_mul(7));

            f0 += 0.5;
            f1 -= 0.25;
            f2 *= 1.0;
            f3 += 0.125;
            f4 -= 0.0625;
            f5 += 1.0;
            f6 -= 0.5;
            f7 += 0.25;
        }

        // Verify exact deterministic final states of all live GP and XMM variables.
        let mut eg0 = 0x1111_2222_3333_4444u64;
        let mut eg1 = 0x5555_6666_7777_8888u64;
        let ck_even = wal_checksum_extend_scalar(true, &wal_block, [1, 2]);
        let ck_odd = wal_checksum_extend_scalar(false, &wal_block, [1, 2]);
        for iter in 0..200u64 {
            let ck = if iter & 1 == 0 { ck_even } else { ck_odd };
            eg0 = eg0.wrapping_add(16 ^ u64::from(ck[0]));
            eg1 = eg1.wrapping_add(58 ^ u64::from(ck[1]));
        }
        assert_eq!(g0, eg0);
        assert_eq!(g1, eg1);
        assert_eq!(g2, 0x9999_aaaa_bbbb_ccccu64 + 600);
        assert_eq!(g3, 0xdddd_eeee_ffff_0001u64 + 1000);
        assert_eq!(g4, 0x0123_4567_89ab_cdefu64 + 6000);
        assert_eq!(g5, 0xfedc_ba98_7654_3210u64 + 600);
        assert_ne!(g6, 0);
        assert_ne!(g7, 0);
        assert_eq!(f0, 1.25 + 100.0);
        assert_eq!(f1, -3.75 - 50.0);
        assert_eq!(f2, 7.5);
        assert_eq!(f3, -15.125 + 25.0);
        assert_eq!(f4, 31.0625 - 12.5);
        assert_eq!(f5, -63.5 + 200.0);
        assert_eq!(f6, 127.25 - 100.0);
        assert_eq!(f7, -255.875 + 50.0);
    }
}
