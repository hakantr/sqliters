//! The run-time half of `vdbeaux.c` that works on records: serial types, `UnpackedRecord`,
//! the record comparators (`sqlite3VdbeRecordCompareWithSkip`, `vdbeRecordCompareInt`,
//! `vdbeRecordCompareString`, `sqlite3VdbeFindCompare`), `sqlite3VdbeIdxRowid` and
//! `sqlite3VdbeIdxKeyCompare`. Port of SQLite 3.54.1; C's names, C's order of operations.
//!
//! (The building half of `vdbeaux.c` — the program builder — is `vdbeaux.rs`.)

use std::borrow::Cow;
use std::sync::Arc;

use sqliters_btree::Btree;
use sqliters_btree::btcursor::{BtCursor, IdxKeyCompare};
use sqliters_types::{Result, SqliteError};

use crate::vdbeaux::{KEYINFO_ORDER_BIGNULL, KEYINFO_ORDER_DESC};
use crate::vdbemem::{
    CollRt, MEM_BLOB, MEM_INT, MEM_INT_REAL, MEM_NULL, MEM_REAL, MEM_STR, MEM_ZERO, Mem, bin_coll_func, sqlite3_int_float_compare,
};

/// `struct KeyInfo` at run time: the collations are bound to their comparison functions.
#[derive(Clone, Debug, Default)]
pub struct KeyInfoRt {
    pub enc: u8,
    /// `nKeyField`
    pub n_key_field: u16,
    /// `nAllField`
    pub n_all_field: u16,
    /// `aSortFlags[]`
    pub a_sort_flags: Vec<u8>,
    /// `aColl[]`: `None` is C's NULL
    pub a_coll: Vec<Option<Arc<CollRt>>>,
}

#[cold]
pub fn corrupt() -> SqliteError {
    SqliteError::corrupt("database disk image is malformed")
}

/// `sqlite3SmallTypeSizes[]` / `sqlite3VdbeSerialTypeLen`
#[inline]
pub fn sqlite3_vdbe_serial_type_len(serial_type: u32) -> u32 {
    const SMALL: [u8; 12] = [0, 1, 2, 3, 4, 6, 8, 8, 0, 0, 0, 0];
    if serial_type >= 12 { (serial_type - 12) / 2 } else { u32::from(SMALL[serial_type as usize]) }
}

/// `getVarint32(&a[i], v)`: `(value, length)`; bytes past the end read as 0.
#[inline]
pub fn get_varint32(a: &[u8], i: usize) -> (u32, usize) {
    let b0 = a.get(i).copied().unwrap_or(0);
    if b0 < 0x80 {
        return (u32::from(b0), 1);
    }
    let mut v: u64 = 0;
    for k in 0..9 {
        let b = a.get(i + k).copied().unwrap_or(0);
        if k == 8 {
            v = (v << 8) | u64::from(b);
            return (if v > 0xffff_ffff { 0xffff_ffff } else { v as u32 }, 9);
        }
        v = (v << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return (if v > 0xffff_ffff { 0xffff_ffff } else { v as u32 }, k + 1);
        }
    }
    (0, 9)
}

/// `sqlite3VarintLen`
#[inline]
pub fn sqlite3_varint_len(mut v: u64) -> usize {
    let mut i = 1;
    loop {
        v >>= 7;
        if v == 0 {
            return i;
        }
        i += 1;
        if i >= 9 {
            return 9;
        }
    }
}

/// `sqlite3PutVarint`
#[inline]
pub fn put_varint(out: &mut Vec<u8>, v: u64) {
    if v <= 0x7f {
        out.push(v as u8);
        return;
    }
    if v & 0xff00_0000_0000_0000 != 0 {
        let mut x = v;
        let mut buf = [0u8; 9];
        buf[8] = x as u8;
        x >>= 8;
        for i in (0..8).rev() {
            buf[i] = ((x & 0x7f) | 0x80) as u8;
            x >>= 7;
        }
        out.extend_from_slice(&buf);
        return;
    }
    let mut buf = [0u8; 10];
    let mut n = 0;
    let mut x = v;
    loop {
        buf[n] = ((x & 0x7f) | 0x80) as u8;
        n += 1;
        x >>= 7;
        if x == 0 {
            break;
        }
    }
    buf[0] &= 0x7f;
    for i in (0..n).rev() {
        out.push(buf[i]);
    }
}

#[inline]
fn byte(a: &[u8], i: usize) -> u8 {
    a.get(i).copied().unwrap_or(0)
}

/// `vdbeRecordDecodeInt`
#[inline]
fn vdbe_record_decode_int(serial_type: u32, a: &[u8], d: usize) -> i64 {
    match serial_type {
        0 | 1 => i64::from(byte(a, d) as i8),
        2 => i64::from(i16::from_be_bytes([byte(a, d), byte(a, d + 1)])),
        3 => i64::from(i32::from_be_bytes([byte(a, d), byte(a, d + 1), byte(a, d + 2), 0]) >> 8),
        4 => i64::from(i32::from_be_bytes([byte(a, d), byte(a, d + 1), byte(a, d + 2), byte(a, d + 3)])),
        5 => {
            let hi = i64::from(i16::from_be_bytes([byte(a, d), byte(a, d + 1)]));
            let lo = i64::from(u32::from_be_bytes([byte(a, d + 2), byte(a, d + 3), byte(a, d + 4), byte(a, d + 5)]));
            lo + (hi << 32)
        }
        6 => get8(a, d) as i64,
        _ => i64::from(serial_type) - 8,
    }
}

#[inline]
fn get8(a: &[u8], d: usize) -> u64 {
    if d + 8 <= a.len() {
        u64::from_be_bytes([a[d], a[d + 1], a[d + 2], a[d + 3], a[d + 4], a[d + 5], a[d + 6], a[d + 7]])
    } else {
        let mut x = 0u64;
        for k in 0..8 {
            x = (x << 8) | u64::from(byte(a, d + k));
        }
        x
    }
}

/// `sqlite3VdbeSerialGet(buf, serial_type, pMem)`: `buf` starts at the value.
pub fn sqlite3_vdbe_serial_get(buf: &[u8], serial_type: u32, mem: &mut Mem) {
    match serial_type {
        10 => {
            mem.flags = MEM_NULL | MEM_ZERO;
            mem.z.clear();
            mem.n_zero = 0;
        }
        11 | 0 => {
            mem.flags = MEM_NULL;
        }
        1..=6 => {
            mem.i = vdbe_record_decode_int(serial_type, buf, 0);
            mem.flags = MEM_INT;
        }
        7 => {
            let r = f64::from_bits(get8(buf, 0));
            mem.r = r;
            mem.flags = if r.is_nan() { MEM_NULL } else { MEM_REAL };
        }
        8 | 9 => {
            mem.i = i64::from(serial_type) - 8;
            mem.flags = MEM_INT;
        }
        _ => {
            let n = ((serial_type - 12) / 2) as usize;
            mem.z.clear();
            mem.z.extend_from_slice(&buf[..n.min(buf.len())]);
            mem.flags = if serial_type & 1 != 0 { MEM_STR } else { MEM_BLOB };
        }
    }
}

/// How `sqlite3VdbeFindCompare` chose to compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordCompare {
    /// `sqlite3VdbeRecordCompare`
    General,
    /// `vdbeRecordCompareInt`
    Int,
    /// `vdbeRecordCompareString`
    String,
}

/// `struct UnpackedRecord`.
pub struct UnpackedRecord<'a> {
    /// `pKeyInfo`
    pub key_info: &'a KeyInfoRt,
    /// `aMem` (`nField` values)
    pub a_mem: &'a [Mem],
    /// `nField`
    pub n_field: u16,
    /// `default_rc`
    pub default_rc: i8,
    /// `errCode`
    pub err_code: u8,
    /// `r1`, `r2`
    pub r1: i8,
    pub r2: i8,
    /// `eqSeen`
    pub eq_seen: bool,
    /// `u.i`
    u_i: i64,
    /// The comparator `sqlite3VdbeFindCompare` selected.
    pub x_compare: RecordCompare,
}

impl<'a> UnpackedRecord<'a> {
    pub fn new(key_info: &'a KeyInfoRt, a_mem: &'a [Mem], n_field: u16, default_rc: i8) -> Self {
        UnpackedRecord {
            key_info,
            a_mem,
            n_field,
            default_rc,
            err_code: 0,
            r1: 0,
            r2: 0,
            eq_seen: false,
            u_i: 0,
            x_compare: RecordCompare::General,
        }
    }

    /// `sqlite3VdbeFindCompare`
    pub fn find_compare(&mut self) {
        self.x_compare = RecordCompare::General;
        if self.key_info.n_all_field <= 13 {
            let flags = self.a_mem[0].flags;
            let sf = self.key_info.a_sort_flags.first().copied().unwrap_or(0);
            if sf != 0 {
                if sf & KEYINFO_ORDER_BIGNULL != 0 {
                    return;
                }
                self.r1 = 1;
                self.r2 = -1;
            } else {
                self.r1 = -1;
                self.r2 = 1;
            }
            if flags & MEM_INT != 0 {
                self.u_i = self.a_mem[0].i;
                self.x_compare = RecordCompare::Int;
                return;
            }
            if flags & (MEM_REAL | MEM_INT_REAL | MEM_NULL | MEM_BLOB) == 0
                && self.a_mem[0].enc == self.key_info.enc
                && self.key_info.a_coll.first().is_none_or(|c| c.as_ref().is_none_or(|c| c.is_binary))
            {
                self.x_compare = RecordCompare::String;
            }
        }
    }

    /// `xRecordCompare(nKey1, pKey1, pPKey2)` with the selected comparator.
    #[inline]
    pub fn x_record_compare(&mut self, key1: &[u8]) -> i32 {
        match self.x_compare {
            RecordCompare::General => sqlite3_vdbe_record_compare_with_skip(key1, self, false),
            RecordCompare::Int => vdbe_record_compare_int(key1, self),
            RecordCompare::String => vdbe_record_compare_string(key1, self),
        }
    }
}

impl IdxKeyCompare for UnpackedRecord<'_> {
    #[inline]
    fn compare(&mut self, cell_key: &[u8]) -> i32 {
        self.x_record_compare(cell_key)
    }
    #[inline]
    fn compare_general(&mut self, cell_key: &[u8]) -> i32 {
        sqlite3_vdbe_record_compare_with_skip(cell_key, self, false)
    }
    #[inline]
    fn has_error(&self) -> bool {
        self.err_code != 0
    }
    #[inline]
    fn clear_error(&mut self) {
        self.err_code = 0;
    }
}

/// `sqlite3VdbeRecordUnpack(nKey, pKey, p)` for a record allocated by
/// `sqlite3VdbeAllocUnpackedRecord` (`nField = nKeyField + 1`): the values and `p->nField`.
pub fn sqlite3_vdbe_record_unpack(key_info: &KeyInfoRt, key: &[u8], max_field: usize) -> Vec<Mem> {
    let n_key = key.len() as u32;
    let (sz_hdr, n) = get_varint32(key, 0);
    let mut idx = n as u32;
    let mut d = sz_hdr;
    let mut out: Vec<Mem> = Vec::with_capacity(max_field.min(16));
    while idx < sz_hdr && d <= n_key {
        let (serial_type, n) = get_varint32(key, idx as usize);
        idx += n as u32;
        let mut m = Mem { enc: key_info.enc, ..Mem::default() };
        let start = (d as usize).min(key.len());
        sqlite3_vdbe_serial_get(&key[start..], serial_type, &mut m);
        d += sqlite3_vdbe_serial_type_len(serial_type);
        out.push(m);
        if out.len() >= max_field {
            break;
        }
    }
    if d > n_key && !out.is_empty() {
        // In a corrupt record entry, the last pMem might have been set up using
        // uninitialized memory. Overwrite its value with NULL, to prevent warnings.
        let last = out.len() - 1;
        out[last].set_null();
    }
    out
}

/// `sqlite3VdbeRecordCompareWithSkip(nKey1, pKey1, pPKey2, bSkip)`
pub fn sqlite3_vdbe_record_compare_with_skip(a_key1: &[u8], p: &mut UnpackedRecord<'_>, b_skip: bool) -> i32 {
    let n_key1 = a_key1.len() as u32;
    let mut d1: u32;
    let mut i: usize;
    let sz_hdr1: u32;
    let mut idx1: u32;
    let mut rc: i32 = 0;
    let key_info = p.key_info;

    // If bSkip is true, then the caller has already determined that the first two elements
    // in the keys are equal. Fix the various stack variables so that this routine begins
    // comparing at the second field.
    if b_skip {
        let mut s1 = u32::from(byte(a_key1, 1));
        if s1 < 0x80 {
            idx1 = 2;
        } else {
            let (v, n) = get_varint32(a_key1, 1);
            s1 = v;
            idx1 = 1 + n as u32;
        }
        sz_hdr1 = u32::from(byte(a_key1, 0));
        d1 = sz_hdr1 + sqlite3_vdbe_serial_type_len(s1);
        i = 1;
    } else {
        let b0 = u32::from(byte(a_key1, 0));
        if b0 < 0x80 {
            sz_hdr1 = b0;
            idx1 = 1;
        } else {
            let (v, n) = get_varint32(a_key1, 0);
            sz_hdr1 = v;
            idx1 = n as u32;
        }
        d1 = sz_hdr1;
        i = 0;
    }
    if d1 > n_key1 {
        p.err_code = 11;
        return 0; // Corruption
    }

    loop {
        let rhs = &p.a_mem[i];
        let serial_type: u32;
        let d = d1 as usize;
        if rhs.flags & (MEM_INT | MEM_INT_REAL) != 0 {
            // RHS is an integer
            serial_type = u32::from(byte(a_key1, idx1 as usize));
            if serial_type >= 10 {
                rc = if serial_type == 10 { -1 } else { 1 };
            } else if serial_type == 0 {
                rc = -1;
            } else if serial_type == 7 {
                let r = f64::from_bits(get8(a_key1, d));
                rc = -sqlite3_int_float_compare(rhs.i, r);
            } else {
                let lhs = vdbe_record_decode_int(serial_type, a_key1, d);
                let r = rhs.i;
                if lhs < r {
                    rc = -1;
                } else if lhs > r {
                    rc = 1;
                }
            }
        } else if rhs.flags & MEM_REAL != 0 {
            // RHS is real
            serial_type = u32::from(byte(a_key1, idx1 as usize));
            if serial_type >= 10 {
                // Serial types 12 or greater are strings and blobs (greater than numbers).
                // Types 10 and 11 are currently "reserved for future use", so it doesn't
                // really matter what the results of comparing them to numeric values are.
                rc = if serial_type == 10 { -1 } else { 1 };
            } else if serial_type == 0 {
                rc = -1;
            } else if serial_type == 7 {
                let r = f64::from_bits(get8(a_key1, d));
                if r.is_nan() {
                    rc = -1; // mem1 is a NaN
                } else if r < rhs.r {
                    rc = -1;
                } else if r > rhs.r {
                    rc = 1;
                }
            } else {
                let lhs = vdbe_record_decode_int(serial_type, a_key1, d);
                rc = sqlite3_int_float_compare(lhs, rhs.r);
            }
        } else if rhs.flags & MEM_STR != 0 {
            // RHS is a string
            serial_type = get_varint32(a_key1, idx1 as usize).0;
            if serial_type < 12 {
                rc = -1;
            } else if serial_type & 0x01 == 0 {
                rc = 1;
            } else {
                let n = (serial_type - 12) / 2;
                if d1 + n > n_key1 || usize::from(key_info.n_all_field) <= i {
                    p.err_code = 11;
                    return 0; // Corruption
                } else if let Some(coll) = key_info.a_coll.get(i).and_then(|c| c.as_ref()) {
                    rc = crate::vdbemem::compare_text_bytes(&a_key1[d..d + n as usize], key_info.enc, &rhs.z, rhs.enc, coll);
                } else {
                    let probe = if rhs.enc == key_info.enc {
                        Cow::Borrowed(rhs.z.as_slice())
                    } else {
                        Cow::Owned(crate::utf::translate_bytes(&rhs.z, rhs.enc, key_info.enc))
                    };
                    rc = bin_coll_func(&a_key1[d..d + n as usize], &probe);
                }
            }
        } else if rhs.flags & MEM_BLOB != 0 {
            // RHS is a blob
            serial_type = get_varint32(a_key1, idx1 as usize).0;
            if serial_type < 12 || serial_type & 0x01 != 0 {
                rc = -1;
            } else {
                let n_str = (serial_type - 12) / 2;
                if d1 + n_str > n_key1 {
                    p.err_code = 11;
                    return 0; // Corruption
                } else if rhs.flags & MEM_ZERO != 0 {
                    if !a_key1[d..d + n_str as usize].iter().all(|&b| b == 0) {
                        rc = 1;
                    } else {
                        rc = n_str as i32 - rhs.n_zero;
                    }
                } else {
                    let n_cmp = (n_str as usize).min(rhs.z.len());
                    rc = match a_key1[d..d + n_cmp].cmp(&rhs.z[..n_cmp]) {
                        std::cmp::Ordering::Less => -1,
                        std::cmp::Ordering::Greater => 1,
                        std::cmp::Ordering::Equal => n_str as i32 - rhs.z.len() as i32,
                    };
                }
            }
        } else {
            // RHS is null
            serial_type = u32::from(byte(a_key1, idx1 as usize));
            if serial_type == 0 || serial_type == 10 || (serial_type == 7 && f64::from_bits(get8(a_key1, d)).is_nan()) {
                // rc = 0
            } else {
                rc = 1;
            }
        }

        if rc != 0 {
            let sort_flags = key_info.a_sort_flags.get(i).copied().unwrap_or(0);
            if sort_flags != 0
                && (sort_flags & KEYINFO_ORDER_BIGNULL == 0
                    || ((sort_flags & KEYINFO_ORDER_DESC != 0) != (serial_type == 0 || rhs.flags & MEM_NULL != 0)))
            {
                rc = -rc;
            }
            return rc;
        }

        i += 1;
        if i == usize::from(p.n_field) {
            break;
        }
        d1 += sqlite3_vdbe_serial_type_len(serial_type);
        if d1 > n_key1 {
            break;
        }
        idx1 += sqlite3_varint_len(u64::from(serial_type)) as u32;
        if idx1 >= sz_hdr1 {
            p.err_code = 11;
            return 0; // Corrupt index
        }
    }

    // rc==0 here means that one or both of the keys ran out of fields and all the fields up
    // to that point were equal. Return the default_rc value.
    p.eq_seen = true;
    i32::from(p.default_rc)
}

/// `vdbeRecordCompareInt`
pub fn vdbe_record_compare_int(key1: &[u8], p: &mut UnpackedRecord<'_>) -> i32 {
    let a_key = usize::from(byte(key1, 0) & 0x3f);
    let serial_type = u32::from(byte(key1, 1));
    let lhs: i64 = match serial_type {
        1..=6 => vdbe_record_decode_int(serial_type, key1, a_key),
        8 | 9 => i64::from(serial_type) - 8,
        // This case could be removed without changing the results of running this code.
        // Including it causes gcc to generate a faster switch statement.
        _ => return sqlite3_vdbe_record_compare_with_skip(key1, p, false),
    };
    let v = p.u_i;
    if v > lhs {
        i32::from(p.r1)
    } else if v < lhs {
        i32::from(p.r2)
    } else if p.n_field > 1 {
        // The first fields of the two keys are equal. Compare the trailing fields.
        sqlite3_vdbe_record_compare_with_skip(key1, p, true)
    } else {
        // The first fields of the two keys are equal and there are no trailing fields.
        // Return pPKey2->default_rc in this case.
        p.eq_seen = true;
        i32::from(p.default_rc)
    }
}

/// `vdbeRecordCompareString`
pub fn vdbe_record_compare_string(a_key1: &[u8], p: &mut UnpackedRecord<'_>) -> i32 {
    let n_key1 = a_key1.len() as i32;
    let mut serial_type = i32::from(byte(a_key1, 1) as i8);
    loop {
        if serial_type < 12 {
            if serial_type < 0 {
                serial_type = get_varint32(a_key1, 1).0 as i32;
                if serial_type >= 12 {
                    continue;
                }
            }
            return i32::from(p.r1); // (pKey1/nKey1) is a number or a null
        }
        break;
    }
    if serial_type & 0x01 == 0 {
        return i32::from(p.r2); // (pKey1/nKey1) is a blob
    }
    let sz_hdr = i32::from(byte(a_key1, 0));
    let n_str = (serial_type - 12) / 2;
    if sz_hdr + n_str > n_key1 {
        p.err_code = 11;
        return 0; // Corruption
    }
    let z = &p.a_mem[0].z;
    let pn = z.len() as i32;
    let n_cmp = pn.min(n_str) as usize;
    let s = sz_hdr as usize;
    match a_key1[s..s + n_cmp].cmp(&z[..n_cmp]) {
        std::cmp::Ordering::Greater => i32::from(p.r2),
        std::cmp::Ordering::Less => i32::from(p.r1),
        std::cmp::Ordering::Equal => {
            let res = n_str - pn;
            if res == 0 {
                if p.n_field > 1 {
                    sqlite3_vdbe_record_compare_with_skip(a_key1, p, true)
                } else {
                    p.eq_seen = true;
                    i32::from(p.default_rc)
                }
            } else if res > 0 {
                i32::from(p.r2)
            } else {
                i32::from(p.r1)
            }
        }
    }
}

/// `sqlite3VdbeMemFromBtree(pCur, offset, amt, pMem)`: `amt` bytes of the payload of the
/// entry the cursor is on.
pub fn sqlite3_vdbe_mem_from_btree(bt: &mut Btree, cur: &mut BtCursor, offset: u32, amt: u32) -> Result<Vec<u8>> {
    if u64::from(amt) + u64::from(offset) > bt.bc_max_record_size() as u64 {
        return Err(corrupt());
    }
    let mut buf = vec![0u8; amt as usize];
    bt.bc_payload(cur, offset, &mut buf)?;
    Ok(buf)
}

/// `sqlite3VdbeMemFromBtreeZeroOffset(pCur, amt, pMem)` (`getCellInfo` has run): the first
/// `amt` bytes of the payload, read from the page when they are all there.
pub fn sqlite3_vdbe_mem_from_btree_zero_offset(
    bt: &mut Btree,
    cur: &mut BtCursor,
    amt: u32,
    out: &mut Vec<u8>,
) -> Result<()> {
    out.clear();
    {
        let local = bt.bc_payload_fetch(cur)?;
        if amt as usize <= local.len() {
            out.extend_from_slice(&local[..amt as usize]);
            return Ok(());
        }
    }
    if u64::from(amt) > bt.bc_max_record_size() as u64 {
        return Err(corrupt());
    }
    out.resize(amt as usize, 0);
    bt.bc_payload(cur, 0, out)
}

/// `sqlite3VdbeIdxRowid(db, pCur, &rowid)`
pub fn sqlite3_vdbe_idx_rowid(bt: &mut Btree, cur: &mut BtCursor, scratch: &mut Vec<u8>) -> Result<i64> {
    // Get the size of the index entry. Only indices entries of less than 2GiB are supported.
    let n_cell_key = bt.bc_payload_size(cur)?;
    sqlite3_vdbe_mem_from_btree_zero_offset(bt, cur, n_cell_key, scratch)?;
    let m = &scratch[..];
    let (sz_hdr, _) = get_varint32(m, 0);
    if sz_hdr < 3 || sz_hdr as usize > m.len() {
        return Err(corrupt());
    }
    // The last field of the index should be an integer - the ROWID. Verify that the last
    // entry really is an integer.
    let (type_rowid, _) = get_varint32(m, sz_hdr as usize - 1);
    if !(1..=9).contains(&type_rowid) || type_rowid == 7 {
        return Err(corrupt());
    }
    let len_rowid = sqlite3_vdbe_serial_type_len(type_rowid) as usize;
    if m.len() < sz_hdr as usize + len_rowid {
        return Err(corrupt());
    }
    Ok(vdbe_record_decode_int(type_rowid, m, m.len() - len_rowid))
}

/// `sqlite3VdbeIdxKeyCompare(db, pC, pUnpacked, &res)`
pub fn sqlite3_vdbe_idx_key_compare(
    bt: &mut Btree,
    cur: &mut BtCursor,
    unpacked: &mut UnpackedRecord<'_>,
    scratch: &mut Vec<u8>,
) -> Result<i32> {
    let n_cell_key = bt.bc_payload_size(cur)?;
    // nCellKey will always be between 0 and 0xffffffff because of the way that
    // btreeParseCellPtr() and sqlite3GetVarint32() are implemented.
    if n_cell_key == 0 || n_cell_key > 0x7fff_ffff {
        return Err(corrupt());
    }
    sqlite3_vdbe_mem_from_btree_zero_offset(bt, cur, n_cell_key, scratch)?;
    Ok(sqlite3_vdbe_record_compare_with_skip(scratch, unpacked, false))
}
