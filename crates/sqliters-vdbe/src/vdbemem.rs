//! `vdbemem.c`: the memory cells (`Mem`, also `sqlite3_value`) of the virtual machine.
//!
//! Port of SQLite 3.54.1, C's names and C's order of operations. A cell owns its text / blob
//! bytes (`z`, with `n == z.len()`), so the storage-class flags `MEM_Dyn` / `MEM_Static` /
//! `MEM_Ephem` / `MEM_Term` have no counterpart; the type flags are C's bits. Text is kept as
//! bytes in the cell's encoding. UTF-16 execution is enabled for the supported LIKE read
//! path; other programs retain their existing routing restrictions.

use std::sync::Arc;

use sqliters_types::fp::{REAL_TEXT_MAX, sqlite_atof_with_encoding, sqlite_atoi64, write_real_text};
use sqliters_types::{RuntimeValue, TextEncoding, Value};

pub const MEM_UNDEFINED: u16 = 0x0000;
pub const MEM_NULL: u16 = 0x0001;
pub const MEM_STR: u16 = 0x0002;
pub const MEM_INT: u16 = 0x0004;
pub const MEM_REAL: u16 = 0x0008;
pub const MEM_BLOB: u16 = 0x0010;
pub const MEM_INT_REAL: u16 = 0x0020;
pub const MEM_AFF_MASK: u16 = 0x003f;
pub const MEM_FROM_BIND: u16 = 0x0040;
pub const MEM_CLEARED: u16 = 0x0100;
pub const MEM_TERM: u16 = 0x0200;
pub const MEM_ZERO: u16 = 0x0400;
pub const MEM_SUBTYPE: u16 = 0x0800;
pub const MEM_TYPE_MASK: u16 = 0x0dbf;
pub const MEM_AGG: u16 = 0x8000;

pub const SQLITE_AFF_NONE: u8 = 0x40;
pub const SQLITE_AFF_BLOB: u8 = 0x41;
pub const SQLITE_AFF_TEXT: u8 = 0x42;
pub const SQLITE_AFF_NUMERIC: u8 = 0x43;
pub const SQLITE_AFF_INTEGER: u8 = 0x44;
pub const SQLITE_AFF_REAL: u8 = 0x45;
pub const SQLITE_AFF_FLEXNUM: u8 = 0x46;
pub const SQLITE_AFF_MASK: u8 = 0x47;

pub const SMALLEST_INT64: i64 = i64::MIN;
pub const LARGEST_INT64: i64 = i64::MAX;

/// A collating function bound to the connection's registry (`CollSeq.xCmp`).
pub type CollCmp = Arc<dyn Fn(&[u8], &[u8]) -> i32 + Send + Sync>;

/// `struct CollSeq` at run time: the name and the comparison function.
#[derive(Clone)]
pub struct CollRt {
    /// CollSeq.enc; NOCASE/RTRIM compare UTF-8 even in a UTF-16 database.
    pub enc: u8,
    pub name: String,
    /// `sqlite3IsBinary(pColl)`: the BINARY collation (`binCollFunc`)
    pub is_binary: bool,
    pub cmp: CollCmp,
    /// The application-defined collation behind `cmp` (`None` for the three built-ins).
    pub custom: Option<sqliters_types::CustomCollation>,
}

impl CollRt {
    /// An application-defined collation (`sqlite3_create_collation`).
    pub fn from_custom(cc: sqliters_types::CustomCollation) -> CollRt {
        let f = std::sync::Arc::clone(&cc.comparator);
        CollRt {
            enc: 1,
            name: cc.name.to_string(),
            is_binary: false,
            cmp: Arc::new(move |a: &[u8], b: &[u8]| {
                let sa = String::from_utf8_lossy(a);
                let sb = String::from_utf8_lossy(b);
                match f(&sa, &sb) {
                    Ok(std::cmp::Ordering::Less) => -1,
                    Ok(std::cmp::Ordering::Greater) => 1,
                    _ => 0,
                }
            }),
            custom: Some(cc),
        }
    }

    /// The collation as the b-tree layer's index insert takes it.
    pub fn key_sort_order(&self, desc: bool, nulls_last: bool) -> sqliters_types::KeySortOrder {
        let collation = if self.custom.is_some() || self.is_binary {
            sqliters_types::Collation::Binary
        } else if self.name.eq_ignore_ascii_case("NOCASE") {
            sqliters_types::Collation::NoCase
        } else {
            sqliters_types::Collation::RTrim
        };
        sqliters_types::KeySortOrder { desc, nulls_last, collation, custom_collation: self.custom.clone() }
    }
}

impl std::fmt::Debug for CollRt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CollRt({})", self.name)
    }
}

/// `binCollFunc`
pub fn bin_coll_func(a: &[u8], b: &[u8]) -> i32 {
    let n = a.len().min(b.len());
    match a[..n].cmp(&b[..n]) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Equal => a.len() as i32 - b.len() as i32,
    }
}

/// `rtrimCollFunc`
pub fn rtrim_coll_func(a: &[u8], b: &[u8]) -> i32 {
    let mut n1 = a.len();
    let mut n2 = b.len();
    while n1 > 0 && a[n1 - 1] == b' ' {
        n1 -= 1;
    }
    while n2 > 0 && b[n2 - 1] == b' ' {
        n2 -= 1;
    }
    bin_coll_func(&a[..n1], &b[..n2])
}

/// `nocaseCollatingFunc` (`sqlite3StrNICmp` over the common length, then the lengths).
pub fn nocase_coll_func(a: &[u8], b: &[u8]) -> i32 {
    let n = a.len().min(b.len());
    for i in 0..n {
        let x = a[i].to_ascii_lowercase();
        let y = b[i].to_ascii_lowercase();
        if x != y {
            return i32::from(x) - i32::from(y);
        }
        if x == 0 { break; }
    }
    a.len() as i32 - b.len() as i32
}

/// The three built-in collations by name (`None`: application-defined).
pub fn builtin_coll(name: &str) -> Option<CollRt> {
    let (is_binary, f): (bool, fn(&[u8], &[u8]) -> i32) = if name.eq_ignore_ascii_case("BINARY") {
        (true, bin_coll_func)
    } else if name.eq_ignore_ascii_case("NOCASE") {
        (false, nocase_coll_func)
    } else if name.eq_ignore_ascii_case("RTRIM") {
        (false, rtrim_coll_func)
    } else {
        return None;
    };
    Some(CollRt { enc: 1, name: name.to_owned(), is_binary, cmp: Arc::new(f), custom: None })
}

/// `struct sqlite3_value` / `Mem`.
#[derive(Clone, Debug, Default)]
pub struct Mem {
    /// `flags`: some combination of `MEM_*`
    pub flags: u16,
    /// `enc`
    pub enc: u8,
    /// `eSubtype`
    pub e_subtype: u8,
    /// `u.i`
    pub i: i64,
    /// `u.r`
    pub r: f64,
    /// `u.nZero`
    pub n_zero: i32,
    /// `z` / `n`
    pub z: Vec<u8>,
}

impl Mem {
    /// A cell holding SQL NULL (`sqlite3VdbeMemInit(p, db, MEM_Null)`).
    #[inline]
    pub fn null() -> Mem {
        Mem { flags: MEM_NULL, enc: 1, ..Mem::default() }
    }

    /// `sqlite3VdbeMemSetNull`
    #[inline]
    pub fn set_null(&mut self) {
        self.flags = MEM_NULL;
        self.z.clear();
    }

    /// `sqlite3VdbeMemSetInt64`
    #[inline]
    pub fn set_int64(&mut self, val: i64) {
        self.i = val;
        self.flags = MEM_INT;
        self.z.clear();
    }

    /// `sqlite3VdbeMemSetDouble`: NaN becomes NULL.
    #[inline]
    pub fn set_double(&mut self, val: f64) {
        self.set_null();
        if !val.is_nan() {
            self.r = val;
            self.flags = MEM_REAL;
        }
    }

    /// `sqlite3VdbeMemSetStr(p, z, n, SQLITE_UTF8, ..)`
    #[inline]
    pub fn set_str(&mut self, z: &[u8]) {
        self.z.clear();
        self.z.extend_from_slice(z);
        self.flags = MEM_STR;
        self.enc = 1;
    }

    /// `sqlite3VdbeMemSetStr(p, z, n, 0, ..)`: a blob.
    #[inline]
    pub fn set_blob(&mut self, z: &[u8]) {
        self.z.clear();
        self.z.extend_from_slice(z);
        self.flags = MEM_BLOB;
    }

    /// `sqlite3VdbeMemSetZeroBlob`
    pub fn set_zero_blob(&mut self, n: i32) {
        self.z.clear();
        self.flags = MEM_BLOB | MEM_ZERO;
        self.n_zero = n.max(0);
        self.enc = 1;
    }

    /// `pMem->n`
    #[inline]
    pub fn n(&self) -> i32 {
        self.z.len() as i32
    }

    /// `sqlite3VdbeMemExpandBlob` (`ExpandBlob(P)`)
    #[inline]
    pub fn expand_blob(&mut self) {
        if self.flags & MEM_ZERO != 0 {
            let n = self.z.len() + self.n_zero.max(0) as usize;
            self.z.resize(n, 0);
            self.flags &= !(MEM_ZERO | MEM_TERM);
        }
    }

    /// `sqlite3VdbeMemTooBig`
    #[inline]
    pub fn too_big(&self, limit_length: i32) -> bool {
        if self.flags & (MEM_STR | MEM_BLOB) != 0 {
            let mut n = self.z.len() as i64;
            if self.flags & MEM_ZERO != 0 {
                n += i64::from(self.n_zero);
            }
            return n > i64::from(limit_length);
        }
        false
    }

    /// `vdbeMemRenderNum` + the tail of `sqlite3VdbeMemStringify(pMem, enc, bForce)`.
    pub fn stringify(&mut self, b_force: bool) {
        if self.flags & (MEM_INT | MEM_INT_REAL) != 0 {
            self.z.clear();
            write_i64(&mut self.z, self.i);
            if self.flags & MEM_INT_REAL != 0 {
                self.z.extend_from_slice(b".0");
            }
        } else {
            let mut buf = [0u8; REAL_TEXT_MAX];
            let n = write_real_text(self.r, &mut buf);
            self.z.clear();
            self.z.extend_from_slice(&buf[..n]);
        }
        self.enc = 1;
        self.flags |= MEM_STR | MEM_TERM;
        if b_force {
            self.flags &= !(MEM_INT | MEM_REAL | MEM_INT_REAL);
        }
    }

    /// sqlite3VdbeChangeEncoding, for a text cell only.
    pub fn change_encoding(&mut self, encoding: u8) {
        if self.flags & MEM_STR != 0 && self.enc != encoding {
            self.z = crate::utf::translate_bytes(&self.z, self.enc, encoding);
            self.enc = encoding;
            self.flags |= MEM_TERM;
        }
    }

    /// `sqlite3VdbeIntValue`
    pub fn int_value(&self) -> i64 {
        let flags = self.flags;
        if flags & (MEM_INT | MEM_INT_REAL) != 0 {
            self.i
        } else if flags & MEM_REAL != 0 {
            sqlite3_real_to_i64(self.r)
        } else if flags & (MEM_STR | MEM_BLOB) != 0 {
            self.integer_value_rc().0
        } else {
            0
        }
    }

    /// `sqlite3Atoi64(..., pMem->enc)`: scan the zero high bytes before parsing
    /// the low-byte prefix, preserving the UTF-16 nonNum return-code decision.
    fn integer_value_rc(&self) -> (i64, i32) {
        if self.enc == 1 {
            return sqlite_atoi64(&self.z);
        }
        let mut ascii = Vec::with_capacity(self.z.len() / 2);
        let mut non_num = false;
        for unit in self.z.chunks_exact(2) {
            let (lo, hi) = if self.enc == 2 { (unit[0], unit[1]) } else { (unit[1], unit[0]) };
            if hi != 0 {
                non_num = true;
                break;
            }
            ascii.push(lo);
        }
        let (value, rc) = sqlite_atoi64(&ascii);
        (value, if non_num && (0..2).contains(&rc) { 1 } else { rc })
    }

    /// `sqlite3MemRealValueRC`: the value and the return code of `sqlite3AtoF`.
    #[inline]
    pub fn real_value_rc(&self) -> (f64, i32) {
        let encoding = match self.enc {
            1 => TextEncoding::Utf8,
            2 => TextEncoding::Utf16Le,
            _ => TextEncoding::Utf16Be,
        };
        sqlite_atof_with_encoding(&self.z, encoding)
    }

    /// `sqlite3VdbeRealValue`
    pub fn real_value(&self) -> f64 {
        if self.flags & MEM_REAL != 0 {
            self.r
        } else if self.flags & (MEM_INT | MEM_INT_REAL) != 0 {
            self.i as f64
        } else if self.flags & (MEM_STR | MEM_BLOB) != 0 {
            self.real_value_rc().0
        } else {
            0.0
        }
    }

    /// `sqlite3VdbeBooleanValue`
    #[inline]
    pub fn boolean_value(&self, if_null: bool) -> bool {
        if self.flags & (MEM_INT | MEM_INT_REAL) != 0 {
            return self.i != 0;
        }
        if self.flags & MEM_NULL != 0 {
            return if_null;
        }
        self.real_value() != 0.0
    }

    /// `MemSetTypeFlag(p, f)`
    #[inline]
    pub fn set_type_flag(&mut self, f: u16) {
        self.flags = (self.flags & !(MEM_TYPE_MASK | MEM_ZERO)) | f;
    }

    /// `sqlite3VdbeIntegerAffinity`
    pub fn integer_affinity(&mut self) {
        if self.flags & MEM_INT_REAL != 0 {
            self.set_type_flag(MEM_INT);
        } else {
            let ix = sqlite3_real_to_i64(self.r);
            // Only mark the value as an integer if (1) the round-trip conversion real->int->real
            // is a no-op, and (2) the integer is not the largest or smallest possible.
            if self.r == ix as f64 && ix > SMALLEST_INT64 && ix < LARGEST_INT64 {
                self.i = ix;
                self.set_type_flag(MEM_INT);
            }
        }
    }

    /// `sqlite3VdbeMemIntegerify`
    pub fn integerify(&mut self) {
        self.i = self.int_value();
        self.set_type_flag(MEM_INT);
    }

    /// `sqlite3VdbeMemRealify`
    pub fn realify(&mut self) {
        self.r = self.real_value();
        self.set_type_flag(MEM_REAL);
    }

    /// `sqlite3VdbeMemNumerify`
    pub fn numerify(&mut self) {
        if self.flags & (MEM_INT | MEM_REAL | MEM_INT_REAL | MEM_NULL) == 0 {
            let (r, rc) = self.real_value_rc();
            self.r = r;
            let (mut ix, a_rc) = self.integer_value_rc();
            if ((rc & 2) == 0 && a_rc < 2) || {
                ix = sqlite3_real_to_i64(self.r);
                sqlite3_real_same_as_int(self.r, ix)
            } {
                self.i = ix;
                self.set_type_flag(MEM_INT);
            } else {
                self.set_type_flag(MEM_REAL);
            }
        }
        self.flags &= !(MEM_STR | MEM_BLOB | MEM_ZERO);
    }

    /// `sqlite3VdbeMemCast(pMem, aff, encoding)`
    pub fn cast(&mut self, aff: u8) {
        self.cast_with_encoding(aff, 1);
    }

    pub fn cast_with_encoding(&mut self, aff: u8, encoding: u8) {
        if self.flags & MEM_NULL != 0 {
            return;
        }
        match aff {
            SQLITE_AFF_BLOB => {
                if self.flags & MEM_BLOB == 0 {
                    self.apply_affinity_with_encoding(SQLITE_AFF_TEXT, encoding);
                    if self.flags & MEM_STR != 0 {
                        self.set_type_flag(MEM_BLOB);
                    }
                } else {
                    self.flags &= !(MEM_TYPE_MASK & !MEM_BLOB);
                }
            }
            SQLITE_AFF_NUMERIC => self.numerify(),
            SQLITE_AFF_INTEGER => self.integerify(),
            SQLITE_AFF_REAL => self.realify(),
            _ => {
                self.flags |= (self.flags & MEM_BLOB) >> 3;
                self.apply_affinity_with_encoding(SQLITE_AFF_TEXT, encoding);
                self.flags &= !(MEM_INT | MEM_REAL | MEM_INT_REAL | MEM_BLOB | MEM_ZERO);
                if encoding != 1 {
                    self.z.truncate(self.z.len() & !1);
                }
                self.change_encoding(encoding);
                self.flags |= MEM_TERM;
            }
        }
    }

    /// `alsoAnInt` (vdbe.c)
    fn also_an_int(&mut self, r_value: f64) -> bool {
        let i_value = sqlite3_real_to_i64(r_value);
        if sqlite3_real_same_as_int(r_value, i_value) {
            self.i = i_value;
            return true;
        }
        let (v, rc) = self.integer_value_rc();
        self.i = v;
        rc == 0
    }

    /// `applyNumericAffinity(pRec, bTryForInt)` (vdbe.c)
    pub fn apply_numeric_affinity(&mut self, b_try_for_int: bool) {
        let (r_value, rc) = self.real_value_rc();
        if rc <= 0 {
            return;
        }
        if (rc & 2) == 0 && self.also_an_int(r_value) {
            self.flags |= MEM_INT;
        } else {
            self.r = r_value;
            self.flags |= MEM_REAL;
            if b_try_for_int {
                self.integer_affinity();
            }
        }
        // TEXT->NUMERIC is many->one.  Hence, it is important to invalidate the string
        // representation after computing a numeric equivalent.
        self.flags &= !MEM_STR;
    }

    /// `applyAffinity(pRec, affinity, enc)` (vdbe.c), also `sqlite3ValueApplyAffinity`.
    pub fn apply_affinity(&mut self, affinity: u8) {
        self.apply_affinity_with_encoding(affinity, 1);
    }

    pub fn apply_affinity_with_encoding(&mut self, affinity: u8, encoding: u8) {
        if affinity >= SQLITE_AFF_NUMERIC {
            if self.flags & MEM_INT == 0 {
                if self.flags & (MEM_REAL | MEM_INT_REAL) == 0 {
                    if self.flags & MEM_STR != 0 {
                        self.apply_numeric_affinity(true);
                    }
                } else if affinity <= SQLITE_AFF_REAL {
                    self.integer_affinity();
                }
            }
        } else if affinity == SQLITE_AFF_TEXT {
            // Only attempt the conversion to TEXT if there is an integer or real
            // representation (blob and NULL do not get converted) but no string
            // representation.
            if self.flags & MEM_STR == 0 && self.flags & (MEM_REAL | MEM_INT | MEM_INT_REAL) != 0 {
                self.stringify(true);
                self.change_encoding(encoding);
            }
            self.flags &= !(MEM_REAL | MEM_INT | MEM_INT_REAL);
        }
    }

    /// `computeNumericType` (vdbe.c)
    fn compute_numeric_type(&mut self) -> u16 {
        if self.flags & MEM_ZERO != 0 {
            self.expand_blob();
        }
        let (r, rc) = self.real_value_rc();
        self.r = r;
        if rc <= 0 {
            let (ix, a_rc) = self.integer_value_rc();
            if (rc & 2) == 0 && a_rc <= 1 {
                self.i = ix;
                MEM_INT
            } else {
                MEM_REAL
            }
        } else {
            if (rc & 2) == 0 {
                let (ix, a_rc) = self.integer_value_rc();
                if a_rc == 0 {
                    self.i = ix;
                    return MEM_INT;
                }
            }
            MEM_REAL
        }
    }

    /// `numericType` (vdbe.c): the numeric type of the value, with `u.i` / `u.r` set for a
    /// string or blob.
    #[inline]
    pub fn numeric_type(&mut self) -> u16 {
        if self.flags & (MEM_INT | MEM_REAL | MEM_INT_REAL | MEM_NULL) != 0 {
            return self.flags & (MEM_INT | MEM_REAL | MEM_INT_REAL | MEM_NULL);
        }
        self.compute_numeric_type()
    }

    /// `sqlite3_value_type`: 1 INTEGER, 2 FLOAT, 3 TEXT, 4 BLOB, 5 NULL.
    #[inline]
    pub fn value_type(&self) -> u8 {
        let f = self.flags;
        if f & MEM_NULL != 0 {
            5
        } else if f & MEM_INT != 0 {
            1
        } else if f & (MEM_REAL | MEM_INT_REAL) != 0 {
            2
        } else if f & MEM_STR != 0 {
            3
        } else {
            4
        }
    }

    /// The value as the engine's `Value` (what `sqlite3_column_value` hands to a caller that
    /// then asks for the column in its own type).
    pub fn to_value(&self) -> Value {
        match self.value_type() {
            5 => Value::Null,
            1 => Value::Integer(self.i),
            2 => {
                if self.flags & MEM_REAL != 0 {
                    Value::Real(self.r)
                } else {
                    Value::Real(self.i as f64)
                }
            }
            3 => Value::Text(match self.enc {
                2 => sqliters_types::TextEncoding::Utf16Le.decode_bytes(&self.z),
                3 => sqliters_types::TextEncoding::Utf16Be.decode_bytes(&self.z),
                _ => match std::str::from_utf8(&self.z) {
                    Ok(s) => s.to_owned(),
                    Err(_) => String::from_utf8_lossy(&self.z).into_owned(),
                },
            }),
            _ => {
                if self.flags & MEM_ZERO != 0 {
                    let mut v = self.z.clone();
                    v.resize(self.z.len() + self.n_zero.max(0) as usize, 0);
                    Value::Blob(v)
                } else {
                    Value::Blob(self.z.clone())
                }
            }
        }
    }

    /// The value as a function argument (`sqlite3_value` with its subtype).
    pub fn to_runtime_value(&self) -> RuntimeValue {
        let v = self.to_value();
        if self.flags & MEM_SUBTYPE != 0 {
            RuntimeValue::with_subtype(v, u32::from(self.e_subtype))
        } else {
            RuntimeValue::plain(v)
        }
    }

    /// `sqlite3_result_*`: stores a function result.
    pub fn set_from_value(&mut self, v: &Value) {
        match v {
            Value::Null => self.set_null(),
            Value::Integer(i) => self.set_int64(*i),
            Value::Real(r) => self.set_double(*r),
            Value::Text(s) => self.set_str(s.as_bytes()),
            Value::Blob(b) => self.set_blob(b),
        }
    }

    /// A cell holding `v`.
    pub fn from_value(v: &Value) -> Mem {
        let mut m = Mem::null();
        m.set_from_value(v);
        m
    }
}

/// `sqlite3Int64ToText`
#[inline]
pub fn write_i64(out: &mut Vec<u8>, v: i64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    let mut x = v.unsigned_abs();
    loop {
        i -= 1;
        buf[i] = b'0' + (x % 10) as u8;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    if v < 0 {
        out.push(b'-');
    }
    out.extend_from_slice(&buf[i..]);
}

/// `sqlite3RealSameAsInt`
#[inline]
pub fn sqlite3_real_same_as_int(r1: f64, i: i64) -> bool {
    let r2 = i as f64;
    r1 == 0.0 || (r1.to_bits() == r2.to_bits() && (-2251799813685248..2251799813685248).contains(&i))
}

/// `sqlite3RealToI64`
#[inline]
pub fn sqlite3_real_to_i64(r: f64) -> i64 {
    if r < -9223372036854774784.0 {
        return SMALLEST_INT64;
    }
    if r > 9223372036854774784.0 {
        return LARGEST_INT64;
    }
    r as i64
}

/// `isAllZero`
#[inline]
fn is_all_zero(z: &[u8]) -> bool {
    z.iter().all(|&b| b == 0)
}

/// `sqlite3BlobCompare`
pub fn sqlite3_blob_compare(b1: &Mem, b2: &Mem) -> i32 {
    let n1 = b1.z.len() as i32;
    let n2 = b2.z.len() as i32;
    if (b1.flags | b2.flags) & MEM_ZERO != 0 {
        if b1.flags & b2.flags & MEM_ZERO != 0 {
            return b1.n_zero - b2.n_zero;
        } else if b1.flags & MEM_ZERO != 0 {
            if !is_all_zero(&b2.z) {
                return -1;
            }
            return b1.n_zero - n2;
        }
        if !is_all_zero(&b1.z) {
            return 1;
        }
        return n1 - b2.n_zero;
    }
    let n = b1.z.len().min(b2.z.len());
    match b1.z[..n].cmp(&b2.z[..n]) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Equal => n1 - n2,
    }
}

/// `sqlite3IntFloatCompare`
#[inline]
pub fn sqlite3_int_float_compare(i: i64, r: f64) -> i32 {
    if r.is_nan() {
        // SQLite considers NaN to be a NULL. And all integer values are greater than NULL.
        return 1;
    }
    if r < -9223372036854775808.0 {
        return 1;
    }
    if r >= 9223372036854775808.0 {
        return -1;
    }
    let y = r as i64;
    if i < y {
        return -1;
    }
    if i > y {
        return 1;
    }
    let s = i as f64;
    if s < r {
        -1
    } else {
        i32::from(s > r)
    }
}

/// vdbeCompareMemStringWithEncodingChange: compare data in CollSeq.enc.
pub fn compare_text_bytes(a: &[u8], a_enc: u8, b: &[u8], b_enc: u8, coll: &CollRt) -> i32 {
    let a = if a_enc == coll.enc { std::borrow::Cow::Borrowed(a) }
        else { std::borrow::Cow::Owned(crate::utf::translate_bytes(a, a_enc, coll.enc)) };
    let b = if b_enc == coll.enc { std::borrow::Cow::Borrowed(b) }
        else { std::borrow::Cow::Owned(crate::utf::translate_bytes(b, b_enc, coll.enc)) };
    (coll.cmp)(&a, &b)
}

/// `sqlite3MemCompare(pMem1, pMem2, pColl)`
pub fn sqlite3_mem_compare(m1: &Mem, m2: &Mem, coll: Option<&CollRt>) -> i32 {
    let f1 = m1.flags;
    let f2 = m2.flags;
    let combined = f1 | f2;
    // If one value is NULL, it is less than the other. If both values are NULL, return 0.
    if combined & MEM_NULL != 0 {
        return i32::from(f2 & MEM_NULL) - i32::from(f1 & MEM_NULL);
    }
    // At least one of the two values is a number
    if combined & (MEM_INT | MEM_REAL | MEM_INT_REAL) != 0 {
        if f1 & f2 & (MEM_INT | MEM_INT_REAL) != 0 {
            return if m1.i < m2.i { -1 } else { i32::from(m1.i > m2.i) };
        }
        if f1 & f2 & MEM_REAL != 0 {
            return if m1.r < m2.r { -1 } else { i32::from(m1.r > m2.r) };
        }
        if f1 & (MEM_INT | MEM_INT_REAL) != 0 {
            return if f2 & MEM_REAL != 0 {
                sqlite3_int_float_compare(m1.i, m2.r)
            } else if f2 & (MEM_INT | MEM_INT_REAL) != 0 {
                if m1.i < m2.i { -1 } else { i32::from(m1.i > m2.i) }
            } else {
                -1
            };
        }
        if f1 & MEM_REAL != 0 {
            return if f2 & (MEM_INT | MEM_INT_REAL) != 0 { -sqlite3_int_float_compare(m2.i, m1.r) } else { -1 };
        }
        return 1;
    }
    // If one value is a string and the other is a blob, the string is less. If both are
    // strings, compare using the collating functions.
    if combined & MEM_STR != 0 {
        if f1 & MEM_STR == 0 {
            return 1;
        }
        if f2 & MEM_STR == 0 {
            return -1;
        }
        if let Some(c) = coll {
            return compare_text_bytes(&m1.z, m1.enc, &m2.z, m2.enc, c);
        }
    }
    sqlite3_blob_compare(m1, m2)
}
