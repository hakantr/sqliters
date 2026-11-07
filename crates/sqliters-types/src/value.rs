//! Fundamental SQLite runtime values, storage classes, type affinities,
//! collating sequences, and comparison rules (`vdbemem.c`, `vdbeaux.c`, `build.c`).

use std::cmp::Ordering;
use std::fmt;

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
            Self::Numeric => match val {
                Value::Text(ref s) => parse_numeric_text(s).unwrap_or(val),
                _ => val,
            },
            Self::Integer => {
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
                    other => other,
                }
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
        match self {
            Self::Binary => a.as_bytes().cmp(b.as_bytes()),
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
    /// Constructs a `Real` value, normalizing IEEE `NaN` to `Value::Null` per SQLite semantics.
    #[inline]
    pub fn real(v: f64) -> Self {
        if v.is_nan() {
            Self::Null
        } else if v == 0.0 {
            // Normalize -0.0 to +0.0 for deterministic serialization, matching SQLite.
            Self::Real(0.0)
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
        match (self, other) {
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Null, _) => Ordering::Less,
            (_, Self::Null) => Ordering::Greater,

            (Self::Integer(a), Self::Integer(b)) => a.cmp(b),
            (Self::Real(a), Self::Real(b)) => a.total_cmp(b),
            (Self::Integer(i), Self::Real(r)) => cmp_int_float(*i, *r),
            (Self::Real(r), Self::Integer(i)) => cmp_int_float(*i, *r).reverse(),

            (Self::Integer(_) | Self::Real(_), Self::Text(_) | Self::Blob(_)) => Ordering::Less,
            (Self::Text(_) | Self::Blob(_), Self::Integer(_) | Self::Real(_)) => Ordering::Greater,

            (Self::Text(a), Self::Text(b)) => collation.compare_str(a, b),
            (Self::Text(_), Self::Blob(_)) => Ordering::Less,
            (Self::Blob(_), Self::Text(_)) => Ordering::Greater,

            (Self::Blob(a), Self::Blob(b)) => a.cmp(b),
        }
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

/// Parses a string into `Value::Integer` or `Value::Real` if the entire trimmed string is numeric.
pub fn parse_numeric_text(s: &str) -> Option<Value> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains('.') && !trimmed.contains('e') && !trimmed.contains('E') {
        if let Ok(i) = trimmed.parse::<i64>() {
            return Some(Value::Integer(i));
        }
    }
    if let Ok(r) = trimmed.parse::<f64>() {
        if r.is_finite() {
            return Some(Value::real(r));
        }
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
    if (i as f64) == r {
        Some(i)
    } else {
        None
    }
}

fn parse_leading_i64(s: &str) -> i64 {
    let trimmed = s.trim_start();
    let mut end = 0;
    let bytes = trimmed.as_bytes();
    if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    trimmed[..end].parse::<i64>().unwrap_or(0)
}

fn parse_leading_f64(s: &str) -> f64 {
    let trimmed = s.trim_start();
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
