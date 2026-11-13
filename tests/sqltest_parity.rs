//! Integration verification for `sqltest` engine parity (`printf`, `unhex`, math functions,
//! configurable `SQLITE_LIMIT_COLUMN` / `SQLITE_MAX_COLUMN` ceilings, and all 168 jobs in the
//! `sqltest/correctness.py` catalog).
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters::{Connection, ErrorCode, SqliteError, StepResult, Value};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("sqliters-{prefix}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp sandbox");
        Self { dir }
    }

    fn path(&self, leaf: &str) -> PathBuf {
        self.dir.join(leaf)
    }
}

impl Drop for TempSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------
// Minimal safe recursive-descent JSON reader for frozen `sqltest` manifests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

impl Json {
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Num(n) => Some(*n as i64),
            _ => None,
        }
    }

    fn as_usize(&self) -> Option<usize> {
        self.as_i64().map(|v| v as usize)
    }

    fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Self::Arr(a) => Some(a),
            _ => None,
        }
    }

    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Obj(m) => m.get(key),
            _ => None,
        }
    }
}

struct JsonParser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn parse(text: &'a str) -> Json {
        let mut p = Self {
            src: text.as_bytes(),
            pos: 0,
        };
        let v = p.parse_value();
        p.skip_ws();
        assert_eq!(p.pos, p.src.len(), "trailing bytes in JSON");
        v
    }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn parse_value(&mut self) -> Json {
        self.skip_ws();
        match self.src[self.pos] {
            b'n' => {
                assert!(self.src[self.pos..].starts_with(b"null"));
                self.pos += 4;
                Json::Null
            }
            b't' => {
                assert!(self.src[self.pos..].starts_with(b"true"));
                self.pos += 4;
                Json::Bool(true)
            }
            b'f' => {
                assert!(self.src[self.pos..].starts_with(b"false"));
                self.pos += 5;
                Json::Bool(false)
            }
            b'"' => Json::Str(self.parse_string()),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            _ => self.parse_number(),
        }
    }

    fn parse_string(&mut self) -> String {
        assert_eq!(self.src[self.pos], b'"');
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.src.len() {
            let b = self.src[self.pos];
            self.pos += 1;
            if b == b'"' {
                return out;
            }
            if b == b'\\' {
                let esc = self.src[self.pos];
                self.pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\x08'),
                    b'f' => out.push('\x0c'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = std::str::from_utf8(&self.src[self.pos..self.pos + 4]).unwrap();
                        self.pos += 4;
                        let code = u32::from_str_radix(hex, 16).unwrap();
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                    other => panic!("unexpected JSON escape: {other}"),
                }
            } else {
                // Reconstruct UTF-8 char starting at `self.pos - 1`.
                let start = self.pos - 1;
                let rest = std::str::from_utf8(&self.src[start..]).expect("valid UTF-8 JSON");
                let ch = rest.chars().next().unwrap();
                self.pos = start + ch.len_utf8();
                out.push(ch);
            }
        }
        panic!("unterminated JSON string");
    }

    fn parse_array(&mut self) -> Json {
        assert_eq!(self.src[self.pos], b'[');
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.src[self.pos] == b']' {
            self.pos += 1;
            return Json::Arr(items);
        }
        loop {
            items.push(self.parse_value());
            self.skip_ws();
            match self.src[self.pos] {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Json::Arr(items);
                }
                other => panic!("unexpected char in JSON array: {}", other as char),
            }
        }
    }

    fn parse_object(&mut self) -> Json {
        assert_eq!(self.src[self.pos], b'{');
        self.pos += 1;
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.src[self.pos] == b'}' {
            self.pos += 1;
            return Json::Obj(map);
        }
        loop {
            self.skip_ws();
            let key = self.parse_string();
            self.skip_ws();
            assert_eq!(self.src[self.pos], b':');
            self.pos += 1;
            let val = self.parse_value();
            map.insert(key, val);
            self.skip_ws();
            match self.src[self.pos] {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Json::Obj(map);
                }
                other => panic!("unexpected char in JSON object: {}", other as char),
            }
        }
    }

    fn parse_number(&mut self) -> Json {
        let start = self.pos;
        while self.pos < self.src.len()
            && matches!(
                self.src[self.pos],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).unwrap();
        Json::Num(s.parse::<f64>().unwrap())
    }
}

// ---------------------------------------------------------------------------
// Wire encoding & CSV helper matching `sqltest/native.py` and `freeze_inputs.py`
// ---------------------------------------------------------------------------

fn encode_hex_lower(bytes: &[u8], dst: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &b in bytes {
        dst.push(HEX[(b >> 4) as usize]);
        dst.push(HEX[(b & 0x0f) as usize]);
    }
}

fn decode_hex_bytes(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0);
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16).unwrap() as u8;
        let lo = (bytes[i + 1] as char).to_digit(16).unwrap() as u8;
        out.push((hi << 4) | lo);
        i += 2;
    }
    out
}

fn append_wire_row(values: &[Value], prefix_arity: bool, dst: &mut Vec<u8>) {
    if prefix_arity {
        dst.extend_from_slice(values.len().to_string().as_bytes());
        dst.push(b'\t');
    }
    for (idx, val) in values.iter().enumerate() {
        if idx > 0 {
            dst.push(b'\t');
        }
        match val {
            Value::Null => dst.push(b'N'),
            Value::Integer(n) => {
                dst.extend_from_slice(b"I:");
                dst.extend_from_slice(n.to_string().as_bytes());
            }
            Value::Real(r) => {
                dst.extend_from_slice(b"R:");
                encode_hex_lower(&r.to_be_bytes(), dst);
            }
            Value::Text(s) => {
                dst.extend_from_slice(b"T:");
                encode_hex_lower(s.as_bytes(), dst);
            }
            Value::Blob(b) => {
                dst.extend_from_slice(b"B:");
                encode_hex_lower(b, dst);
            }
        }
    }
    dst.push(b'\n');
}

fn token_to_value(tok: &str) -> Value {
    if tok == "N" {
        return Value::Null;
    }
    let (tag, payload) = tok.split_once(':').expect("valid frozen token");
    match tag {
        "I" => Value::Integer(payload.parse::<i64>().unwrap()),
        "R" => {
            let raw = decode_hex_bytes(payload);
            let arr: [u8; 8] = raw.try_into().unwrap();
            Value::Real(f64::from_be_bytes(arr))
        }
        "T" => Value::Text(String::from_utf8(decode_hex_bytes(payload)).unwrap()),
        "B" => Value::Blob(decode_hex_bytes(payload)),
        other => panic!("unknown frozen token tag: {other}"),
    }
}

#[derive(Debug)]
struct StagedError {
    stage: &'static str,
    err: SqliteError,
}

/// Executes a multi-statement SQL script identically to `sqltest/native.py:Database.execute`,
/// tagging errors with `"prepare"` vs `"step"` and recording rows emitted prior to any error.
fn execute_script_wire(
    conn: &Connection,
    mut sql: &str,
    prefix_arity: bool,
    out_wire: &mut Vec<u8>,
) -> Result<(usize, usize), StagedError> {
    let mut rows = 0usize;
    let mut fields = 0usize;

    while !sql.is_empty() {
        let mut stmt = conn.prepare(sql).map_err(|err| StagedError {
            stage: "prepare",
            err,
        })?;
        let tail_len = stmt.tail().len();
        let cur_len = sql.len();
        loop {
            match stmt.step() {
                Ok(StepResult::Row(r)) => {
                    rows += 1;
                    fields += r.len();
                    append_wire_row(r.values(), prefix_arity, out_wire);
                }
                Ok(StepResult::Done) => break,
                Err(err) => {
                    let _ = stmt.finalize();
                    return Err(StagedError { stage: "step", err });
                }
            }
        }
        stmt.finalize().map_err(|err| StagedError {
            stage: "finalize",
            err,
        })?;
        if tail_len == cur_len {
            break;
        }
        sql = &sql[(cur_len - tail_len)..];
    }

    Ok((rows, fields))
}

fn exec_script_discard(conn: &Connection, sql: &str) -> Result<(), StagedError> {
    let mut sink = Vec::new();
    let _ = execute_script_wire(conn, sql, false, &mut sink)?;
    Ok(())
}

fn open_configured_db(path: &Path, work_dir: &Path, max_column: usize) -> Connection {
    fs::create_dir_all(work_dir.join("artifacts")).unwrap();
    let conn = Connection::open(path).expect("open candidate db");
    conn.set_working_dir(Some(work_dir.to_path_buf())).unwrap();
    assert_eq!(conn.set_max_column_ceiling(max_column), max_column);
    conn
}

// ---------------------------------------------------------------------------
// Standalone engine regression tests (`printf`, `unhex`, math, column limits)
// ---------------------------------------------------------------------------

#[test]
fn test_printf_altform2_and_float_specifiers() {
    let conn = Connection::open_in_memory().unwrap();
    let row = conn
        .query_row(
            "SELECT
                printf('%!.26g', 0.0),
                printf('%!.26g', 42.0),
                printf('%!.26g', -101255.625),
                printf('%!.26g', 0.125),
                printf('%!.26g', 1e25),
                printf('%.4g', 12345.6),
                printf('%#.4g', 12.0),
                printf('%+08.2f', 3.5),
                printf('% 8.2f', 3.5),
                printf('%,d', -1234567890);",
            &[],
        )
        .unwrap()
        .unwrap();

    assert_eq!(row.get_str(0), Some("0.0"));
    assert_eq!(row.get_str(1), Some("42.0"));
    assert_eq!(row.get_str(2), Some("-101255.625"));
    assert_eq!(row.get_str(3), Some("0.125"));
    let parsed_e25: f64 = row.get_str(4).unwrap().parse().unwrap();
    assert_eq!(parsed_e25, 1e25_f64);
    assert_eq!(row.get_str(5), Some("1.235e+04"));
    assert_eq!(row.get_str(6), Some("12.00"));
    assert_eq!(row.get_str(7), Some("+0003.50"));
    assert_eq!(row.get_str(8), Some("    3.50"));
    assert_eq!(row.get_str(9), Some("-1,234,567,890"));
}

#[test]
fn test_unhex_1_and_2_arg() {
    let conn = Connection::open_in_memory().unwrap();
    let row = conn
        .query_row(
            "SELECT
                unhex(''),
                unhex('4142ff'),
                unhex('414'),
                unhex('41-42:ff', '-: '),
                unhex('4-142', '-'),
                unhex(NULL),
                unhex('4142', NULL),
                unhex(X'4142'),
                unhex(X'475a');",
            &[],
        )
        .unwrap()
        .unwrap();

    assert_eq!(row.get_blob(0), Some(&b""[..]));
    assert_eq!(row.get_blob(1), Some(&[0x41, 0x42, 0xff][..]));
    assert_eq!(row.get(2), Some(&Value::Null));
    assert_eq!(row.get_blob(3), Some(&[0x41, 0x42, 0xff][..]));
    // Ignored characters are only allowed BETWEEN complete 2-nibble pairs (`4-1` is rejected).
    assert_eq!(row.get(4), Some(&Value::Null));
    assert_eq!(row.get(5), Some(&Value::Null));
    assert_eq!(row.get(6), Some(&Value::Null));
    // `X'4142'` coerces to ASCII `"AB"` via `sqlite3_value_text` -> `0xAB`, whereas `"GZ"` (`X'475a'`) -> `NULL`.
    assert_eq!(row.get_blob(7), Some(&[0xab][..]));
    assert_eq!(row.get(8), Some(&Value::Null));
}

#[test]
fn test_builtin_math_functions_and_function_list() {
    let conn = Connection::open_in_memory().unwrap();
    let row = conn
        .query_row(
            "SELECT
                sqrt(16),
                pow(2, 8),
                power(3, 3),
                log2(8),
                log(100),
                log10(1000),
                log(2, 32),
                ceil(3.2),
                ceiling(-3.2),
                floor(3.8),
                trunc(-3.8),
                ceil(7),
                mod(7, 3),
                round(degrees(radians(180.0)), 6),
                sqrt(-1),
                sqrt('not_a_number');",
            &[],
        )
        .unwrap()
        .unwrap();

    assert_eq!(row.get_f64(0), Some(4.0));
    assert_eq!(row.get_f64(1), Some(256.0));
    assert_eq!(row.get_f64(2), Some(27.0));
    assert_eq!(row.get_f64(3), Some(3.0));
    assert_eq!(row.get_f64(4), Some(2.0));
    assert_eq!(row.get_f64(5), Some(3.0));
    assert_eq!(row.get_f64(6), Some(5.0));
    assert_eq!(row.get(7), Some(&Value::Real(4.0)));
    assert_eq!(row.get(8), Some(&Value::Real(-3.0)));
    assert_eq!(row.get(9), Some(&Value::Real(3.0)));
    assert_eq!(row.get(10), Some(&Value::Real(-3.0)));
    assert_eq!(row.get(11), Some(&Value::Integer(7)));
    assert_eq!(row.get_f64(12), Some(1.0));
    assert_eq!(row.get_f64(13), Some(180.0));
    assert_eq!(row.get(14), Some(&Value::Null));
    assert_eq!(row.get(15), Some(&Value::Null));

    let fn_cnt = conn
        .query_row(
            "SELECT count(DISTINCT name) FROM pragma_function_list WHERE name IN ('sqrt','pow','log2','atan2','pi');",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(fn_cnt.get_i64(0), Some(5));
}

#[test]
fn test_column_limit_prepare_boundaries() {
    for ceiling in [128usize, 2000, 4096] {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(conn.set_max_column_ceiling(ceiling), ceiling);
        assert_eq!(conn.set_column_limit((ceiling + 500) as isize), ceiling);
        assert_eq!(conn.set_column_limit(-1), ceiling);

        // Narrow further to 4 columns to test every prepare-time boundary fast & deterministically,
        // and also verify exact 4096-column CREATE TABLE acceptance when ceiling == 4096.
        if ceiling == 4096 {
            let cols_4096 = (1..=4096)
                .map(|i| format!("c{i} INT"))
                .collect::<Vec<_>>()
                .join(",");
            conn.execute(&format!("CREATE TABLE wide_ok({cols_4096});"))
                .unwrap();
            let cols_4097 = (1..=4097)
                .map(|i| format!("c{i} INT"))
                .collect::<Vec<_>>()
                .join(",");
            let err = conn
                .prepare(&format!("CREATE TABLE wide_bad({cols_4097});"))
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::Error);
            assert!(err.message.contains("too many columns on wide_bad"));
        }

        conn.set_column_limit(4);
        conn.execute("CREATE TABLE t(c1 INT, c2 INT, c3 INT, c4 INT);")
            .unwrap();

        let e_alter = conn
            .prepare("ALTER TABLE t ADD COLUMN c5 INT;")
            .unwrap_err();
        assert_eq!(e_alter.code, ErrorCode::Error);
        assert!(
            e_alter
                .message
                .contains("too many columns on sqlite_altertab_t")
        );

        let e_idx = conn
            .prepare("CREATE INDEX idx_bad ON t(c1, c2, c3, c4, c1);")
            .unwrap_err();
        assert_eq!(e_idx.code, ErrorCode::Error);
        assert!(e_idx.message.contains("too many columns in index"));

        let e_set = conn
            .prepare("UPDATE t SET c1=1, c2=2, c3=3, c4=4, c1=5;")
            .unwrap_err();
        assert_eq!(e_set.code, ErrorCode::Error);
        assert!(e_set.message.contains("too many columns in set list"));

        let e_grp = conn
            .prepare("SELECT c1 FROM t GROUP BY c1, c2, c3, c4, c1;")
            .unwrap_err();
        assert_eq!(e_grp.code, ErrorCode::Error);
        assert!(e_grp.message.contains("too many terms in GROUP BY clause"));

        let e_ord = conn
            .prepare("SELECT c1 FROM t ORDER BY c1, c2, c3, c4, c1;")
            .unwrap_err();
        assert_eq!(e_ord.code, ErrorCode::Error);
        assert!(e_ord.message.contains("too many terms in ORDER BY clause"));
    }
}

#[test]
fn test_cast_int64_overflow_and_numeric_affinity() {
    let conn = Connection::open_in_memory().unwrap();
    let row = conn
        .query_row(
            "SELECT
                -9223372036854775808,
                9223372036854775807,
                typeof(9223372036854775807 + 1),
                CAST('9223372036854775808' AS INTEGER),
                CAST('-9223372036854775809' AS INTEGER),
                CAST('99999999999999999999999999' AS INTEGER),
                CAST('-99999999999999999999999999' AS INTEGER),
                typeof(CAST('1.0' AS NUMERIC)),
                CAST('1.0' AS NUMERIC),
                typeof(CAST(1.0 AS NUMERIC)),
                typeof(CAST('1e2' AS NUMERIC)),
                typeof(CAST('9223372036854775808' AS NUMERIC)),
                CAST(X'312e30' AS NUMERIC);",
            &[],
        )
        .unwrap()
        .unwrap();

    assert_eq!(row.get_i64(0), Some(i64::MIN));
    assert_eq!(row.get_i64(1), Some(i64::MAX));
    assert_eq!(row.get_str(2), Some("real"));
    assert_eq!(row.get_i64(3), Some(i64::MAX));
    assert_eq!(row.get_i64(4), Some(i64::MIN));
    assert_eq!(row.get_i64(5), Some(i64::MAX));
    assert_eq!(row.get_i64(6), Some(i64::MIN));
    assert_eq!(row.get_str(7), Some("integer"));
    assert_eq!(row.get_i64(8), Some(1));
    assert_eq!(row.get_str(9), Some("real"));
    assert_eq!(row.get_str(10), Some("integer"));
    assert_eq!(row.get_str(11), Some("real"));
    assert_eq!(row.get(12), Some(&Value::Integer(1)));
}

#[test]
fn test_negative_zero_real_preservation_and_equality() {
    let conn = Connection::open_in_memory().unwrap();
    let row = conn
        .query_row(
            "SELECT
                -0.0,
                typeof(-0.0),
                1e999,
                -1e999,
                0.0 / 0.0,
                CAST(0.125 AS TEXT),
                CAST(-0.0 AS TEXT),
                printf('%g', -0.0),
                -0.0 = 0.0,
                -0.0 < 0.0,
                -0.0 > 0.0,
                -0.0 IS 0.0;",
            &[],
        )
        .unwrap()
        .unwrap();

    match row.get(0) {
        Some(Value::Real(r)) => assert_eq!(r.to_bits(), (-0.0_f64).to_bits()),
        other => panic!("expected Value::Real(-0.0), got {other:?}"),
    }
    assert_eq!(row.get_str(1), Some("real"));
    assert_eq!(row.get_f64(2), Some(f64::INFINITY));
    assert_eq!(row.get_f64(3), Some(f64::NEG_INFINITY));
    assert_eq!(row.get(4), Some(&Value::Null));
    assert_eq!(row.get_str(5), Some("0.125"));
    assert_eq!(row.get_str(6), Some("0.0"));
    assert_eq!(row.get_str(7), Some("0"));
    assert_eq!(row.get_i64(8), Some(1));
    assert_eq!(row.get_i64(9), Some(0));
    assert_eq!(row.get_i64(10), Some(0));
    assert_eq!(row.get_i64(11), Some(1));

    conn.execute("CREATE TABLE t_blob(x); INSERT INTO t_blob VALUES (-0.0);")
        .unwrap();
    let b_row = conn
        .query_row("SELECT x FROM t_blob;", &[])
        .unwrap()
        .unwrap();
    match b_row.get(0) {
        Some(Value::Real(r)) => assert_eq!(r.to_bits(), (-0.0_f64).to_bits()),
        other => panic!("expected BLOB-affinity column to preserve -0.0, got {other:?}"),
    }

    conn.execute("CREATE TABLE t_real(x REAL); INSERT INTO t_real VALUES (-0.0);")
        .unwrap();
    let r_row = conn
        .query_row("SELECT x FROM t_real;", &[])
        .unwrap()
        .unwrap();
    match r_row.get(0) {
        Some(Value::Real(r)) => assert_eq!(r.to_bits(), 0.0_f64.to_bits()),
        other => panic!("expected REAL-affinity column to normalize -0.0 -> +0.0, got {other:?}"),
    }
}

#[test]
fn test_datetime_month_year_and_anchor_modifiers() {
    let conn = Connection::open_in_memory().unwrap();
    let row = conn
        .query_row(
            "SELECT
                date('2000-02-28', '+1 day'),
                strftime('%Y-%m-%d', '2000-01-01', '+1 month'),
                unixepoch('1970-01-01 00:00:00'),
                date('2000-01-31', '+1 month'),
                date('2000-03-31', '-1 month'),
                date('2000-01-01', '+1.5 months'),
                date('2000-02-29', '+1 year'),
                datetime('2024-05-15 14:30:45', 'start of month'),
                datetime('2024-05-15 14:30:45', 'start of year'),
                datetime('2024-05-15 14:30:45', 'start of day'),
                date('2024-05-15', 'weekday 0'),
                date('2024-05-15', 'weekday 3');",
            &[],
        )
        .unwrap()
        .unwrap();

    assert_eq!(row.get_str(0), Some("2000-02-29"));
    assert_eq!(row.get_str(1), Some("2000-02-01"));
    assert_eq!(row.get_i64(2), Some(0));
    assert_eq!(row.get_str(3), Some("2000-03-02"));
    assert_eq!(row.get_str(4), Some("2000-03-02"));
    assert_eq!(row.get_str(5), Some("2000-02-16"));
    assert_eq!(row.get_str(6), Some("2001-03-01"));
    assert_eq!(row.get_str(7), Some("2024-05-01 00:00:00"));
    assert_eq!(row.get_str(8), Some("2024-01-01 00:00:00"));
    assert_eq!(row.get_str(9), Some("2024-05-15 00:00:00"));
    assert_eq!(row.get_str(10), Some("2024-05-19"));
    assert_eq!(row.get_str(11), Some("2024-05-15"));
}

// ---------------------------------------------------------------------------
// Full 168-job `sqltest/correctness.py` verification against C baseline
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum CorrectnessJobKind {
    Wide { maximum: usize },
    Append,
    UuidFull { seed: i64 },
    Core { spec: Json },
    Boundary { maximum: usize, spec: Json },
    Additional { maximum: usize, spec: Json },
}

#[derive(Debug)]
struct CorrectnessJob {
    id: String,
    kind: CorrectnessJobKind,
}

fn build_correctness_plan(manifest: &Json) -> Vec<CorrectnessJob> {
    let cfg = manifest.get("config").unwrap();
    let mut jobs = Vec::new();

    for w in cfg.get("wide_columns").unwrap().as_arr().unwrap() {
        let maximum = w.as_usize().unwrap();
        jobs.push(CorrectnessJob {
            id: format!("wide-{maximum}"),
            kind: CorrectnessJobKind::Wide { maximum },
        });
    }

    jobs.push(CorrectnessJob {
        id: "append-all-seeds".to_owned(),
        kind: CorrectnessJobKind::Append,
    });

    for s in cfg.get("seeds").unwrap().as_arr().unwrap() {
        let seed = s.as_i64().unwrap();
        jobs.push(CorrectnessJob {
            id: format!("uuid-{seed}"),
            kind: CorrectnessJobKind::UuidFull { seed },
        });
    }

    for c in manifest.get("catalog_scenarios").unwrap().as_arr().unwrap() {
        jobs.push(CorrectnessJob {
            id: c.get("id").unwrap().as_str().unwrap().to_owned(),
            kind: CorrectnessJobKind::Core { spec: c.clone() },
        });
    }

    for c in manifest.get("catalog").unwrap().as_arr().unwrap() {
        if c.get("kind").and_then(Json::as_str) == Some("boundary_sql") {
            let max_col = c.get("future_max_column").unwrap().as_usize().unwrap();
            let src = c.get("source_path").unwrap().as_str().unwrap();
            let stem = Path::new(src).file_stem().unwrap().to_str().unwrap();
            jobs.push(CorrectnessJob {
                id: format!("max{max_col}-{stem}"),
                kind: CorrectnessJobKind::Boundary {
                    maximum: max_col,
                    spec: c.clone(),
                },
            });
        }
    }

    for c in manifest
        .get("catalog_additional_cases")
        .unwrap()
        .as_arr()
        .unwrap()
    {
        let fam = c.get("family").unwrap().as_str().unwrap();
        let id = c.get("id").unwrap().as_str().unwrap();
        if matches!(
            fam,
            "optional" | "conflict" | "ddl_fk" | "generated" | "storage" | "stress"
        ) && !id.starts_with("wide-")
        {
            let max_col = c
                .get("future_max_column")
                .and_then(Json::as_usize)
                .unwrap_or(2000);
            jobs.push(CorrectnessJob {
                id: id.to_owned(),
                kind: CorrectnessJobKind::Additional {
                    maximum: max_col,
                    spec: c.clone(),
                },
            });
        }
    }

    jobs
}

fn verify_integrity_ok(conn: &Connection, job_id: &str) {
    let res = conn.query("PRAGMA integrity_check;", &[]).unwrap();
    assert_eq!(
        res.rows.len(),
        1,
        "job {job_id}: integrity_check row count != 1"
    );
    assert_eq!(
        res.rows[0].get_str(0),
        Some("ok"),
        "job {job_id}: integrity_check failed: {:?}",
        res.rows
    );
}

fn run_sqltest_correctness_subset<F>(mut include_idx: F)
where
    F: FnMut(usize) -> bool,
{
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sqltest_root = repo_root.join("sqltest");
    let manifest_path = sqltest_root.join("external-input-manifest.json");
    let baseline_status_path =
        sqltest_root.join("artifacts/external/runs/c-baseline-001/status.json");

    if !manifest_path.is_file() || !baseline_status_path.is_file() {
        // Allow `cargo test` to pass cleanly in checkouts without frozen external artifacts.
        return;
    }

    let manifest = JsonParser::parse(&fs::read_to_string(&manifest_path).unwrap());
    let baseline_status = JsonParser::parse(&fs::read_to_string(&baseline_status_path).unwrap());
    let baseline_entries = baseline_status
        .get("correctness")
        .unwrap()
        .as_arr()
        .unwrap();

    let jobs = build_correctness_plan(&manifest);
    assert_eq!(jobs.len(), 168, "expected 168 correctness jobs");
    assert_eq!(baseline_entries.len(), 168);

    let data_dir = sqltest_root.join("artifacts/external/inputs");
    let datasets = manifest.get("datasets").unwrap();
    let cfg = manifest.get("config").unwrap();

    let mut mismatches: Vec<String> = Vec::new();

    for (job_idx, job) in jobs.iter().enumerate() {
        if !include_idx(job_idx) {
            continue;
        }
        let base_entry = &baseline_entries[job_idx];
        assert_eq!(
            base_entry.get("id").unwrap().as_str().unwrap(),
            job.id,
            "job index {job_idx} ID mismatch"
        );
        let base_rel = base_entry.get("result_path").unwrap().as_str().unwrap();
        let base_res = JsonParser::parse(&fs::read_to_string(sqltest_root.join(base_rel)).unwrap());
        let expected_status = base_res.get("status").unwrap().as_str().unwrap();
        let expected_artifacts = base_res.get("artifacts").unwrap().as_arr().unwrap();

        let sandbox = TempSandbox::new(&format!("corr-{job_idx:04}"));
        let db_path = sandbox.path("case.db");

        let max_col = match &job.kind {
            CorrectnessJobKind::Wide { maximum }
            | CorrectnessJobKind::Boundary { maximum, .. }
            | CorrectnessJobKind::Additional { maximum, .. } => *maximum,
            CorrectnessJobKind::Append
            | CorrectnessJobKind::UuidFull { .. }
            | CorrectnessJobKind::Core { .. } => 2000,
        };

        let mut conn = open_configured_db(&db_path, &sandbox.dir, max_col);
        let mut actual_artifacts: Vec<Vec<u8>> = Vec::new();
        let actual_status: &str;

        match &job.kind {
            CorrectnessJobKind::Wide { maximum } => {
                let ds_folder = sqltest_root.join(
                    datasets
                        .get(&format!("wide-{maximum}"))
                        .unwrap()
                        .get("folder")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                );
                exec_script_discard(
                    &conn,
                    &fs::read_to_string(ds_folder.join("schema.txt")).unwrap(),
                )
                .unwrap_or_else(|e| panic!("job {}: schema failed: {:?}", job.id, e));
                exec_script_discard(
                    &conn,
                    &fs::read_to_string(data_dir.join("begin.txt")).unwrap(),
                )
                .unwrap();
                let mut ins = conn
                    .prepare(&fs::read_to_string(ds_folder.join("insert-template.txt")).unwrap())
                    .unwrap();
                let csv_text = fs::read_to_string(ds_folder.join("rows.csv")).unwrap();
                let mut lines = csv_text.lines();
                let _header = lines.next().unwrap();
                let mut row_vals = Vec::with_capacity(*maximum);
                for line in lines {
                    if line.is_empty() {
                        continue;
                    }
                    row_vals.clear();
                    for tok in line.trim_end_matches('\r').split(',') {
                        row_vals.push(token_to_value(tok));
                    }
                    ins.bind_all(&row_vals).unwrap();
                    let _ = ins.step().unwrap();
                    ins.reset().unwrap();
                }
                ins.finalize().unwrap();
                exec_script_discard(
                    &conn,
                    &fs::read_to_string(data_dir.join("commit.txt")).unwrap(),
                )
                .unwrap();

                let select_sql = fs::read_to_string(ds_folder.join("select.txt")).unwrap();
                let mut base_wire = Vec::new();
                execute_script_wire(&conn, &select_sql, false, &mut base_wire).unwrap();
                actual_artifacts.push(base_wire);

                let workload_sql = fs::read_to_string(ds_folder.join("workload.txt")).unwrap();
                let mut var_wire = Vec::new();
                execute_script_wire(&conn, &workload_sql, true, &mut var_wire)
                    .unwrap_or_else(|e| panic!("job {}: workload failed: {:?}", job.id, e));
                actual_artifacts.push(var_wire);

                let mut mutated_wire = Vec::new();
                execute_script_wire(&conn, &select_sql, false, &mut mutated_wire).unwrap();
                actual_artifacts.push(mutated_wire);
                actual_status = "PASS";
            }

            CorrectnessJobKind::Append => {
                exec_script_discard(
                    &conn,
                    &fs::read_to_string(data_dir.join("schema.txt")).unwrap(),
                )
                .unwrap();
                let seeds: Vec<i64> = cfg
                    .get("seeds")
                    .unwrap()
                    .as_arr()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_i64().unwrap())
                    .collect();
                for (label, list) in [("full", &seeds[..1]), ("append", &seeds[..])] {
                    for seed in list {
                        let ds_folder = sqltest_root.join(
                            datasets
                                .get(&format!("{seed}-{label}"))
                                .unwrap()
                                .get("folder")
                                .unwrap()
                                .as_str()
                                .unwrap(),
                        );
                        exec_script_discard(
                            &conn,
                            &fs::read_to_string(ds_folder.join("insert-one-txn.txt")).unwrap(),
                        )
                        .unwrap();
                    }
                }
                let mut append_wire = Vec::new();
                execute_script_wire(
                    &conn,
                    &fs::read_to_string(data_dir.join("select.txt")).unwrap(),
                    false,
                    &mut append_wire,
                )
                .unwrap();
                actual_artifacts.push(append_wire);
                actual_status = "PASS";
            }

            CorrectnessJobKind::UuidFull { seed } => {
                let ds_folder = sqltest_root.join(
                    datasets
                        .get(&format!("uuid7-{seed}-full"))
                        .unwrap()
                        .get("folder")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                );
                exec_script_discard(
                    &conn,
                    &fs::read_to_string(ds_folder.join("schema.txt")).unwrap(),
                )
                .unwrap();
                let fk_row = conn
                    .query_row(
                        &fs::read_to_string(ds_folder.join("foreign-keys-readback.txt")).unwrap(),
                        &[],
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(fk_row.get_i64(0), Some(1));
                exec_script_discard(
                    &conn,
                    &fs::read_to_string(ds_folder.join("parents-insert.txt")).unwrap(),
                )
                .unwrap();
                exec_script_discard(
                    &conn,
                    &fs::read_to_string(ds_folder.join("details-insert.txt")).unwrap(),
                )
                .unwrap();

                for stem in ["parents", "details", "join", "aggregate"] {
                    let mut w = Vec::new();
                    execute_script_wire(
                        &conn,
                        &fs::read_to_string(ds_folder.join(format!("{stem}-query.txt"))).unwrap(),
                        false,
                        &mut w,
                    )
                    .unwrap_or_else(|e| panic!("job {} {stem}: {:?}", job.id, e));
                    actual_artifacts.push(w);
                }
                let mut plain_w = Vec::new();
                execute_script_wire(
                    &conn,
                    &fs::read_to_string(ds_folder.join("join-noindex.txt")).unwrap(),
                    false,
                    &mut plain_w,
                )
                .unwrap();
                actual_artifacts.push(plain_w);

                for (leaf, ext) in [
                    ("negative-orphan.txt", 787),
                    ("negative-duplicate.txt", 1555),
                ] {
                    let err = exec_script_discard(
                        &conn,
                        &fs::read_to_string(ds_folder.join(leaf)).unwrap(),
                    )
                    .expect_err("UUID negative must fail");
                    assert_eq!(err.stage, "step", "job {} {leaf} stage", job.id);
                    assert_eq!(err.err.code, ErrorCode::Constraint);
                    assert_eq!(err.err.extended_code, ext, "job {} {leaf} ext", job.id);
                }
                assert!(
                    conn.query("PRAGMA foreign_key_check;", &[])
                        .unwrap()
                        .rows
                        .is_empty()
                );
                actual_status = "PASS";
            }

            CorrectnessJobKind::Core { spec } => {
                let negative = spec
                    .get("intentionally_negative")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                for pre in spec.get("prerequisites").unwrap().as_arr().unwrap() {
                    let pre_sql =
                        fs::read_to_string(data_dir.join("catalog").join(pre.as_str().unwrap()))
                            .unwrap();
                    exec_script_discard(&conn, &pre_sql).unwrap_or_else(|e| {
                        panic!("job {} prerequisite {:?} failed: {:?}", job.id, pre, e)
                    });
                }
                let sql_rel = spec.get("sql_path").unwrap().as_str().unwrap();
                let sql = fs::read_to_string(data_dir.join("catalog").join(sql_rel)).unwrap();
                let mut var_wire = Vec::new();
                let res = execute_script_wire(&conn, &sql, true, &mut var_wire);
                actual_artifacts.push(var_wire);

                match res {
                    Ok(_) => {
                        assert!(
                            !negative,
                            "job {}: expected negative error accepted",
                            job.id
                        );
                        actual_status = "PASS";
                    }
                    Err(se) => {
                        assert!(
                            negative,
                            "job {}: unexpected error at {}: {:?}",
                            job.id, se.stage, se.err
                        );
                        let exp_err = &base_res.get("errors").unwrap().as_arr().unwrap()[0];
                        let exp_primary = exp_err.get("primary").unwrap().as_i64().unwrap() as i32;
                        let exp_ext = exp_err.get("extended").unwrap().as_i64().unwrap() as i32;
                        let exp_stage = exp_err.get("stage").unwrap().as_str().unwrap();
                        let exp_ac = exp_err.get("autocommit").unwrap().as_bool().unwrap();
                        assert_eq!(
                            se.stage, exp_stage,
                            "job {}: stage mismatch ({:?})",
                            job.id, se.err
                        );
                        assert_eq!(
                            se.err.code as i32, exp_primary,
                            "job {}: primary code mismatch ({:?})",
                            job.id, se.err
                        );
                        if exp_primary == 19 {
                            assert_eq!(
                                se.err.extended_code, exp_ext,
                                "job {}: extended code mismatch ({:?})",
                                job.id, se.err
                            );
                        }
                        assert_eq!(
                            conn.is_autocommit(),
                            exp_ac,
                            "job {}: autocommit after error mismatch",
                            job.id
                        );
                        actual_status = "EXPECTED_ERROR";
                    }
                }
            }

            CorrectnessJobKind::Boundary { spec, .. } => {
                let negative = spec.get("limit_delta").unwrap().as_i64().unwrap() > 0;
                let src_rel = spec.get("source_path").unwrap().as_str().unwrap();
                let sql = fs::read_to_string(data_dir.join("catalog").join(src_rel)).unwrap();
                let mut var_wire = Vec::new();
                let res = execute_script_wire(&conn, &sql, true, &mut var_wire);
                actual_artifacts.push(var_wire);

                match res {
                    Ok(_) => {
                        assert!(!negative, "job {}: boundary overflow was accepted!", job.id);
                        actual_status = "PASS";
                    }
                    Err(se) => {
                        assert!(
                            negative,
                            "job {}: in-limit boundary failed at {}: {:?}",
                            job.id, se.stage, se.err
                        );
                        assert_eq!(
                            se.stage, "prepare",
                            "job {}: expected prepare error, got {}: {:?}",
                            job.id, se.stage, se.err
                        );
                        assert_eq!(
                            se.err.code,
                            ErrorCode::Error,
                            "job {}: expected ErrorCode::Error, got {:?}",
                            job.id,
                            se.err
                        );
                        actual_status = "EXPECTED_ERROR";
                    }
                }
            }

            CorrectnessJobKind::Additional { maximum, spec } => {
                let family = spec.get("family").unwrap().as_str().unwrap();
                let paths: Vec<&str> = spec
                    .get("paths")
                    .unwrap()
                    .as_arr()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().unwrap())
                    .collect();

                if family == "conflict" {
                    let s0 = fs::read_to_string(data_dir.join("catalog").join(paths[0])).unwrap();
                    let s1 = fs::read_to_string(data_dir.join("catalog").join(paths[1])).unwrap();
                    let s2 = fs::read_to_string(data_dir.join("catalog").join(paths[2])).unwrap();
                    exec_script_discard(&conn, &s0).unwrap();
                    let se = exec_script_discard(&conn, &s1)
                        .expect_err(&format!("job {}: conflict must fail", job.id));
                    assert_eq!(se.stage, "step");
                    assert_eq!(se.err.code, ErrorCode::Constraint);
                    assert_eq!(
                        se.err.extended_code, 2067,
                        "job {}: expected SQLITE_CONSTRAINT_UNIQUE (2067), got {:?}",
                        job.id, se.err
                    );
                    let got_ids: Vec<i64> = conn
                        .query("SELECT id FROM conflict_demo ORDER BY id;", &[])
                        .unwrap()
                        .rows
                        .iter()
                        .map(|r| r.get_i64(0).unwrap())
                        .collect();
                    let exp_ids: Vec<i64> = spec
                        .get("expected_ids")
                        .unwrap()
                        .as_arr()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_i64().unwrap())
                        .collect();
                    assert_eq!(got_ids, exp_ids, "job {}: surviving IDs mismatch", job.id);
                    let exp_ac = spec
                        .get("expected_autocommit_after_error")
                        .unwrap()
                        .as_bool()
                        .unwrap();
                    assert_eq!(
                        conn.is_autocommit(),
                        exp_ac,
                        "job {}: autocommit state mismatch",
                        job.id
                    );
                    let mut var_wire = Vec::new();
                    execute_script_wire(&conn, &s2, true, &mut var_wire).unwrap();
                    actual_artifacts.push(var_wire);
                    actual_status = "EXPECTED_ERROR";
                } else if family == "storage" {
                    let s0 = fs::read_to_string(data_dir.join("catalog").join(paths[0])).unwrap();
                    let s1 = fs::read_to_string(data_dir.join("catalog").join(paths[1])).unwrap();
                    exec_script_discard(&conn, &s0).unwrap();
                    conn.try_close().expect("close storage db cleanly");
                    conn = open_configured_db(&db_path, &sandbox.dir, *maximum);
                    let exp_ps = spec.get("expected_page_size").unwrap().as_i64().unwrap();
                    let ps = conn
                        .query_row("PRAGMA page_size;", &[])
                        .unwrap()
                        .unwrap()
                        .get_i64(0)
                        .unwrap();
                    assert_eq!(ps, exp_ps, "job {}: reopened page_size mismatch", job.id);
                    let mut var_wire = Vec::new();
                    execute_script_wire(&conn, &s1, true, &mut var_wire).unwrap();
                    actual_artifacts.push(var_wire);
                    let exp_cnt = spec.get("planned_records").unwrap().as_i64().unwrap();
                    let cnt = conn
                        .query_row("SELECT count(*) FROM page_probe;", &[])
                        .unwrap()
                        .unwrap()
                        .get_i64(0)
                        .unwrap();
                    assert_eq!(cnt, exp_cnt, "job {}: reopened row count mismatch", job.id);
                    actual_status = "PASS";
                } else {
                    if let Some(pres) = spec.get("prerequisites").and_then(Json::as_arr) {
                        for pre in pres {
                            let pre_sql = fs::read_to_string(
                                data_dir.join("catalog").join(pre.as_str().unwrap()),
                            )
                            .unwrap();
                            exec_script_discard(&conn, &pre_sql).unwrap_or_else(|e| {
                                panic!("job {} pre {:?}: {:?}", job.id, pre, e)
                            });
                        }
                    }
                    for p in &paths {
                        let sql = fs::read_to_string(data_dir.join("catalog").join(p)).unwrap();
                        let mut var_wire = Vec::new();
                        execute_script_wire(&conn, &sql, true, &mut var_wire)
                            .unwrap_or_else(|e| panic!("job {} path {p}: {:?}", job.id, e));
                        actual_artifacts.push(var_wire);
                    }
                    if job.id == "relational-mutation-stress" {
                        let mut sum = 0i64;
                        for tbl in ["customers", "orders", "items", "employees", "measurements"] {
                            sum += conn
                                .query_row(&format!("SELECT count(*) FROM {tbl};"), &[])
                                .unwrap()
                                .unwrap()
                                .get_i64(0)
                                .unwrap();
                        }
                        let exp_after =
                            spec.get("planned_records_after").unwrap().as_i64().unwrap();
                        assert_eq!(sum, exp_after, "job {}: stress count mismatch", job.id);
                    }
                    actual_status = "PASS";
                }
            }
        }

        assert_eq!(
            actual_status, expected_status,
            "job {} ({job_idx:04}): status mismatch",
            job.id
        );
        assert_eq!(
            actual_artifacts.len(),
            expected_artifacts.len(),
            "job {} ({job_idx:04}): artifact count mismatch",
            job.id
        );

        for (art_idx, (actual_bytes, exp_meta)) in actual_artifacts
            .iter()
            .zip(expected_artifacts.iter())
            .enumerate()
        {
            let golden_rel = exp_meta.get("path").unwrap().as_str().unwrap();
            let golden_bytes = fs::read(sqltest_root.join(golden_rel)).unwrap();
            if job.id == "explain" {
                let actual_str = String::from_utf8_lossy(actual_bytes);
                let explain_rows: Vec<&str> = actual_str
                    .lines()
                    .filter(|l| l.starts_with("8\t"))
                    .collect();
                let eqp_rows: Vec<&str> = actual_str
                    .lines()
                    .filter(|l| l.starts_with("4\t"))
                    .collect();
                assert!(
                    explain_rows.len() >= 5 && explain_rows[0].contains("T:496e6974"),
                    "explain VDBE bytecode rows missing Init: {explain_rows:?}"
                );
                assert_eq!(eqp_rows.len(), 2, "expected 2 EQP rows: {eqp_rows:?}");
                continue;
            }
            if actual_bytes != &golden_bytes {
                let actual_str = String::from_utf8_lossy(actual_bytes);
                let golden_str = String::from_utf8_lossy(&golden_bytes);
                let first_diff = actual_str
                    .lines()
                    .zip(golden_str.lines())
                    .enumerate()
                    .find(|(_, (a, b))| a != b);
                mismatches.push(format!(
                    "job {} ({job_idx:04}) artifact #{art_idx} ({golden_rel}) wire mismatch: actual_lines={} golden_lines={} first_diff={:?}",
                    job.id,
                    actual_str.lines().count(),
                    golden_str.lines().count(),
                    first_diff
                ));
            }
        }

        verify_integrity_ok(&conn, &job.id);
        let exp_ac_end = base_res.get("autocommit_end").unwrap().as_bool().unwrap();
        assert_eq!(
            conn.is_autocommit(),
            exp_ac_end,
            "job {}: autocommit_end mismatch",
            job.id
        );
        conn.try_close().expect("clean close at end of job");
    }

    if !mismatches.is_empty() {
        panic!(
            "{} correctness mismatches:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

#[test]
fn test_sqltest_correctness_catalog() {
    // Runs `wide-128` (0000) plus all 165 append / UUID / core / boundary / optional / storage /
    // stress jobs (`0003..=0167`) by default in `cargo test --workspace`.
    run_sqltest_correctness_subset(|idx| idx == 0 || idx >= 3);
}

#[test]
#[ignore = "heavy 3000x2000 and 3000x4096 multi-hundred-MB wide overflow soak; run with --ignored"]
fn test_sqltest_wide_2000_and_4096_catalog() {
    run_sqltest_correctness_subset(|idx| idx == 1 || idx == 2);
}

#[test]
fn test_p1a_c_sqlite_3541_equivalence_edge_cases() {
    let conn = Connection::open_in_memory().unwrap();

    // F-08: Only ASCII whitespace (`' '`, `'\t'`, `'\n'`, `'\r'`, `'\x0b'`, `'\x0c'`) is trimmed
    // in numeric conversions; Unicode NBSP (`U+00A0`) is NOT trimmed.
    let rows = conn
        .query(
            "SELECT \
                CAST(char(160) || '12' AS INTEGER), \
                CAST(char(160) || '12.5' AS REAL), \
                CAST(char(160) || '12' AS NUMERIC), \
                CAST(char(11) || char(12) || '42' AS INTEGER), \
                (char(160) || '12') + 0, \
                (' \t\n\r\x0b\x0c99 ' ) + 0",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(
        rows[0].values(),
        &[
            Value::Integer(0),
            Value::Real(0.0),
            Value::Integer(0),
            Value::Integer(42),
            Value::Integer(0),
            Value::Integer(99),
        ]
    );
    conn.execute_batch("CREATE TABLE f08_num(v NUMERIC); INSERT INTO f08_num VALUES (char(160) || '12'), ('12'|| char(160)), (' 12 ');")
        .unwrap();
    let f08_rows = conn
        .query("SELECT typeof(v), v FROM f08_num ORDER BY rowid", &[])
        .unwrap()
        .rows;
    assert_eq!(
        f08_rows[0].values(),
        &[Value::text("text"), Value::text("\u{00a0}12")]
    );
    assert_eq!(
        f08_rows[1].values(),
        &[Value::text("text"), Value::text("12\u{00a0}")]
    );
    assert_eq!(
        f08_rows[2].values(),
        &[Value::text("integer"), Value::Integer(12)]
    );

    // F-06: Overflowing float exponent in `parse_numeric_text` yields `±Inf`, whereas literal
    // words `'inf'`, `'-inf'`, `'infinity'`, `'nan'` stay non-numeric (`TEXT` / `0` in `+ 0`).
    let f06_arith = conn
        .query(
            "SELECT '1e309' + 0, '-1e309' + 0, 'inf' + 0, '-inf' + 0, 'infinity' + 0, 'nan' + 0",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(
        f06_arith[0].values(),
        &[
            Value::Real(f64::INFINITY),
            Value::Real(f64::NEG_INFINITY),
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(0),
        ]
    );
    conn.execute_batch(
        "CREATE TABLE f06_num(n NUMERIC, r REAL); \
         INSERT INTO f06_num VALUES ('1e400', '1e400'), ('-1e400', '-1e400'), ('inf', 'inf'), ('nan', 'nan');",
    )
    .unwrap();
    let f06_tbl = conn
        .query(
            "SELECT typeof(n), n, typeof(r), r FROM f06_num ORDER BY rowid",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(
        f06_tbl[0].values(),
        &[
            Value::text("real"),
            Value::Real(f64::INFINITY),
            Value::text("real"),
            Value::Real(f64::INFINITY),
        ]
    );
    assert_eq!(
        f06_tbl[1].values(),
        &[
            Value::text("real"),
            Value::Real(f64::NEG_INFINITY),
            Value::text("real"),
            Value::Real(f64::NEG_INFINITY),
        ]
    );
    assert_eq!(
        f06_tbl[2].values(),
        &[
            Value::text("text"),
            Value::text("inf"),
            Value::text("text"),
            Value::text("inf"),
        ]
    );
    assert_eq!(
        f06_tbl[3].values(),
        &[
            Value::text("text"),
            Value::text("nan"),
            Value::text("text"),
            Value::text("nan"),
        ]
    );

    // F-12: `GLOB` bracket negation uses ONLY `[^...]`; `[!a]` matches `'!'` or `'a'` literally.
    let f12 = conn
        .query(
            "SELECT 'a' GLOB '[!a]', '!' GLOB '[!a]', 'b' GLOB '[!a]', \
                    'a' GLOB '[^a]', 'b' GLOB '[^a]'",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(
        f12[0].values(),
        &[
            Value::Integer(1),
            Value::Integer(1),
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(1),
        ]
    );

    // F-13: VDBE `SeekRowid` (`OP_SeekRowid` / `OP_MustBeInt`) with `TEXT` and `REAL` keys.
    conn.execute_batch(
        "CREATE TABLE f13_t(id INTEGER PRIMARY KEY, a TEXT); \
         INSERT INTO f13_t VALUES (2, 'two'), (10, 'ten');",
    )
    .unwrap();
    let mut stmt = conn.prepare("SELECT a FROM f13_t WHERE id = ?1").unwrap();
    for (param, expected) in [
        (Value::text("2"), Some("two")),
        (Value::text(" 2.0 "), Some("two")),
        (Value::text("1e1"), Some("ten")),
        (Value::Real(2.0), Some("two")),
        (Value::text("2.5"), None),
        (Value::text("abc"), None),
        (Value::text("\u{00a0}2"), None),
        (Value::Real(9_223_372_036_854_775_808.0), None),
    ] {
        stmt.reset().unwrap();
        stmt.bind(1, param.clone()).unwrap();
        let got = stmt.collect_rows().unwrap();
        match expected {
            Some(exp) => {
                assert_eq!(got.len(), 1, "expected match for {param:?}");
                assert_eq!(got[0].values(), &[Value::text(exp)]);
            }
            None => assert!(got.is_empty(), "expected miss for {param:?}, got {got:?}"),
        }
    }
    drop(stmt);

    // F-14: `NATURAL JOIN` and `JOIN ... USING` use the left table column's collation.
    conn.execute_batch(
        "CREATE TABLE f14_t1(b TEXT); \
         CREATE TABLE f14_t2(B TEXT COLLATE NOCASE); \
         INSERT INTO f14_t1 VALUES ('abc'); \
         INSERT INTO f14_t2 VALUES ('ABC');",
    )
    .unwrap();
    assert!(
        conn.query("SELECT * FROM f14_t1 NATURAL JOIN f14_t2", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    assert!(
        conn.query("SELECT * FROM f14_t1 JOIN f14_t2 USING(b)", &[])
            .unwrap()
            .rows
            .is_empty()
    );
    let rev_nat = conn
        .query("SELECT * FROM f14_t2 NATURAL JOIN f14_t1", &[])
        .unwrap()
        .rows;
    assert_eq!(rev_nat.len(), 1);
    assert_eq!(rev_nat[0].values(), &[Value::text("ABC")]);
    let rev_using = conn
        .query("SELECT * FROM f14_t2 JOIN f14_t1 USING(B)", &[])
        .unwrap()
        .rows;
    assert_eq!(rev_using.len(), 1);
    assert_eq!(rev_using[0].values(), &[Value::text("ABC")]);
}
