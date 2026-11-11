//! Fundamental SQLite runtime values, storage classes, type affinities,
//! collating sequences, and comparison rules (`vdbemem.c`, `vdbeaux.c`, `build.c`).

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use crate::error::{ErrorCode, Result, SqliteError};
use crate::record::KeySortOrder;

/// The 5 fundamental SQLite datatypes (`SQLITE_INTEGER`, `SQLITE_FLOAT`, `SQLITE_TEXT`, `SQLITE_BLOB`, `SQLITE_NULL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StorageClass {
    Integer = 1,
    Float = 2,
    Text = 3,
    Blob = 4,
    Null = 5,
}

impl StorageClass {
    /// Returns the result of SQLite's `typeof(X)` SQL function.
    pub const fn typeof_str(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Integer => "integer",
            Self::Float => "real",
            Self::Text => "text",
            Self::Blob => "blob",
        }
    }
}

/// Column type affinity (`SQLITE_AFF_*` in `sqliteInt.h`), determined per SQLite §3.1 rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TypeAffinity {
    /// `BLOB` / `NONE` affinity — values are stored as-is without coercion.
    #[default]
    Blob,
    /// `TEXT` affinity — numeric values convert to text.
    Text,
    /// `NUMERIC` affinity — well-formed numeric strings convert to integer or real.
    Numeric,
    /// `INTEGER` affinity — like `NUMERIC`, plus exact whole floats convert to integer.
    Integer,
    /// `REAL` affinity — like `NUMERIC`, plus integers convert to IEEE-754 `f64`.
    Real,
}

impl TypeAffinity {
    /// Determines column affinity from a declared column type name (`sqlite3AffinityType` in `build.c`).
    pub fn from_declared_type(decl_type: &str) -> Self {
        let trimmed = decl_type.trim();
        if trimmed.is_empty() {
            return Self::Blob;
        }
        let upper = trimmed.to_ascii_uppercase();
        if upper.contains("INT") {
            Self::Integer
        } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
            Self::Text
        } else if upper.contains("BLOB") {
            Self::Blob
        } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
            Self::Real
        } else {
            Self::Numeric
        }
    }

    /// Applies this affinity to `val` following SQLite's `applyAffinity` rules (`vdbe.c`).
    pub fn apply(self, val: Value) -> Value {
        match self {
            Self::Blob => val,
            Self::Text => match val {
                Value::Null | Value::Blob(_) | Value::Text(_) => val,
                Value::Integer(i) => Value::Text(i.to_string()),
                Value::Real(r) => Value::Text(format_sqlite_real(r)),
            },
            Self::Numeric | Self::Integer => {
                let v = match val {
                    Value::Text(ref s) => parse_numeric_text(s).unwrap_or(val),
                    other => other,
                };
                match v {
                    Value::Real(r) => real_to_exact_int(r).map_or(Value::Real(r), Value::Integer),
                    other => other,
                }
            }
            Self::Real => {
                let v = match val {
                    Value::Text(ref s) => parse_numeric_text(s).unwrap_or(val),
                    other => other,
                };
                match v {
                    Value::Integer(i) => Value::real(i as f64),
                    Value::Real(r) if r.to_bits() == (-0.0_f64).to_bits() => Value::Real(0.0),
                    other => other,
                }
            }
        }
    }
}

/// On-disk database text encoding (`SQLITE_UTF8 = 1`, `SQLITE_UTF16LE = 2`, `SQLITE_UTF16BE = 3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum TextEncoding {
    #[default]
    Utf8 = 1,
    Utf16Le = 2,
    Utf16Be = 3,
}

impl TextEncoding {
    /// Parses the 4-byte integer at offset 56 of the SQLite database header.
    /// `0` (uninitialized header) normalizes to `Utf8`; values outside `0..=3` return `None`.
    #[inline]
    pub const fn from_header_u32(raw: u32) -> Option<Self> {
        match raw {
            0 | 1 => Some(Self::Utf8),
            2 => Some(Self::Utf16Le),
            3 => Some(Self::Utf16Be),
            _ => None,
        }
    }

    /// Parses a `PRAGMA encoding` string (`UTF-8`, `UTF-16`, `UTF-16le`, `UTF-16be`).
    pub fn from_pragma_name(name: &str) -> Option<Self> {
        let trimmed = name.trim().trim_matches(|c| c == '\'' || c == '"');
        if trimmed.eq_ignore_ascii_case("UTF-8") || trimmed.eq_ignore_ascii_case("UTF8") {
            Some(Self::Utf8)
        } else if trimmed.eq_ignore_ascii_case("UTF-16") || trimmed.eq_ignore_ascii_case("UTF16") {
            if cfg!(target_endian = "little") {
                Some(Self::Utf16Le)
            } else {
                Some(Self::Utf16Be)
            }
        } else if trimmed.eq_ignore_ascii_case("UTF-16le")
            || trimmed.eq_ignore_ascii_case("UTF16le")
        {
            Some(Self::Utf16Le)
        } else if trimmed.eq_ignore_ascii_case("UTF-16be")
            || trimmed.eq_ignore_ascii_case("UTF16be")
        {
            Some(Self::Utf16Be)
        } else {
            None
        }
    }

    /// Canonical string returned by `PRAGMA encoding`.
    #[inline]
    pub const fn as_pragma_str(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16le",
            Self::Utf16Be => "UTF-16be",
        }
    }

    /// Byte length of `s` when serialized in this database encoding.
    #[inline]
    pub fn encoded_byte_len(self, s: &str) -> usize {
        match self {
            Self::Utf8 => s.len(),
            Self::Utf16Le | Self::Utf16Be => s.encode_utf16().count() * 2,
        }
    }

    /// Appends `s` encoded in this database encoding into `out`.
    pub fn encode_into(self, s: &str, out: &mut Vec<u8>) {
        match self {
            Self::Utf8 => out.extend_from_slice(s.as_bytes()),
            Self::Utf16Le => {
                for u in s.encode_utf16() {
                    out.extend_from_slice(&u.to_le_bytes());
                }
            }
            Self::Utf16Be => {
                for u in s.encode_utf16() {
                    out.extend_from_slice(&u.to_be_bytes());
                }
            }
        }
    }

    /// Decodes raw record payload bytes into a UTF-8 Rust `String` according to this encoding.
    pub fn decode_bytes(self, buf: &[u8]) -> String {
        match self {
            Self::Utf8 => match std::str::from_utf8(buf) {
                Ok(s) => s.to_owned(),
                Err(_) => String::from_utf8_lossy(buf).into_owned(),
            },
            Self::Utf16Le => {
                let iter = buf
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_le_bytes(c));
                char::decode_utf16(iter)
                    .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect()
            }
            Self::Utf16Be => {
                let iter = buf
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| u16::from_be_bytes(c));
                char::decode_utf16(iter)
                    .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                    .collect()
            }
        }
    }
}

/// Built-in SQLite collating sequences (`BINARY`, `NOCASE`, `RTRIM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Collation {
    #[default]
    Binary,
    NoCase,
    RTrim,
}

impl Collation {
    pub fn from_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("BINARY") {
            Some(Self::Binary)
        } else if name.eq_ignore_ascii_case("NOCASE") {
            Some(Self::NoCase)
        } else if name.eq_ignore_ascii_case("RTRIM") {
            Some(Self::RTrim)
        } else {
            None
        }
    }

    pub fn compare_str(self, a: &str, b: &str) -> Ordering {
        self.compare_str_with_encoding(a, b, TextEncoding::Utf8)
    }

    /// Compares `a` and `b` using SQLite's exact collation rules for the database's [`TextEncoding`].
    /// In SQLite (`main.c`), `BINARY` runs `memcmp` on the database's native text encoding
    /// (`UTF-8`, `UTF-16le`, or `UTF-16be`), while `NOCASE` and `RTRIM` operate on UTF-8.
    pub fn compare_str_with_encoding(self, a: &str, b: &str, enc: TextEncoding) -> Ordering {
        match self {
            Self::Binary => match enc {
                TextEncoding::Utf8 => a.as_bytes().cmp(b.as_bytes()),
                TextEncoding::Utf16Le => {
                    let mut ia = a.encode_utf16();
                    let mut ib = b.encode_utf16();
                    loop {
                        match (ia.next(), ib.next()) {
                            (Some(ua), Some(ub)) => match ua.to_le_bytes().cmp(&ub.to_le_bytes()) {
                                Ordering::Equal => continue,
                                non_eq => return non_eq,
                            },
                            (None, Some(_)) => return Ordering::Less,
                            (Some(_), None) => return Ordering::Greater,
                            (None, None) => return Ordering::Equal,
                        }
                    }
                }
                TextEncoding::Utf16Be => {
                    let mut ia = a.encode_utf16();
                    let mut ib = b.encode_utf16();
                    loop {
                        match (ia.next(), ib.next()) {
                            (Some(ua), Some(ub)) => match ua.cmp(&ub) {
                                Ordering::Equal => continue,
                                non_eq => return non_eq,
                            },
                            (None, Some(_)) => return Ordering::Less,
                            (Some(_), None) => return Ordering::Greater,
                            (None, None) => return Ordering::Equal,
                        }
                    }
                }
            },
            Self::NoCase => {
                for (&ba, &bb) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
                    let la = ba.to_ascii_lowercase();
                    let lb = bb.to_ascii_lowercase();
                    match la.cmp(&lb) {
                        Ordering::Equal => continue,
                        non_eq => return non_eq,
                    }
                }
                a.len().cmp(&b.len())
            }
            Self::RTrim => {
                let ta = a.trim_end_matches(' ');
                let tb = b.trim_end_matches(' ');
                ta.as_bytes().cmp(tb.as_bytes())
            }
        }
    }
}

/// Thread-safe fallible comparator callback for a custom collation sequence.
pub type CustomCollationComparator = Arc<dyn Fn(&str, &str) -> Result<Ordering> + Send + Sync>;

/// Cloneable custom collation descriptor carrying the registered name and fallible comparator.
#[derive(Clone)]
pub struct CustomCollation {
    pub name: Arc<str>,
    pub comparator: CustomCollationComparator,
}

impl CustomCollation {
    pub fn new<F>(name: impl Into<Arc<str>>, comparator: F) -> Self
    where
        F: Fn(&str, &str) -> Result<Ordering> + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            comparator: Arc::new(comparator),
        }
    }

    /// Creates a placeholder custom collation for a schema object loaded before the
    /// custom collation callback is registered on the connection.
    pub fn unbound(name: impl Into<Arc<str>>) -> Self {
        let name_arc: Arc<str> = name.into();
        let err_name = Arc::clone(&name_arc);
        Self {
            name: name_arc,
            comparator: Arc::new(move |_, _| {
                Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such collation sequence: {err_name}"),
                ))
            }),
        }
    }
}

impl fmt::Debug for CustomCollation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomCollation")
            .field("name", &self.name)
            .finish()
    }
}

impl PartialEq for CustomCollation {
    fn eq(&self, other: &Self) -> bool {
        self.name.eq_ignore_ascii_case(&other.name)
            && Arc::ptr_eq(&self.comparator, &other.comparator)
    }
}

impl Eq for CustomCollation {}

/// Dynamic runtime value corresponding to SQLite's `sqlite3_value` / `Mem` cell.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    /// Constructs a `Real` value, normalizing IEEE `NaN` to `Value::Null` per SQLite semantics
    /// while preserving IEEE `-0.0` sign bit (`0x8000_0000_0000_0000`).
    #[inline]
    pub fn real(v: f64) -> Self {
        if v.is_nan() {
            Self::Null
        } else {
            Self::Real(v)
        }
    }

    #[inline]
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    #[inline]
    pub fn blob(b: impl Into<Vec<u8>>) -> Self {
        Self::Blob(b.into())
    }

    #[inline]
    pub const fn storage_class(&self) -> StorageClass {
        match self {
            Self::Null => StorageClass::Null,
            Self::Integer(_) => StorageClass::Integer,
            Self::Real(_) => StorageClass::Float,
            Self::Text(_) => StorageClass::Text,
            Self::Blob(_) => StorageClass::Blob,
        }
    }

    #[inline]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Wraps this value in a plain [`crate::RuntimeValue`] (`subtype == 0`).
    #[inline]
    pub const fn into_runtime(self) -> crate::RuntimeValue {
        crate::RuntimeValue::plain(self)
    }

    /// Wraps this value in a [`crate::RuntimeValue`] with low-8-bit `subtype`.
    #[inline]
    pub const fn with_subtype(self, subtype: u32) -> crate::RuntimeValue {
        crate::RuntimeValue::with_subtype(self, subtype)
    }

    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(v) => Some(*v),
            _ => None,
        }
    }

    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Real(v) => Some(*v),
            Self::Integer(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Blob(b) => Some(b.as_slice()),
            Self::Text(s) => Some(s.as_bytes()),
            _ => None,
        }
    }

    /// Converts value to integer following `sqlite3_value_int64` / `CAST(x AS INTEGER)` rules.
    pub fn to_integer_lossy(&self) -> i64 {
        match self {
            Self::Null => 0,
            Self::Integer(i) => *i,
            Self::Real(r) => {
                if *r < -9223372036854775808.0 {
                    i64::MIN
                } else if *r >= 9223372036854775808.0 {
                    i64::MAX
                } else {
                    *r as i64
                }
            }
            Self::Text(s) => parse_leading_i64(s),
            Self::Blob(b) => parse_leading_i64(&String::from_utf8_lossy(b)),
        }
    }

    /// Converts value to `f64` following `sqlite3_value_double` / `CAST(x AS REAL)` rules.
    pub fn to_real_lossy(&self) -> f64 {
        match self {
            Self::Null => 0.0,
            Self::Integer(i) => *i as f64,
            Self::Real(r) => *r,
            Self::Text(s) => parse_leading_f64(s),
            Self::Blob(b) => parse_leading_f64(&String::from_utf8_lossy(b)),
        }
    }

    /// Converts `self` to an exact `i64` rowid key per C SQLite `OP_MustBeInt` / `OP_SeekRowid`:
    /// - `Integer(i)` -> `Some(i)`
    /// - `Real(r)` -> `real_to_exact_int(r)`
    /// - `Text(s)` -> `parse_numeric_text(s)` converted via `Integer(i)` or `real_to_exact_int(r)`
    /// - `Null` / `Blob` / non-numeric or fractional values -> `None`
    pub fn to_exact_rowid_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(i) => Some(*i),
            Self::Real(r) => real_to_exact_int(*r),
            Self::Text(s) => match parse_numeric_text(s)? {
                Self::Integer(i) => Some(i),
                Self::Real(r) => real_to_exact_int(r),
                _ => None,
            },
            Self::Null | Self::Blob(_) => None,
        }
    }

    /// Converts value to text following `sqlite3_value_text` / string concatenation rules.
    pub fn to_text_lossy(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Integer(i) => i.to_string(),
            Self::Real(r) => format_sqlite_real(*r),
            Self::Text(s) => s.clone(),
            Self::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        }
    }

    /// Evaluates SQLite boolean truthiness (`OP_If` / `WHERE` condition):
    /// `NULL` is `None` (UNKNOWN), non-zero numbers are `Some(true)`, zero / non-numeric text is `Some(false)`.
    pub fn truthiness(&self) -> Option<bool> {
        match self {
            Self::Null => None,
            Self::Integer(i) => Some(*i != 0),
            Self::Real(r) => Some(*r != 0.0),
            Self::Text(s) => {
                if let Some(num) = parse_numeric_text(s) {
                    num.truthiness()
                } else {
                    let f = parse_leading_f64(s);
                    Some(f != 0.0)
                }
            }
            Self::Blob(_) => Some(false),
        }
    }

    /// Compares two values using SQLite's exact B-tree / `ORDER BY` sort rules (`sqlite3MemCompare`).
    ///
    /// Order: `NULL < (Integer / Real) < Text < Blob`.
    pub fn cmp_sqlite(&self, other: &Self, collation: Collation) -> Ordering {
        self.cmp_sqlite_with_encoding(other, collation, TextEncoding::Utf8)
    }

    /// Compares two values using SQLite's exact B-tree / `ORDER BY` sort rules under `enc`.
    pub fn cmp_sqlite_with_encoding(
        &self,
        other: &Self,
        collation: Collation,
        enc: TextEncoding,
    ) -> Ordering {
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Null, _) => Ordering::Less,
            (_, Self::Null) => Ordering::Greater,

            (Self::Integer(a), Self::Integer(b)) => a.cmp(b),
            (Self::Real(a), Self::Real(b)) => {
                if *a == 0.0 && *b == 0.0 {
                    Ordering::Equal
                } else {
                    a.total_cmp(b)
                }
            }
            (Self::Integer(i), Self::Real(r)) => cmp_int_float(*i, *r),
            (Self::Real(r), Self::Integer(i)) => cmp_int_float(*i, *r).reverse(),

            (Self::Integer(_) | Self::Real(_), Self::Text(_) | Self::Blob(_)) => Ordering::Less,
            (Self::Text(_) | Self::Blob(_), Self::Integer(_) | Self::Real(_)) => Ordering::Greater,

            (Self::Text(a), Self::Text(b)) => collation.compare_str_with_encoding(a, b, enc),
            (Self::Text(_), Self::Blob(_)) => Ordering::Less,
            (Self::Blob(_), Self::Text(_)) => Ordering::Greater,

            (Self::Blob(a), Self::Blob(b)) => a.cmp(b),
        }
    }

    /// Compares two values using `spec` (`KeySortOrder`), invoking `spec.custom_collation`
    /// when both operands are `Value::Text` and propagating any custom collation error.
    pub fn cmp_sqlite_fallible(
        &self,
        other: &Self,
        spec: &KeySortOrder,
        enc: TextEncoding,
    ) -> Result<Ordering> {
        if let (Self::Text(a), Self::Text(b)) = (self, other)
            && let Some(ref cc) = spec.custom_collation
        {
            return (cc.comparator)(a, b);
        }
        Ok(self.cmp_sqlite_with_encoding(other, spec.collation, enc))
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_sqlite(other, Collation::Binary) == Ordering::Equal
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp_sqlite(other, Collation::Binary)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "NULL"),
            Self::Integer(i) => write!(f, "{i}"),
            Self::Real(r) => write!(f, "{}", format_sqlite_real(*r)),
            Self::Text(s) => write!(f, "{s}"),
            Self::Blob(b) => {
                write!(f, "X'")?;
                for byte in b {
                    write!(f, "{byte:02X}")?;
                }
                write!(f, "'")
            }
        }
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Self::Integer(i64::from(v))
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::real(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::Text(v.to_owned())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Self::Blob(v)
    }
}

/// Lossless comparison between `i64` and `f64`, ported from `sqlite3IntFloatCompare` in `vdbeaux.c`.
#[inline]
pub fn cmp_int_float(i: i64, r: f64) -> Ordering {
    if r.is_nan() {
        // In SQLite, NaN is NULL which sorts before numbers; if encountered raw, i > NaN.
        return Ordering::Greater;
    }
    // -9223372036854775808.0 (-2^63) and +9223372036854775808.0 (+2^63) are exact in IEEE-754 f64.
    if r < -9_223_372_036_854_775_808.0 {
        return Ordering::Greater;
    }
    if r >= 9_223_372_036_854_775_808.0 {
        return Ordering::Less;
    }
    let y = r as i64;
    match i.cmp(&y) {
        Ordering::Less => Ordering::Less,
        Ordering::Greater => Ordering::Greater,
        Ordering::Equal => {
            // If |r| >= 2^53, r has no fractional bits; otherwise y as f64 is exact.
            let yf = y as f64;
            yf.partial_cmp(&r).unwrap_or(Ordering::Equal)
        }
    }
}

/// Formats a floating-point number similarly to SQLite's `printf("%!.15g")` text rendering:
/// whole numbers still include `.0` (e.g. `1.0`, matching SQLite `CAST(1 AS REAL)` -> `'1.0'`).
pub fn format_sqlite_real(r: f64) -> String {
    if r.is_infinite() {
        return if r.is_sign_positive() {
            "Inf".to_owned()
        } else {
            "-Inf".to_owned()
        };
    }
    if r == 0.0 {
        return "0.0".to_owned();
    }
    let abs = r.abs();
    if !(1e-4..1e15).contains(&abs) {
        let s = format!("{r:e}");
        return s;
    }
    let s = format!("{r}");
    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
        format!("{s}.0")
    } else {
        s
    }
}

/// Returns `true` iff `b` is ASCII whitespace recognized by C SQLite's `sqlite3Isspace`.
#[inline]
pub const fn is_sqlite_ascii_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Trims leading ASCII whitespace (`sqlite3Isspace`: `' '`, `'\t'`, `'\n'`, `'\r'`, `0x0b`, `0x0c`).
#[inline]
pub fn sqlite_trim_ascii_start(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() && is_sqlite_ascii_space(bytes[start]) {
        start += 1;
    }
    &s[start..]
}

/// Trims leading and trailing ASCII whitespace (`sqlite3Isspace`: `' '`, `'\t'`, `'\n'`, `'\r'`, `0x0b`, `0x0c`).
#[inline]
pub fn sqlite_trim_ascii(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() && is_sqlite_ascii_space(bytes[start]) {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && is_sqlite_ascii_space(bytes[end - 1]) {
        end -= 1;
    }
    &s[start..end]
}

fn is_well_formed_sqlite_number(trimmed: &str) -> (bool, bool) {
    let bytes = trimmed.as_bytes();
    if bytes.is_empty() {
        return (false, false);
    }
    let mut i = usize::from(bytes[0] == b'+' || bytes[0] == b'-');
    let mut saw_digit = false;
    let mut is_float_syntax = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        saw_digit = true;
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        is_float_syntax = true;
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            saw_digit = true;
            i += 1;
        }
    }
    if !saw_digit {
        return (false, false);
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        is_float_syntax = true;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return (false, false);
        }
    }
    (i == bytes.len(), is_float_syntax)
}

/// Parses a string into `Value::Integer` or `Value::Real` if the entire ASCII-trimmed string is
/// well-formed SQLite numeric text (`sqlite3AtoF` / `sqlite3Atoi64`).
///
/// Overflowing decimal exponents such as `'1e309'` evaluate to `Some(Value::Real(±f64::INFINITY))`,
/// whereas non-numeric words such as `'inf'` or `'nan'` return `None`.
pub fn parse_numeric_text(s: &str) -> Option<Value> {
    let trimmed = sqlite_trim_ascii(s);
    let (valid, is_float_syntax) = is_well_formed_sqlite_number(trimmed);
    if !valid {
        return None;
    }
    if !is_float_syntax && let Ok(i) = trimmed.parse::<i64>() {
        return Some(Value::Integer(i));
    }
    if let Ok(r) = trimmed.parse::<f64>()
        && !r.is_nan()
    {
        return Some(Value::real(r));
    }
    None
}

/// If `r` is an exact integer within `i64` range (and within `2^53` or exact power representable),
/// converts it to `i64` for `INTEGER` affinity.
fn real_to_exact_int(r: f64) -> Option<i64> {
    if !r.is_finite() || r < -9_223_372_036_854_775_808.0 || r >= 9_223_372_036_854_775_808.0 {
        return None;
    }
    let i = r as i64;
    if (i as f64) == r { Some(i) } else { None }
}

pub fn parse_leading_i64(s: &str) -> i64 {
    let trimmed = sqlite_trim_ascii_start(s);
    let bytes = trimmed.as_bytes();
    let mut idx = 0;
    let mut neg = false;
    if idx < bytes.len() && (bytes[idx] == b'+' || bytes[idx] == b'-') {
        neg = bytes[idx] == b'-';
        idx += 1;
    }
    let digit_start = idx;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == digit_start {
        return 0;
    }
    let mut acc: u64 = 0;
    let limit: u64 = if neg { 1u64 << 63 } else { (1u64 << 63) - 1 };
    for &b in &bytes[digit_start..idx] {
        let d = u64::from(b - b'0');
        acc = match acc.checked_mul(10).and_then(|v| v.checked_add(d)) {
            Some(v) if v <= limit => v,
            _ => return if neg { i64::MIN } else { i64::MAX },
        };
    }
    if neg {
        if acc == (1u64 << 63) {
            i64::MIN
        } else {
            -(acc as i64)
        }
    } else {
        acc as i64
    }
}

pub fn parse_leading_f64(s: &str) -> f64 {
    let trimmed = sqlite_trim_ascii_start(s);
    let bytes = trimmed.as_bytes();
    let mut end = 0;
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    let mut saw_digit = false;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        saw_digit = true;
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'.' {
        end += 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            saw_digit = true;
            end += 1;
        }
    }
    if !saw_digit {
        return 0.0;
    }
    if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
        let exp_start = end;
        end += 1;
        if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
            end += 1;
        }
        let mut exp_digits = false;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            exp_digits = true;
            end += 1;
        }
        if !exp_digits {
            end = exp_start;
        }
    }
    trimmed[..end].parse::<f64>().unwrap_or(0.0)
}
