//! Query Result Formatter: a port of `sqlite/ext/qrf/qrf.c` (SQLite 3.54.1) onto the
//! statement interface of this engine. Function and field names follow the C source so that
//! the two can be read side by side.
#![allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::io::Write;

use sqliters::{Connection, SqliteError, Statement, StepResult, Value, types::format_sqlite_real};

pub const QRF_STYLE_AUTO: u8 = 0;
pub const QRF_STYLE_BOX: u8 = 1;
pub const QRF_STYLE_COLUMN: u8 = 2;
pub const QRF_STYLE_COUNT: u8 = 3;
pub const QRF_STYLE_CSV: u8 = 4;
pub const QRF_STYLE_EQP: u8 = 5;
pub const QRF_STYLE_EXPLAIN: u8 = 6;
pub const QRF_STYLE_HTML: u8 = 7;
pub const QRF_STYLE_INSERT: u8 = 8;
pub const QRF_STYLE_JSON: u8 = 9;
pub const QRF_STYLE_JOBJECT: u8 = 10;
pub const QRF_STYLE_LINE: u8 = 11;
pub const QRF_STYLE_LIST: u8 = 12;
pub const QRF_STYLE_MARKDOWN: u8 = 13;
pub const QRF_STYLE_OFF: u8 = 14;
pub const QRF_STYLE_QUOTE: u8 = 15;
pub const QRF_STYLE_STATS: u8 = 16;
pub const QRF_STYLE_STATSEST: u8 = 17;
pub const QRF_STYLE_STATSVM: u8 = 18;
pub const QRF_STYLE_TABLE: u8 = 19;

pub const QRF_TEXT_PLAIN: u8 = 1;
pub const QRF_TEXT_SQL: u8 = 2;
pub const QRF_TEXT_CSV: u8 = 3;
pub const QRF_TEXT_HTML: u8 = 4;
pub const QRF_TEXT_TCL: u8 = 5;
pub const QRF_TEXT_JSON: u8 = 6;
pub const QRF_TEXT_RELAXED: u8 = 7;

pub const QRF_BLOB_AUTO: u8 = 0;
pub const QRF_BLOB_TEXT: u8 = 1;
pub const QRF_BLOB_SQL: u8 = 2;
pub const QRF_BLOB_HEX: u8 = 3;
pub const QRF_BLOB_TCL: u8 = 4;
pub const QRF_BLOB_JSON: u8 = 5;
pub const QRF_BLOB_SIZE: u8 = 6;

pub const QRF_ESC_OFF: u8 = 1;
pub const QRF_ESC_ASCII: u8 = 2;
pub const QRF_ESC_SYMBOL: u8 = 3;

pub const QRF_AUTO: u8 = 0;
pub const QRF_NO: u8 = 1;
pub const QRF_YES: u8 = 2;
pub const QRF_ALWAYS: u8 = 3;

pub const QRF_ALIGN_LEFT: u8 = 1;
pub const QRF_ALIGN_CENTER: u8 = 2;
pub const QRF_ALIGN_RIGHT: u8 = 3;
pub const QRF_ALIGN_HMASK: u8 = 3;
pub const QRF_ALIGN_VMASK: u8 = 12;

pub const QRF_MAX_WIDTH: i64 = 10000;

/// `sqlite3_qrf_spec`.
#[derive(Clone, Debug, Default)]
pub struct QrfSpec {
    pub e_style: u8,
    pub e_esc: u8,
    pub e_text: u8,
    pub e_title: u8,
    pub e_blob: u8,
    pub b_titles: u8,
    pub b_word_wrap: u8,
    pub b_text_jsonb: u8,
    pub e_dflt_align: u8,
    pub e_title_align: u8,
    pub b_split_column: u8,
    pub b_border: u8,
    pub n_wrap: i16,
    pub n_screen_width: i16,
    pub n_line_limit: i16,
    pub n_title_limit: i16,
    pub n_multi_insert: u32,
    pub n_char_limit: i32,
    pub a_width: Vec<i16>,
    pub a_align: Vec<u8>,
    pub z_column_sep: Option<String>,
    pub z_row_sep: Option<String>,
    pub z_table_name: Option<String>,
    pub z_null: Option<String>,
    pub b_row_count: u8,
    pub z_ifmt: Option<String>,
    pub z_fpfmt: Option<String>,
}

/// What `sqlite3_stmt_isexplain()` would report for the statement text: 0 = ordinary
/// statement, 1 = EXPLAIN, 2 = EXPLAIN QUERY PLAN.
pub fn stmt_isexplain(sql: &str) -> u8 {
    // The first keyword of the statement, after white space and comments.
    let b = sql.as_bytes();
    let mut i = 0usize;
    let mut words: [&[u8]; 3] = [b""; 3];
    let mut n = 0usize;
    while i < b.len() && n < 3 {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            words[n] = &b[start..i];
            n += 1;
            if n == 1 && !words[0].eq_ignore_ascii_case(b"explain") {
                return 0;
            }
        } else {
            break;
        }
    }
    if !words[0].eq_ignore_ascii_case(b"explain") {
        return 0;
    }
    if words[1].eq_ignore_ascii_case(b"query") && words[2].eq_ignore_ascii_case(b"plan") {
        return 2;
    }
    1
}

/// `aQrfUWidth[]` of qrf.c: (width, first code point of the span).
pub(crate) const QRF_UWIDTH: [(u8, u32); 303] = [
    (0, 0x00300),
    (1, 0x00370),
    (0, 0x00483),
    (1, 0x00487),
    (0, 0x00488),
    (1, 0x0048a),
    (0, 0x00591),
    (1, 0x005be),
    (0, 0x005bf),
    (1, 0x005c0),
    (0, 0x005c1),
    (1, 0x005c3),
    (0, 0x005c4),
    (1, 0x005c6),
    (0, 0x005c7),
    (1, 0x005c8),
    (0, 0x00600),
    (1, 0x00604),
    (0, 0x00610),
    (1, 0x00616),
    (0, 0x0064b),
    (1, 0x0065f),
    (0, 0x00670),
    (1, 0x00671),
    (0, 0x006d6),
    (1, 0x006e5),
    (0, 0x006e7),
    (1, 0x006e9),
    (0, 0x006ea),
    (1, 0x006ee),
    (0, 0x0070f),
    (1, 0x00710),
    (0, 0x00711),
    (1, 0x00712),
    (0, 0x00730),
    (1, 0x0074b),
    (0, 0x007a6),
    (1, 0x007b1),
    (0, 0x007eb),
    (1, 0x007f4),
    (0, 0x00901),
    (1, 0x00903),
    (0, 0x0093c),
    (1, 0x0093d),
    (0, 0x00941),
    (1, 0x00949),
    (0, 0x0094d),
    (1, 0x0094e),
    (0, 0x00951),
    (1, 0x00955),
    (0, 0x00962),
    (1, 0x00964),
    (0, 0x00981),
    (1, 0x00982),
    (0, 0x009bc),
    (1, 0x009bd),
    (0, 0x009c1),
    (1, 0x009c5),
    (0, 0x009cd),
    (1, 0x009ce),
    (0, 0x009e2),
    (1, 0x009e4),
    (0, 0x00a01),
    (1, 0x00a03),
    (0, 0x00a3c),
    (1, 0x00a3d),
    (0, 0x00a41),
    (1, 0x00a43),
    (0, 0x00a47),
    (1, 0x00a49),
    (0, 0x00a4b),
    (1, 0x00a4e),
    (0, 0x00a70),
    (1, 0x00a72),
    (0, 0x00a81),
    (1, 0x00a83),
    (0, 0x00abc),
    (1, 0x00abd),
    (0, 0x00ac1),
    (1, 0x00ac6),
    (0, 0x00ac7),
    (1, 0x00ac9),
    (0, 0x00acd),
    (1, 0x00ace),
    (0, 0x00ae2),
    (1, 0x00ae4),
    (0, 0x00b01),
    (1, 0x00b02),
    (0, 0x00b3c),
    (1, 0x00b3d),
    (0, 0x00b3f),
    (1, 0x00b40),
    (0, 0x00b41),
    (1, 0x00b44),
    (0, 0x00b4d),
    (1, 0x00b4e),
    (0, 0x00b56),
    (1, 0x00b57),
    (0, 0x00b82),
    (1, 0x00b83),
    (0, 0x00bc0),
    (1, 0x00bc1),
    (0, 0x00bcd),
    (1, 0x00bce),
    (0, 0x00c3e),
    (1, 0x00c41),
    (0, 0x00c46),
    (1, 0x00c49),
    (0, 0x00c4a),
    (1, 0x00c4e),
    (0, 0x00c55),
    (1, 0x00c57),
    (0, 0x00cbc),
    (1, 0x00cbd),
    (0, 0x00cbf),
    (1, 0x00cc0),
    (0, 0x00cc6),
    (1, 0x00cc7),
    (0, 0x00ccc),
    (1, 0x00cce),
    (0, 0x00ce2),
    (1, 0x00ce4),
    (0, 0x00d41),
    (1, 0x00d44),
    (0, 0x00d4d),
    (1, 0x00d4e),
    (0, 0x00dca),
    (1, 0x00dcb),
    (0, 0x00dd2),
    (1, 0x00dd5),
    (0, 0x00dd6),
    (1, 0x00dd7),
    (0, 0x00e31),
    (1, 0x00e32),
    (0, 0x00e34),
    (1, 0x00e3b),
    (0, 0x00e47),
    (1, 0x00e4f),
    (0, 0x00eb1),
    (1, 0x00eb2),
    (0, 0x00eb4),
    (1, 0x00eba),
    (0, 0x00ebb),
    (1, 0x00ebd),
    (0, 0x00ec8),
    (1, 0x00ece),
    (0, 0x00f18),
    (1, 0x00f1a),
    (0, 0x00f35),
    (1, 0x00f36),
    (0, 0x00f37),
    (1, 0x00f38),
    (0, 0x00f39),
    (1, 0x00f3a),
    (0, 0x00f71),
    (1, 0x00f7f),
    (0, 0x00f80),
    (1, 0x00f85),
    (0, 0x00f86),
    (1, 0x00f88),
    (0, 0x00f90),
    (1, 0x00f98),
    (0, 0x00f99),
    (1, 0x00fbd),
    (0, 0x00fc6),
    (1, 0x00fc7),
    (0, 0x0102d),
    (1, 0x01031),
    (0, 0x01032),
    (1, 0x01033),
    (0, 0x01036),
    (1, 0x0103b),
    (0, 0x01058),
    (1, 0x0105a),
    (2, 0x01100),
    (0, 0x01160),
    (1, 0x01200),
    (0, 0x0135f),
    (1, 0x01360),
    (0, 0x01712),
    (1, 0x01715),
    (0, 0x01732),
    (1, 0x01735),
    (0, 0x01752),
    (1, 0x01754),
    (0, 0x01772),
    (1, 0x01774),
    (0, 0x017b4),
    (1, 0x017b6),
    (0, 0x017b7),
    (1, 0x017be),
    (0, 0x017c6),
    (1, 0x017c7),
    (0, 0x017c9),
    (1, 0x017d4),
    (0, 0x017dd),
    (1, 0x017de),
    (0, 0x0180b),
    (1, 0x0180e),
    (0, 0x018a9),
    (1, 0x018aa),
    (0, 0x01920),
    (1, 0x01923),
    (0, 0x01927),
    (1, 0x01929),
    (0, 0x01932),
    (1, 0x01933),
    (0, 0x01939),
    (1, 0x0193c),
    (0, 0x01a17),
    (1, 0x01a19),
    (0, 0x01b00),
    (1, 0x01b04),
    (0, 0x01b34),
    (1, 0x01b35),
    (0, 0x01b36),
    (1, 0x01b3b),
    (0, 0x01b3c),
    (1, 0x01b3d),
    (0, 0x01b42),
    (1, 0x01b43),
    (0, 0x01b6b),
    (1, 0x01b74),
    (0, 0x01dc0),
    (1, 0x01dcb),
    (0, 0x01dfe),
    (1, 0x01e00),
    (0, 0x0200b),
    (1, 0x02010),
    (0, 0x0202a),
    (1, 0x0202f),
    (0, 0x02060),
    (1, 0x02064),
    (0, 0x0206a),
    (1, 0x02070),
    (0, 0x020d0),
    (1, 0x020f0),
    (2, 0x02329),
    (1, 0x0232b),
    (2, 0x02e80),
    (0, 0x0302a),
    (2, 0x03030),
    (1, 0x0303f),
    (2, 0x03040),
    (0, 0x03099),
    (2, 0x0309b),
    (1, 0x0a4d0),
    (0, 0x0a806),
    (1, 0x0a807),
    (0, 0x0a80b),
    (1, 0x0a80c),
    (0, 0x0a825),
    (1, 0x0a827),
    (2, 0x0ac00),
    (1, 0x0d7a4),
    (2, 0x0f900),
    (1, 0x0fb00),
    (0, 0x0fb1e),
    (1, 0x0fb1f),
    (0, 0x0fe00),
    (2, 0x0fe10),
    (1, 0x0fe1a),
    (0, 0x0fe20),
    (1, 0x0fe24),
    (2, 0x0fe30),
    (1, 0x0fe70),
    (0, 0x0feff),
    (2, 0x0ff00),
    (1, 0x0ff61),
    (2, 0x0ffe0),
    (1, 0x0ffe7),
    (0, 0x0fff9),
    (1, 0x0fffc),
    (0, 0x10a01),
    (1, 0x10a04),
    (0, 0x10a05),
    (1, 0x10a07),
    (0, 0x10a0c),
    (1, 0x10a10),
    (0, 0x10a38),
    (1, 0x10a3b),
    (0, 0x10a3f),
    (1, 0x10a40),
    (0, 0x1d167),
    (1, 0x1d16a),
    (0, 0x1d173),
    (1, 0x1d183),
    (0, 0x1d185),
    (1, 0x1d18c),
    (0, 0x1d1aa),
    (1, 0x1d1ae),
    (0, 0x1d242),
    (1, 0x1d245),
    (2, 0x20000),
    (1, 0x2fffe),
    (2, 0x30000),
    (1, 0x3fffe),
    (0, 0xe0001),
    (1, 0xe0002),
    (0, 0xe0020),
    (1, 0xe0080),
    (0, 0xe0100),
    (1, 0xe01f0),
];

/// `sqlite3_qrf_ckformat()`: 0 = not a format, 1 = integer format, 2 = floating point format.
pub fn qrf_ckformat(z: &str) -> u8 {
    let b = z.as_bytes();
    if b.first() != Some(&b'%') {
        return 0;
    }
    let mut i = 1usize;
    let span = |i: usize, set: &[u8]| b[i..].iter().take_while(|c| set.contains(c)).count();
    let n = span(i, b"+-,#0");
    if n > 5 {
        return 0;
    }
    i += n;
    if b.get(i).is_some_and(|c| (b'1'..=b'9').contains(c)) {
        let n = span(i, b"0123456789");
        if n > 3 {
            return 0;
        }
        i += n;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let n = span(i, b"0123456789");
        if n > 3 {
            return 0;
        }
        i += n;
    }
    if span(i, b"eEgGf") == 1 && i + 1 == b.len() {
        return 2;
    }
    if span(i, b"dxXo") == 1 && i + 1 == b.len() {
        return 1;
    }
    0
}

#[inline]
fn at(z: &[u8], i: usize) -> u8 {
    z.get(i).copied().unwrap_or(0)
}

/// The bytes of `z` up to its first NUL (how the C code sees a `char*`).
#[inline]
pub fn cstr(z: &[u8]) -> &[u8] {
    match z.iter().position(|&c| c == 0) {
        Some(n) => &z[..n],
        None => z,
    }
}

fn qrf_space(c: u8) -> bool {
    matches!(c, 9..=13 | b' ')
}
fn qrf_digit(c: u8) -> bool {
    c.is_ascii_digit()
}
fn qrf_alpha(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
fn qrf_alnum(c: u8) -> bool {
    qrf_alpha(c) || qrf_digit(c)
}

/// `sqlite3_qrf_wcwidth()`.
pub fn qrf_wcwidth(c: i32) -> i32 {
    if c < 0x300 {
        return 1;
    }
    let c = c as u32;
    let mut i_first = 0usize;
    let mut i_last = QRF_UWIDTH.len() - 1;
    while i_first + 1 < i_last {
        let i_mid = (i_first + i_last) / 2;
        let c_mid = QRF_UWIDTH[i_mid].1;
        if c_mid < c {
            i_first = i_mid;
        } else if c_mid > c {
            i_last = i_mid - 1;
        } else {
            return i32::from(QRF_UWIDTH[i_mid].0);
        }
    }
    if QRF_UWIDTH[i_last].1 > c {
        return i32::from(QRF_UWIDTH[i_first].0);
    }
    i32::from(QRF_UWIDTH[i_last].0)
}

/// `sqlite3_qrf_decode_utf8()`: length and value of the multi-byte character at `z[i]`.
fn decode_utf8(z: &[u8], i: usize) -> (usize, i32) {
    let b0 = at(z, i);
    let b1 = at(z, i + 1);
    let b2 = at(z, i + 2);
    let b3 = at(z, i + 3);
    if (b0 & 0xe0) == 0xc0 && (b1 & 0xc0) == 0x80 {
        return (2, (i32::from(b0 & 0x1f) << 6) | i32::from(b1 & 0x3f));
    }
    if (b0 & 0xf0) == 0xe0 && (b1 & 0xc0) == 0x80 && (b2 & 0xc0) == 0x80 {
        return (
            3,
            (i32::from(b0 & 0x0f) << 12) | (i32::from(b1 & 0x3f) << 6) | i32::from(b2 & 0x3f),
        );
    }
    if (b0 & 0xf8) == 0xf0 && (b1 & 0xc0) == 0x80 && (b2 & 0xc0) == 0x80 && (b3 & 0xc0) == 0x80 {
        return (
            4,
            (i32::from(b0 & 0x0f) << 18)
                | (i32::from(b1 & 0x3f) << 12)
                | (i32::from(b2 & 0x3f) << 6)
                | i32::from(b3 & 0x3f),
        );
    }
    (1, 0)
}

/// `qrfIsVt100()`: length of the VT100 escape at `z[i]` (which is ESC), or 0.
fn is_vt100(z: &[u8], i: usize) -> usize {
    if at(z, i + 1) != b'[' {
        return 0;
    }
    let mut k = 2usize;
    while (0x30..=0x3f).contains(&at(z, i + k)) {
        k += 1;
    }
    while (0x20..=0x2f).contains(&at(z, i + k)) {
        k += 1;
    }
    let c = at(z, i + k);
    if !(0x40..=0x7e).contains(&c) {
        return 0;
    }
    k + 1
}

/// `sqlite3_qrf_wcswidth()` (the argument is a C string).
pub fn qrf_wcswidth(z_in: &[u8]) -> usize {
    let z = cstr(z_in);
    let mut n = 0usize;
    let mut i = 0usize;
    while i < z.len() {
        let c = z[i];
        if c < b' ' {
            let k = if c == 0x1b { is_vt100(z, i) } else { 0 };
            i += if k > 0 { k } else { 1 };
        } else if c & 0x80 == 0 {
            n += 1;
            i += 1;
        } else {
            let (len, u) = decode_utf8(z, i);
            i += len;
            n += qrf_wcwidth(u) as usize;
        }
    }
    n
}

/// `qrfDisplayWidth()`: width of the longest line of `z` and the number of line breaks.
fn display_width(z: &[u8]) -> (i32, i32) {
    let mut mx = 0i32;
    let mut n = 0i32;
    let mut n_nl = 0i32;
    let mut i = 0usize;
    while i < z.len() {
        let c = z[i];
        if c < b' ' {
            let k = if c == 0x1b { is_vt100(z, i) } else { 0 };
            if k > 0 {
                i += k;
            } else {
                if c == b'\t' {
                    n = (n + 8) & !7;
                } else if c == b'\n' || c == b'\r' {
                    n_nl += 1;
                    if n > mx {
                        mx = n;
                    }
                    n = 0;
                }
                i += 1;
            }
        } else if c & 0x80 == 0 {
            n += 1;
            i += 1;
        } else {
            let (len, u) = decode_utf8(z, i);
            i += len;
            n += qrf_wcwidth(u);
        }
    }
    if mx > n {
        n = mx;
    }
    (n, n_nl)
}

/// `qrfEscape()`: escape the control characters of `out[start..]` in place.
fn qrf_escape(e_esc: u8, out: &mut Vec<u8>, start: usize) {
    let z = &out[start..];
    let is_ctrl = |i: usize, c: u8| {
        c <= 0x1f && c != b'\t' && c != b'\n' && (c != b'\r' || at(z, i + 1) != b'\n')
    };
    if !z.iter().enumerate().any(|(i, &c)| is_ctrl(i, c)) {
        return;
    }
    let mut res = Vec::with_capacity(z.len() + 16);
    for (i, &c) in z.iter().enumerate() {
        if is_ctrl(i, c) {
            if e_esc == QRF_ESC_SYMBOL {
                res.extend_from_slice(&[0xe2, 0x90, 0x80 + c]);
            } else {
                res.push(b'^');
                res.push(0x40 + c);
            }
        } else {
            res.push(c);
        }
    }
    out.truncate(start);
    out.extend_from_slice(&res);
}

const QRF_CSV_QUOTE: fn(u8) -> bool = |c| c <= 0x20 || c == b'"' || c == b'\'' || c >= 0x7f;

fn sql_quote(z: &[u8], alt: bool) -> Vec<u8> {
    let v = Value::Text(String::from_utf8_lossy(z).into_owned());
    let fmt: &[u8] = if alt { b"%#Q" } else { b"%Q" };
    if std::str::from_utf8(z).is_ok()
        && let Ok(r) = sqliters::vdbe::printf::sqlite_printf(fmt, std::slice::from_ref(&v))
    {
        return r;
    }
    // Text that is not valid UTF-8: plain quoting of the raw bytes.
    let mut r = vec![b'\''];
    for &c in z {
        if c == b'\'' {
            r.push(b'\'');
        }
        r.push(c);
    }
    r.push(b'\'');
    r
}

struct ColData {
    n_col: usize,
    b_multi_row: bool,
    n_margin: i32,
    az: Vec<Vec<u8>>,
    ai_wth: Vec<i32>,
    ab_num: Vec<bool>,
    a: Vec<PerCol>,
}

#[derive(Clone, Default)]
struct PerCol {
    w: i32,
    mx_w: i32,
    e: u8,
    fx: bool,
    b_num: bool,
}

struct EqpRow {
    i_eqp_id: i32,
    i_parent_id: i32,
    text: Vec<u8>,
}

/// The source of rows: a prepared statement of this engine.
pub struct StmtSource<'s> {
    pub stmt: &'s mut Statement,
    /// The error of the last failed step, reported when the statement is reset.
    pub err: Option<SqliteError>,
}

impl StmtSource<'_> {
    fn step(&mut self) -> Option<Vec<Value>> {
        if self.err.is_some() {
            return None;
        }
        match self.stmt.step() {
            Ok(StepResult::Row(r)) => Some(r.into_values()),
            Ok(StepResult::Done) => None,
            Err(e) => {
                self.err = Some(e);
                None
            }
        }
    }
}

struct Qrf<'a, 's> {
    src: &'a mut StmtSource<'s>,
    names: Vec<Vec<u8>>,
    out: Vec<u8>,
    w: &'a mut dyn Write,
    i_err: Option<(i32, String)>,
    n_col: usize,
    mx_width: i32,
    mx_height: i32,
    n_row: i64,
    spec: QrfSpec,
    col_sep: Vec<u8>,
    row_sep: Vec<u8>,
    null: Vec<u8>,
    ifmt: Option<String>,
    fpfmt: Option<String>,
    line_cols: Option<(i32, Vec<Vec<u8>>)>,
    graph: Vec<EqpRow>,
    n_ins: u32,
    jtrans: Option<Connection>,
}

impl Qrf<'_, '_> {
    fn write(&mut self) {
        if !self.out.is_empty() {
            let _ = self.w.write_all(&self.out);
            self.out.clear();
        }
    }

    /// `qrfRelaxable()`.
    fn relaxable(&self, z: &[u8]) -> bool {
        let z0 = at(z, 0);
        if z0 == b'\'' || qrf_space(z0) {
            return false;
        }
        if z.is_empty() {
            return !self.null.is_empty();
        }
        let n = z.len();
        if z[n - 1] == b'\'' || qrf_space(z[n - 1]) {
            return false;
        }
        if self.null == z {
            return false;
        }
        let mut i = usize::from(z0 == b'-' || z0 == b'+');
        if &z[i..] == b"Inf" {
            return false;
        }
        if !qrf_digit(at(z, i)) {
            return true;
        }
        i += 1;
        while qrf_digit(at(z, i)) {
            i += 1;
        }
        if at(z, i) == 0 {
            return false;
        }
        if at(z, i) == b'.' {
            i += 1;
            while qrf_digit(at(z, i)) {
                i += 1;
            }
            if at(z, i) == 0 {
                return false;
            }
        }
        if at(z, i) == b'e' || at(z, i) == b'E' {
            i += 1;
            if at(z, i) == b'+' || at(z, i) == b'-' {
                i += 1;
            }
            if !qrf_digit(at(z, i)) {
                return true;
            }
            i += 1;
            while qrf_digit(at(z, i)) {
                i += 1;
            }
        }
        at(z, i) != 0
    }

    /// `qrfEncodeText()`: `z` is a C string (no NUL).
    fn encode_text(&self, out: &mut Vec<u8>, z: &[u8]) {
        let i_start = out.len();
        let mut e_text = self.spec.e_text;
        if e_text == QRF_TEXT_RELAXED {
            if self.relaxable(z) {
                out.extend_from_slice(z);
                e_text = 255;
            } else {
                e_text = QRF_TEXT_SQL;
            }
        }
        match e_text {
            255 => {}
            QRF_TEXT_SQL => {
                out.extend_from_slice(&sql_quote(z, self.spec.e_esc != QRF_ESC_OFF));
            }
            QRF_TEXT_CSV => {
                let mut quote = z.iter().any(|&c| QRF_CSV_QUOTE(c));
                // (`i==0` in the C code is also true for the empty string.)
                if z.is_empty() {
                    quote = true;
                }
                if !quote && !self.col_sep.is_empty() && find_sub(z, &self.col_sep).is_some() {
                    quote = true;
                }
                if self.col_sep.is_empty() {
                    // strstr(z, "") is always non-NULL.
                    quote = true;
                }
                if quote {
                    out.push(b'"');
                    for &c in z {
                        if c == b'"' {
                            out.push(b'"');
                        }
                        out.push(c);
                    }
                    out.push(b'"');
                } else {
                    out.extend_from_slice(z);
                }
            }
            QRF_TEXT_HTML => {
                for &c in z {
                    match c {
                        b'>' => out.extend_from_slice(b"&gt;"),
                        b'&' => out.extend_from_slice(b"&amp;"),
                        b'<' => out.extend_from_slice(b"&lt;"),
                        b'"' => out.extend_from_slice(b"&quot;"),
                        b'\'' => out.extend_from_slice(b"&#39;"),
                        _ => out.push(c),
                    }
                }
            }
            QRF_TEXT_TCL | QRF_TEXT_JSON => {
                out.push(b'"');
                for &c in z {
                    if c >= 0x20 && c != b'\\' && c != b'"' {
                        out.push(c);
                        continue;
                    }
                    match c {
                        b'"' => out.extend_from_slice(b"\\\""),
                        b'\\' => out.extend_from_slice(b"\\\\"),
                        0x08 => out.extend_from_slice(b"\\b"),
                        0x0c => out.extend_from_slice(b"\\f"),
                        b'\n' => out.extend_from_slice(b"\\n"),
                        b'\r' => out.extend_from_slice(b"\\r"),
                        b'\t' => out.extend_from_slice(b"\\t"),
                        _ => {
                            if e_text == QRF_TEXT_JSON {
                                out.extend_from_slice(format!("\\u{c:04x}").as_bytes());
                            } else {
                                out.extend_from_slice(format!("\\{c:03o}").as_bytes());
                            }
                        }
                    }
                }
                out.push(b'"');
            }
            _ => out.extend_from_slice(z),
        }
        if self.spec.e_esc != QRF_ESC_OFF {
            qrf_escape(self.spec.e_esc, out, i_start);
        }
    }

    /// `qrfJsonbToJson()`.
    fn jsonb_to_json(&mut self, blob: &[u8]) -> Option<Vec<u8>> {
        if !jsonb_quick_check(blob) {
            return None;
        }
        if self.jtrans.is_none() {
            self.jtrans = Connection::open_in_memory().ok();
        }
        let db = self.jtrans.as_ref()?;
        match db.query_row("SELECT json(?1)", &[Value::Blob(blob.to_vec())]) {
            Ok(Some(row)) => match row.get(0) {
                Some(Value::Text(t)) => Some(cstr(t.as_bytes()).to_vec()),
                _ => None,
            },
            _ => None,
        }
    }

    /// `qrfRenderValue()`. `to_main` tells that `out` is the main output buffer (the simple
    /// styles); the columnar styles render each cell into its own buffer.
    fn render_value(&mut self, out_cell: Option<&mut Vec<u8>>, v: &Value) {
        let mut tmp = Vec::new();
        let to_main = out_cell.is_none();
        let i_start_len;
        {
            let out: &mut Vec<u8> = if to_main { &mut tmp } else { out_cell.unwrap() };
            // For the main buffer the text is produced in `tmp` and appended afterwards, so
            // that `&mut self` stays usable; offsets are relative to the value's own start.
            i_start_len = out.len();
            match v {
                Value::Integer(i) => match &self.ifmt {
                    None => out.extend_from_slice(i.to_string().as_bytes()),
                    Some(f) => {
                        if let Ok(r) = sqliters::vdbe::printf::sqlite_printf(
                            f.as_bytes(),
                            std::slice::from_ref(v),
                        ) {
                            out.extend_from_slice(&r);
                        }
                    }
                },
                Value::Real(r) => match &self.fpfmt {
                    Some(f) => {
                        if let Ok(x) = sqliters::vdbe::printf::sqlite_printf(
                            f.as_bytes(),
                            std::slice::from_ref(v),
                        ) {
                            out.extend_from_slice(&x);
                        }
                    }
                    None => out.extend_from_slice(format_sqlite_real(*r).as_bytes()),
                },
                Value::Null => {
                    let n = self.null.clone();
                    out.extend_from_slice(&n);
                }
                Value::Text(t) => {
                    let z = cstr(t.as_bytes());
                    self.encode_text(out, z);
                }
                Value::Blob(b) => {
                    let mut done = false;
                    if self.spec.b_text_jsonb == QRF_YES
                        && let Some(js) = self.jsonb_to_json(b)
                    {
                        if self.spec.e_text == QRF_TEXT_SQL {
                            out.extend_from_slice(b"jsonb(");
                            self.encode_text(out, &js);
                            out.push(b')');
                        } else {
                            self.encode_text(out, &js);
                        }
                        done = true;
                    }
                    if !done {
                        const HEX: &[u8; 16] = b"0123456789abcdef";
                        match self.spec.e_blob {
                            QRF_BLOB_HEX | QRF_BLOB_SQL => {
                                if self.spec.e_blob == QRF_BLOB_SQL {
                                    out.extend_from_slice(b"x'");
                                }
                                for &c in b {
                                    out.push(HEX[usize::from(c >> 4)]);
                                    out.push(HEX[usize::from(c & 0xf)]);
                                }
                                if self.spec.e_blob == QRF_BLOB_SQL {
                                    out.push(b'\'');
                                }
                            }
                            QRF_BLOB_TCL | QRF_BLOB_JSON => {
                                out.push(b'"');
                                for &c in b {
                                    out.push(b'\\');
                                    if self.spec.e_blob == QRF_BLOB_TCL {
                                        out.push(b'0' + ((c >> 6) & 3));
                                        out.push(b'0' + ((c >> 3) & 7));
                                        out.push(b'0' + (c & 7));
                                    } else {
                                        out.extend_from_slice(b"u00");
                                        out.push(HEX[usize::from(c >> 4)]);
                                        out.push(HEX[usize::from(c & 0xf)]);
                                    }
                                }
                                out.push(b'"');
                            }
                            QRF_BLOB_SIZE => {
                                out.extend_from_slice(
                                    format!("({}-byte blob)", b.len()).as_bytes(),
                                );
                            }
                            _ => {
                                // qrfWrite(p); then the blob either goes straight to the
                                // output (no escaping) or is appended and the whole buffer is
                                // escaped from its first byte.
                                if to_main {
                                    // tmp holds only this value so far (nothing): emit the
                                    // pending main buffer first.
                                    self.write();
                                }
                                if b.is_empty() {
                                } else if self.spec.e_esc == QRF_ESC_OFF {
                                    let _ = self.w.write_all(b);
                                } else {
                                    out.extend_from_slice(b);
                                    qrf_escape(self.spec.e_esc, out, 0);
                                }
                            }
                        }
                    }
                }
            }
            if self.spec.n_char_limit > 0
                && (out.len() - i_start_len) as i64 > i64::from(self.spec.n_char_limit)
            {
                let mut limit = self.spec.n_char_limit;
                if limit < 4 {
                    limit = 4;
                }
                let z = &out[i_start_len..];
                let mut ii = 0usize;
                let mut w = 0i32;
                loop {
                    let c = at(z, ii);
                    if c < b' ' {
                        let k = if c == 0x1b { is_vt100(z, ii) } else { 0 };
                        if k > 0 {
                            ii += k;
                        } else if c == 0 {
                            break;
                        } else {
                            ii += 1;
                        }
                    } else if c & 0x80 == 0 {
                        w += 1;
                        if w > limit {
                            break;
                        }
                        ii += 1;
                    } else {
                        let (len, u) = decode_utf8(z, ii);
                        w += qrf_wcwidth(u);
                        if w > limit {
                            break;
                        }
                        ii += len;
                    }
                }
                if w > limit {
                    out.truncate(i_start_len + ii);
                    out.extend_from_slice(b"...");
                }
            }
        }
        if to_main {
            self.out.extend_from_slice(&tmp);
        }
    }

    fn eqp_append(&mut self, id: i32, parent: i32, text: &[u8]) {
        self.graph.push(EqpRow {
            i_eqp_id: id,
            i_parent_id: parent,
            text: text.to_vec(),
        });
    }

    fn eqp_render_level(&mut self, i_eqp_id: i32, prefix: &mut Vec<u8>) {
        let rows: Vec<usize> = (0..self.graph.len())
            .filter(|&i| self.graph[i].i_parent_id == i_eqp_id)
            .collect();
        let n = prefix.len();
        for (k, &ri) in rows.iter().enumerate() {
            let has_next = k + 1 < rows.len();
            self.out.extend_from_slice(prefix);
            self.out
                .extend_from_slice(if has_next { b"|--" } else { b"`--" });
            let t = self.graph[ri].text.clone();
            self.out.extend_from_slice(&t);
            self.out.push(b'\n');
            if n < 400 - 7 {
                prefix.extend_from_slice(if has_next { b"|  " } else { b"   " });
                let id = self.graph[ri].i_eqp_id;
                self.eqp_render_level(id, prefix);
                prefix.truncate(n);
            }
        }
    }

    /// `qrfEqpRender()` (without the scan-status variants).
    fn eqp_render(&mut self) {
        if self.graph.is_empty() {
            return;
        }
        if self.graph[0].text.first() == Some(&b'-') {
            if self.graph.len() == 1 {
                self.graph.clear();
                return;
            }
            let t = self.graph.remove(0).text;
            self.out.extend_from_slice(&t[3.min(t.len())..]);
            self.out.push(b'\n');
        } else {
            self.out.extend_from_slice(b"QUERY PLAN\n");
        }
        let mut prefix = Vec::new();
        self.eqp_render_level(0, &mut prefix);
        self.graph.clear();
    }

    /// `qrfSimpleTitle()`.
    fn simple_title(&mut self) {
        match self.spec.e_style {
            QRF_STYLE_HTML => {
                self.out.extend_from_slice(b"<tr>");
                for i in 0..self.n_col {
                    self.out.extend_from_slice(b"\n<th>");
                    let name = self.names[i].clone();
                    let mut o = std::mem::take(&mut self.out);
                    self.encode_text(&mut o, &name);
                    self.out = o;
                }
                self.out.extend_from_slice(b"\n</tr>\n");
            }
            QRF_STYLE_QUOTE | QRF_STYLE_LIST => {
                let saved = self.spec.e_text;
                self.spec.e_text = self.spec.e_title;
                for i in 0..self.n_col {
                    if i > 0 {
                        let s = self.col_sep.clone();
                        self.out.extend_from_slice(&s);
                    }
                    let name = self.names[i].clone();
                    let mut o = std::mem::take(&mut self.out);
                    self.encode_text(&mut o, &name);
                    self.out = o;
                }
                let s = self.row_sep.clone();
                self.out.extend_from_slice(&s);
                self.write();
                self.spec.e_text = saved;
            }
            _ => {}
        }
    }

    fn one_json_row(&mut self, vals: &[Value]) {
        for i in 0..self.n_col {
            if i > 0 {
                self.out.push(b',');
            }
            let name = self.names[i].clone();
            let mut o = std::mem::take(&mut self.out);
            self.encode_text(&mut o, &name);
            self.out = o;
            self.out.push(b':');
            self.render_value(None, &vals[i]);
        }
        self.write();
    }

    /// `qrfOneSimpleRow()`.
    fn one_simple_row(&mut self, vals: &[Value]) {
        match self.spec.e_style {
            QRF_STYLE_OFF | QRF_STYLE_COUNT => {}
            QRF_STYLE_JSON => {
                self.out
                    .extend_from_slice(if self.n_row == 0 { b"[{" } else { b"},\n{" });
                self.one_json_row(vals);
            }
            QRF_STYLE_JOBJECT => {
                self.out
                    .extend_from_slice(if self.n_row == 0 { b"{" } else { b"}\n{" });
                self.one_json_row(vals);
            }
            QRF_STYLE_HTML => {
                if self.n_row == 0 && self.spec.b_titles >= QRF_YES {
                    self.simple_title();
                }
                self.out.extend_from_slice(b"<tr>");
                for v in vals {
                    self.out.extend_from_slice(b"\n<td>");
                    self.render_value(None, v);
                }
                self.out.extend_from_slice(b"\n</tr>\n");
                self.write();
            }
            QRF_STYLE_INSERT => {
                let mx_ins = self.spec.n_multi_insert;
                let sz_start = self.out.len();
                if self.n_ins == 0 || self.n_ins >= mx_ins {
                    if self.n_ins != 0 {
                        self.out.extend_from_slice(b";\n");
                        self.n_ins = 0;
                    }
                    let tn = self.spec.z_table_name.clone().unwrap_or_default();
                    self.out.extend_from_slice(b"INSERT INTO ");
                    append_ident(&mut self.out, tn.as_bytes());
                    if self.spec.b_titles >= QRF_YES {
                        for i in 0..self.n_col {
                            self.out.push(if i == 0 { b'(' } else { b',' });
                            let name = self.names[i].clone();
                            append_ident(&mut self.out, &name);
                        }
                        self.out.push(b')');
                    }
                    self.out.extend_from_slice(b" VALUES(");
                } else {
                    self.out.extend_from_slice(b",\n  (");
                }
                // (Blob-as-text never occurs here: the insert style quotes blobs.)
                let mut written = 0usize;
                for (i, v) in vals.iter().enumerate() {
                    if i > 0 {
                        self.out.push(b',');
                    }
                    let before = self.out.len();
                    self.render_value(None, v);
                    written += self.out.len().saturating_sub(before);
                }
                let _ = written;
                self.n_ins = self
                    .n_ins
                    .wrapping_add((self.out.len() + 2 - sz_start) as u32);
                if self.n_ins >= mx_ins {
                    self.out.extend_from_slice(b");\n");
                    self.n_ins = 0;
                } else {
                    self.out.push(b')');
                }
                self.write();
            }
            QRF_STYLE_LINE => {
                if self.line_cols.is_none() {
                    let mut mx = 0i32;
                    let mut cols = Vec::with_capacity(self.n_col);
                    for i in 0..self.n_col {
                        let mut name = self.names[i].clone();
                        if self.spec.n_title_limit > 0 {
                            title_limit(&mut name, i32::from(self.spec.n_title_limit));
                        }
                        let sz = qrf_wcswidth(&name) as i32;
                        if sz > mx {
                            mx = sz;
                        }
                        cols.push(name);
                    }
                    self.line_cols = Some((mx, cols));
                }
                let (mx_col_wth, cols) = self.line_cols.clone().unwrap();
                if self.n_row != 0 {
                    self.out.push(b'\n');
                }
                let n_sep = self.col_sep.len() as i32;
                let mx_w = self.mx_width.saturating_sub(n_sep + mx_col_wth);
                let b_ww = self.spec.b_word_wrap == QRF_YES;
                for (i, v) in vals.iter().enumerate() {
                    width_print(&mut self.out, -mx_col_wth, &cols[i]);
                    let s = self.col_sep.clone();
                    self.out.extend_from_slice(&s);
                    let mut val = Vec::new();
                    // (A blob shown as text flushes the pending output first, like C.)
                    let pending = std::mem::take(&mut self.out);
                    let _ = self.w.write_all(&pending);
                    self.render_value(Some(&mut val), v);
                    let zv = cstr(&val).to_vec();
                    let mut z: &[u8] = &zv;
                    let mut cnt = 0i32;
                    loop {
                        let (mut n_this, _n_wide, mut i_next) = wrap_line(z, mx_w, b_ww);
                        if cnt != 0 {
                            self.out
                                .resize(self.out.len() + (mx_col_wth + n_sep) as usize, b' ');
                        }
                        cnt += 1;
                        let dots: &[u8] = b"...";
                        if cnt > self.mx_height {
                            z = dots;
                            n_this = 3;
                            i_next = 3;
                        }
                        self.out.extend_from_slice(&z[..n_this.min(z.len())]);
                        self.out.push(b'\n');
                        z = &z[i_next.min(z.len())..];
                        if z.is_empty() {
                            break;
                        }
                    }
                }
                self.write();
            }
            QRF_STYLE_EQP => {
                let line = match vals.get(3) {
                    Some(v) => value_text(v),
                    None => Vec::new(),
                };
                let id = vals.first().map_or(0, value_int);
                let parent = vals.get(1).map_or(0, value_int);
                if line.first() == Some(&b'-') {
                    self.eqp_render();
                }
                self.eqp_append(id, parent, &line);
            }
            _ => {
                if self.n_row == 0 && self.spec.b_titles >= QRF_YES {
                    self.simple_title();
                }
                for (i, v) in vals.iter().enumerate() {
                    if i > 0 {
                        let s = self.col_sep.clone();
                        self.out.extend_from_slice(&s);
                    }
                    self.render_value(None, v);
                }
                let s = self.row_sep.clone();
                self.out.extend_from_slice(&s);
                self.write();
            }
        }
        self.n_row += 1;
    }

    fn load_alignment(&self, data: &mut ColData) {
        for i in 0..data.n_col {
            data.a[i].e = self.spec.e_dflt_align;
            if i < self.spec.a_align.len() {
                let ax = self.spec.a_align[i];
                if ax & QRF_ALIGN_HMASK != 0 {
                    data.a[i].e = (ax & QRF_ALIGN_HMASK) | (data.a[i].e & QRF_ALIGN_VMASK);
                }
            } else if i < self.spec.a_width.len() && self.spec.a_width[i] < 0 {
                data.a[i].e = QRF_ALIGN_RIGHT | (data.a[i].e & QRF_ALIGN_VMASK);
            }
        }
    }

    /// `qrfSplitColumn()`.
    fn split_column(&self, data: &mut ColData) {
        let n = data.az.len();
        let nsw = i64::from(self.spec.n_screen_width);
        let valid_layout = |n_col: usize| -> Option<Vec<i32>> {
            let mut aw = vec![0i32; n_col];
            let nr = n.div_ceil(n_col);
            let mut w = 0i32;
            for i in 0..n {
                if i % nr == 0 {
                    if i > 0 {
                        aw[i / nr - 1] = w;
                    }
                    w = data.ai_wth[i];
                } else if data.ai_wth[i] > w {
                    w = data.ai_wth[i];
                }
            }
            aw[n_col - 1] = w;
            let t: i64 = aw.iter().map(|&x| i64::from(x)).sum::<i64>() + 2 * (n_col as i64 - 1);
            if t > nsw { None } else { Some(aw) }
        };
        let mut n_col = 1usize;
        let mut aw: Vec<i32> = Vec::new();
        let mut n_col_next = 2usize;
        let mut n_row = 1usize;
        loop {
            let Some(aw_new) = valid_layout(n_col_next) else {
                break;
            };
            aw = aw_new;
            n_col = n_col_next;
            n_row = n.div_ceil(n_col);
            if n_row == 1 {
                break;
            }
            n_col_next += 1;
            while n.div_ceil(n_col_next) == n_row {
                n_col_next += 1;
            }
        }
        if n_col == 1 {
            return;
        }
        let total = n_row * n_col;
        let mut az = vec![Vec::new(); total];
        let mut ai_wth = vec![0i32; total];
        let mut ab_num = vec![false; total];
        for i in 0..n {
            let j = (i % n_row) * n_col + (i / n_row);
            az[j] = std::mem::take(&mut data.az[i]);
            ab_num[j] = data.ab_num[i];
            ai_wth[j] = data.ai_wth[i];
        }
        let e0 = data.a[0].e;
        data.a = aw
            .iter()
            .map(|&w| PerCol {
                w,
                mx_w: w,
                e: e0,
                fx: w != 0,
                b_num: false,
            })
            .collect();
        data.az = az;
        data.ai_wth = ai_wth;
        data.ab_num = ab_num;
        data.n_col = n_col;
        let w: i32 = data.a.iter().map(|c| c.w).sum();
        data.n_margin = (i32::from(self.spec.n_screen_width) - w) / (n_col as i32 - 1);
        if data.n_margin > 5 {
            data.n_margin = 5;
        }
    }

    /// `qrfRestrictScreenWidth()`.
    fn restrict_screen_width(&self, data: &mut ColData) {
        data.n_margin = 2;
        if self.spec.n_screen_width == 0 {
            return;
        }
        let n_col = data.n_col as i32;
        let mut sep_w;
        if self.spec.e_style == QRF_STYLE_COLUMN {
            sep_w = n_col * 2 - 2;
        } else {
            sep_w = n_col * 3 + 1;
            if self.spec.b_border == QRF_NO {
                sep_w -= 2;
            }
        }
        let mut sum_w: i64 = 0;
        for c in &data.a {
            if sum_w > 2_147_483_647 - i64::from(c.w) {
                return;
            }
            sum_w += i64::from(c.w);
        }
        if i64::from(self.spec.n_screen_width) >= sum_w + i64::from(sep_w) {
            return;
        }
        data.n_margin = 0;
        if self.spec.e_style == QRF_STYLE_COLUMN {
            sep_w = n_col - 1;
        } else {
            sep_w = n_col + 1;
            if self.spec.b_border == QRF_NO {
                sep_w -= 2;
            }
        }
        let target_w = i64::from(i32::from(self.spec.n_screen_width) - sep_w);
        const MIN_SQUOZE: i32 = 8;
        const MIN_EX_SQUOZE: i32 = 16;
        while sum_w > target_w {
            let mut ix: Option<usize> = None;
            let mut mx = 0i32;
            for (i, c) in data.a.iter().enumerate() {
                let w = c.w;
                if !c.fx && w > mx && w > MIN_SQUOZE && (w > MIN_EX_SQUOZE || w * 2 > c.mx_w) {
                    ix = Some(i);
                    mx = w;
                }
            }
            let Some(ix) = ix else { break };
            let mut gain = if mx >= MIN_SQUOZE * 2 {
                mx / 2
            } else {
                mx - MIN_SQUOZE
            };
            if sum_w - i64::from(gain) < target_w {
                gain = (sum_w - target_w) as i32;
            }
            sum_w -= i64::from(gain);
            data.a[ix].w -= gain;
            data.b_multi_row = true;
        }
    }

    /// `qrfColumnar()`.
    fn columnar(&mut self) {
        let mut n_column = self.n_col;
        let mut row = self.src.step();
        if n_column == 0 {
            return;
        }
        if row.is_none() {
            if self.src.err.is_some() {
                return;
            }
            if self.spec.b_titles != QRF_ALWAYS {
                if self.spec.b_row_count == QRF_YES {
                    self.out.extend_from_slice(b"(0 rows)\n");
                    self.write();
                }
                return;
            }
        }
        let mut data = ColData {
            n_col: n_column,
            b_multi_row: false,
            n_margin: 0,
            az: Vec::new(),
            ai_wth: Vec::new(),
            ab_num: Vec::new(),
            a: vec![PerCol::default(); n_column],
        };
        if self.spec.b_titles >= QRF_YES {
            let saved = self.spec.e_text;
            self.spec.e_text = self.spec.e_title;
            for i in 0..n_column {
                let name = self.names[i].clone();
                let mut s = Vec::new();
                self.encode_text(&mut s, &name);
                let w;
                let mut n_nl = 0;
                if self.spec.n_title_limit != 0 {
                    w = title_limit(&mut s, i32::from(self.spec.n_title_limit));
                } else {
                    let (ww, nl) = display_width(&s);
                    w = ww;
                    n_nl = nl;
                }
                data.az.push(s);
                data.ai_wth.push(w);
                data.ab_num.push(false);
                if w > data.a[i].mx_w {
                    data.a[i].mx_w = w;
                }
                if n_nl != 0 {
                    data.b_multi_row = true;
                }
            }
            self.spec.e_text = saved;
        }
        while let Some(vals) = row {
            for (i, v) in vals.iter().enumerate().take(n_column) {
                let mut s = Vec::new();
                self.render_value(Some(&mut s), v);
                let (w, n_nl) = display_width(&s);
                // The cell is a C string from here on.
                let s = cstr(&s).to_vec();
                data.az.push(s);
                data.ab_num
                    .push(matches!(v, Value::Integer(_) | Value::Real(_)));
                data.ai_wth.push(w);
                if w > data.a[i].mx_w {
                    data.a[i].mx_w = w;
                }
                if n_nl != 0 {
                    data.b_multi_row = true;
                }
            }
            self.n_row += 1;
            row = self.src.step();
        }

        if self.spec.b_titles == QRF_NO {
            self.load_alignment(&mut data);
        } else {
            let e = if self.spec.e_title_align == QRF_AUTO {
                QRF_ALIGN_CENTER
            } else {
                self.spec.e_title_align
            };
            for c in &mut data.a {
                c.e = e;
            }
        }
        for i in 0..n_column {
            let mut w = 0i32;
            if i < self.spec.a_width.len() {
                w = i32::from(self.spec.a_width[i]);
                let auto_align =
                    self.spec.a_align.len() > i && (self.spec.a_align[i] & QRF_ALIGN_HMASK) == 0;
                if w == -32768 {
                    w = 0;
                    if auto_align {
                        data.a[i].e |= QRF_ALIGN_RIGHT;
                    }
                } else if w < 0 {
                    w = -w;
                    if auto_align {
                        data.a[i].e |= QRF_ALIGN_RIGHT;
                    }
                }
                if w != 0 {
                    data.a[i].fx = true;
                }
            }
            if w == 0 {
                w = data.a[i].mx_w;
                if self.spec.n_wrap > 0 && w > i32::from(self.spec.n_wrap) {
                    w = i32::from(self.spec.n_wrap);
                    data.b_multi_row = true;
                }
            } else if (!data.b_multi_row || w == 1) && data.a[i].mx_w > w {
                data.b_multi_row = true;
                if w == 1 {
                    w = 2;
                }
            }
            data.a[i].w = w;
        }

        if n_column == 1
            && data.az.len() > 1
            && self.spec.b_split_column == QRF_YES
            && self.spec.e_style == QRF_STYLE_COLUMN
            && self.spec.b_titles == QRF_NO
            && i32::from(self.spec.n_screen_width) > data.a[0].w + 3
        {
            self.split_column(&mut data);
            n_column = data.n_col;
        } else {
            self.restrict_screen_width(&mut data);
        }

        const BOX_13: &str = "\u{2502}";
        let border = self.spec.b_border != QRF_NO;
        let (row_start, col_sep, row_sep): (String, String, String);
        match self.spec.e_style {
            QRF_STYLE_BOX => {
                let (mut rs, cs, mut re) = if data.n_margin != 0 {
                    (
                        format!("{BOX_13} "),
                        format!(" {BOX_13} "),
                        format!(" {BOX_13}\n"),
                    )
                } else {
                    (BOX_13.to_owned(), BOX_13.to_owned(), format!("{BOX_13}\n"))
                };
                if !border {
                    rs = rs[3..].to_owned();
                    re = "\n".to_owned();
                } else {
                    box_separator(
                        &mut self.out,
                        &data,
                        border,
                        "\u{256d}",
                        "\u{252c}",
                        "\u{256e}",
                        false,
                    );
                }
                row_start = rs;
                col_sep = cs;
                row_sep = re;
            }
            QRF_STYLE_TABLE => {
                let (mut rs, cs, mut re) = if data.n_margin != 0 {
                    ("| ".to_owned(), " | ".to_owned(), " |\n".to_owned())
                } else {
                    ("|".to_owned(), "|".to_owned(), "|\n".to_owned())
                };
                if !border {
                    rs = rs[1..].to_owned();
                    re = "\n".to_owned();
                } else {
                    row_separator(&mut self.out, &data, border, b'+');
                }
                row_start = rs;
                col_sep = cs;
                row_sep = re;
            }
            QRF_STYLE_COLUMN => {
                row_start = String::new();
                col_sep = if data.n_margin < 2 {
                    " ".to_owned()
                } else if data.n_margin <= 5 {
                    " ".repeat(data.n_margin as usize)
                } else {
                    "     ".to_owned()
                };
                row_sep = "\n".to_owned();
            }
            _ => {
                if data.n_margin != 0 {
                    row_start = "| ".to_owned();
                    col_sep = " | ".to_owned();
                    row_sep = " |\n".to_owned();
                } else {
                    row_start = "|".to_owned();
                    col_sep = "|".to_owned();
                    row_sep = "|\n".to_owned();
                }
            }
        }

        let b_ww = self.spec.b_word_wrap == QRF_YES && data.b_multi_row;
        let b_rtrim = self.spec.e_style == QRF_STYLE_COLUMN
            || (!border
                && (self.spec.e_style == QRF_STYLE_BOX || self.spec.e_style == QRF_STYLE_TABLE));
        let n_cells = data.az.len();
        let mut i = 0usize;
        while i < n_cells {
            let mut pos = vec![0usize; n_column];
            for j in 0..n_column {
                data.a[j].b_num = data.ab_num[i + j];
            }
            let mut n_row = 0i32;
            let mut b_more;
            loop {
                self.out.extend_from_slice(row_start.as_bytes());
                b_more = false;
                for j in 0..n_column {
                    let z = &data.az[i + j][pos[j]..];
                    let (n_this, n_wide, i_next) = wrap_line(z, data.a[j].w, b_ww);
                    let n_ws = data.a[j].w - n_wide;
                    print_aligned(&mut self.out, &data.a[j], &z[..n_this.min(z.len())], n_ws);
                    pos[j] += i_next.min(z.len());
                    if pos[j] < data.az[i + j].len() {
                        b_more = true;
                    }
                    if j + 1 < n_column {
                        self.out.extend_from_slice(col_sep.as_bytes());
                    } else {
                        if b_rtrim {
                            rtrim(&mut self.out);
                        }
                        self.out.extend_from_slice(row_sep.as_bytes());
                    }
                }
                n_row += 1;
                if !(b_more && n_row < self.mx_height) {
                    break;
                }
            }
            if b_more {
                self.out.extend_from_slice(row_start.as_bytes());
                for j in 0..n_column {
                    if pos[j] >= data.az[i + j].len() {
                        self.out
                            .resize(self.out.len() + data.a[j].w.max(0) as usize, b' ');
                    } else {
                        let n_e = 3.min(data.a[j].w.max(0)) as usize;
                        print_aligned(
                            &mut self.out,
                            &data.a[j],
                            &b"..."[..n_e],
                            data.a[j].w - n_e as i32,
                        );
                    }
                    if j + 1 < n_column {
                        self.out.extend_from_slice(col_sep.as_bytes());
                    } else {
                        if b_rtrim {
                            rtrim(&mut self.out);
                        }
                        self.out.extend_from_slice(row_sep.as_bytes());
                    }
                }
            }
            if (i == 0 || data.b_multi_row) && i + n_column < n_cells {
                let is_title_sep = i == 0 && self.spec.b_titles >= QRF_YES;
                if is_title_sep {
                    self.load_alignment(&mut data);
                }
                match self.spec.e_style {
                    QRF_STYLE_TABLE => {
                        if is_title_sep || data.b_multi_row {
                            row_separator(&mut self.out, &data, border, b'+');
                        }
                    }
                    QRF_STYLE_BOX => {
                        if is_title_sep {
                            box_separator(
                                &mut self.out,
                                &data,
                                border,
                                "\u{255e}",
                                "\u{256a}",
                                "\u{2561}",
                                true,
                            );
                        } else if data.b_multi_row {
                            box_separator(
                                &mut self.out,
                                &data,
                                border,
                                "\u{251c}",
                                "\u{253c}",
                                "\u{2524}",
                                false,
                            );
                        }
                    }
                    QRF_STYLE_MARKDOWN => {
                        if is_title_sep {
                            row_separator(&mut self.out, &data, border, b'|');
                        }
                    }
                    QRF_STYLE_COLUMN => {
                        if is_title_sep {
                            column_separator(
                                &mut self.out,
                                &data,
                                col_sep.as_bytes(),
                                row_sep.as_bytes(),
                            );
                        } else if data.b_multi_row {
                            rtrim(&mut self.out);
                            self.out.push(b'\n');
                        }
                    }
                    _ => {}
                }
            }
            i += n_column;
        }
        if border || self.n_row == 0 {
            match self.spec.e_style {
                QRF_STYLE_BOX => {
                    if self.n_row > 0 {
                        box_separator(
                            &mut self.out,
                            &data,
                            border,
                            "\u{2570}",
                            "\u{2534}",
                            "\u{256f}",
                            false,
                        );
                    } else {
                        box_separator(
                            &mut self.out,
                            &data,
                            border,
                            "\u{2558}",
                            "\u{2567}",
                            "\u{255b}",
                            true,
                        );
                    }
                }
                QRF_STYLE_TABLE => row_separator(&mut self.out, &data, border, b'+'),
                QRF_STYLE_MARKDOWN => {
                    if self.n_row == 0 {
                        row_separator(&mut self.out, &data, border, b'|');
                    }
                }
                QRF_STYLE_COLUMN if self.n_row == 0 => {
                    column_separator(&mut self.out, &data, col_sep.as_bytes(), row_sep.as_bytes());
                }
                _ => {}
            }
        }
        if self.spec.b_row_count == QRF_YES {
            let n = self.n_row;
            self.out.extend_from_slice(
                format!("({n} row{})\n", if n == 1 { "" } else { "s" }).as_bytes(),
            );
        }
        self.write();
    }

    /// `qrfExplain()`: the opcode listing with indentation. (The rows are collected in the
    /// first pass instead of running the statement twice.)
    fn explain(&mut self) {
        const NEXT: [&str; 6] = ["Next", "Prev", "VPrev", "VNext", "SorterNext", "Return"];
        const YIELD: [&str; 5] = ["Yield", "SeekLT", "SeekGT", "RowSetRead", "Rewind"];
        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut ai_indent: Vec<i32> = Vec::new();
        let mut ab_yield: Vec<bool> = Vec::new();
        while let Some(vals) = self.src.step() {
            let i_op = rows.len() as i64;
            let i_addr = vals.first().map_or(0, value_int) as i64;
            let z_op = vals.get(1).map(value_text).unwrap_or_default();
            let z_op = String::from_utf8_lossy(&z_op).into_owned();
            let p1 = vals.get(2).map_or(0, value_int);
            let p2 = vals.get(3).map_or(0, value_int) as i64;
            let p2op = p2 + (i_op - i_addr);
            ab_yield.push(YIELD.contains(&z_op.as_str()));
            ai_indent.push(0);
            if NEXT.contains(&z_op.as_str()) && p2op > 0 {
                for k in p2op.max(0)..i_op {
                    ai_indent[k as usize] += 2;
                }
            }
            if z_op == "Goto" && p2op < i_op && p2op >= 0 && (ab_yield[p2op as usize] || p1 != 0) {
                for k in p2op..i_op {
                    ai_indent[k as usize] += 2;
                }
            }
            rows.push(vals);
        }
        const WIDTH: [i32; 8] = [4, 13, 4, 4, 4, 13, 2, 13];
        let n_arg = self.n_col.min(WIDTH.len());
        for (i_op, vals) in rows.iter().enumerate() {
            if i_op == 0 {
                for i in 0..n_arg {
                    let name = self.names[i].clone();
                    width_print(&mut self.out, WIDTH[i], &name);
                    self.out
                        .extend_from_slice(if i == n_arg - 1 { b"\n" } else { b"  " });
                }
                for (i, w) in WIDTH.iter().enumerate().take(n_arg) {
                    self.out.resize(self.out.len() + *w as usize, b'-');
                    self.out
                        .extend_from_slice(if i == n_arg - 1 { b"\n" } else { b"  " });
                }
            }
            for i in 0..n_arg {
                let mut sep: &[u8] = b"  ";
                let mut w = WIDTH[i];
                let val = value_text(&vals[i]);
                let val = cstr(&val);
                if i == n_arg - 1 {
                    w = 0;
                }
                let len = qrf_wcswidth(val) as i32;
                if len > w {
                    w = len;
                    sep = b" ";
                }
                if i == 1 {
                    self.out
                        .resize(self.out.len() + ai_indent[i_op].max(0) as usize, b' ');
                }
                width_print(&mut self.out, w, val);
                if i == n_arg - 1 {
                    self.out.push(b'\n');
                } else {
                    self.out.extend_from_slice(sep);
                }
            }
            self.n_row += 1;
        }
        self.write();
    }
}

fn find_sub(h: &[u8], n: &[u8]) -> Option<usize> {
    if n.is_empty() {
        return Some(0);
    }
    h.windows(n.len()).position(|w| w == n)
}

/// `sqlite3_column_text()` of a value.
pub fn value_text(v: &Value) -> Vec<u8> {
    match v {
        Value::Null => Vec::new(),
        Value::Integer(i) => i.to_string().into_bytes(),
        Value::Real(r) => format_sqlite_real(*r).into_bytes(),
        Value::Text(s) => cstr(s.as_bytes()).to_vec(),
        Value::Blob(b) => cstr(b).to_vec(),
    }
}

/// `sqlite3_column_int()` of a value.
pub fn value_int(v: &Value) -> i32 {
    match v {
        Value::Integer(i) => *i as i32,
        Value::Real(r) => *r as i64 as i32,
        Value::Text(s) => sqliters::types::fp::sqlite_atoi64(s.as_bytes()).0 as i32,
        _ => 0,
    }
}

/// `qrf_need_quote()` + the `"%w"` / plain choice of the insert style.
fn append_ident(out: &mut Vec<u8>, name: &[u8]) {
    let need = name.is_empty()
        || !qrf_alpha(name[0])
        || name.iter().any(|&c| !qrf_alnum(c))
        || std::str::from_utf8(name).is_ok_and(crate::util::keyword_check);
    if need {
        out.push(b'"');
        for &c in name {
            if c == b'"' {
                out.push(b'"');
            }
            out.push(c);
        }
        out.push(b'"');
    } else {
        out.extend_from_slice(name);
    }
}

/// `qrfJsonbQuickCheck()`.
fn jsonb_quick_check(a: &[u8]) -> bool {
    let n_blob = a.len();
    if n_blob == 0 {
        return false;
    }
    let x = usize::from(a[0] >> 4);
    if x <= 11 {
        return n_blob == 1 + x;
    }
    let n = if x < 14 { x - 11 } else { 4 * (x - 13) };
    if n_blob < 1 + n {
        return false;
    }
    let mut sz = u64::from(a[1]);
    for i in 1..n {
        sz = (sz << 8).wrapping_add(u64::from(a[i + 1]));
    }
    sz.wrapping_add(n as u64).wrapping_add(1) == n_blob as u64
}

/// `qrfTitleLimit()`: truncates the C string `z` in place and returns its display width.
fn title_limit(z: &mut Vec<u8>, n_lim: i32) -> i32 {
    let mut i = 0usize;
    let mut n = 0i32;
    let mut ellipsis: Option<usize> = None;
    let mut cut: Option<usize> = None;
    loop {
        let c = at(z, i);
        if c < b' ' {
            if c == 0 {
                ellipsis = None;
                break;
            }
            let k = if c == 0x1b { is_vt100(z, i) } else { 0 };
            if k > 0 {
                i += k;
            } else if c == b'\t' || c == b'\n' || c == b'\r' {
                z[i] = b' ';
            } else {
                i += 1;
            }
        } else if c & 0x80 == 0 {
            if n >= n_lim - 3 && ellipsis.is_none() {
                ellipsis = Some(i);
            }
            if n == n_lim {
                cut = Some(i);
                break;
            }
            n += 1;
            i += 1;
        } else {
            let (len, u) = decode_utf8(z, i);
            if n + len as i32 > n_lim - 3 && ellipsis.is_none() {
                ellipsis = Some(i);
            }
            if n + len as i32 > n_lim {
                cut = Some(i);
                break;
            }
            i += len;
            n += qrf_wcwidth(u);
        }
    }
    if let Some(c) = cut {
        z.truncate(c);
    }
    if let Some(e) = ellipsis
        && n_lim >= 3
    {
        z.truncate(e);
        z.extend_from_slice(b"...");
    }
    n
}

fn rtrim(out: &mut Vec<u8>) {
    while out.last() == Some(&b' ') {
        out.pop();
    }
}

/// `qrfWidthPrint()`.
pub fn width_print(out: &mut Vec<u8>, w: i32, z: &[u8]) {
    let a = cstr(z);
    let aw = w.unsigned_abs().min(10_000_000) as i32;
    let mut i = 0usize;
    let mut n = 0i32;
    while i < a.len() {
        let c = a[i];
        if (c & 0xc0) == 0xc0 {
            let (len, u) = decode_utf8(a, i);
            let x = qrf_wcwidth(u);
            if x + n > aw {
                break;
            }
            i += len;
            n += x;
        } else if c == 0x1b && is_vt100(a, i) > 0 {
            i += is_vt100(a, i);
        } else if n >= aw {
            break;
        } else {
            n += 1;
            i += 1;
        }
    }
    let i = i.min(a.len());
    if n >= aw {
        out.extend_from_slice(&a[..i]);
    } else if w < 0 {
        out.resize(out.len() + (aw - n) as usize, b' ');
        out.extend_from_slice(&a[..i]);
    } else {
        out.extend_from_slice(&a[..i]);
        out.resize(out.len() + (aw - n) as usize, b' ');
    }
}

/// `qrfWrapLine()`: (bytes for this line, display width of it, offset of the next line).
fn wrap_line(z: &[u8], w: i32, b_wrap: bool) -> (usize, i32, usize) {
    if z.is_empty() || z[0] == 0 {
        return (0, 0, 0);
    }
    let mut n = 0i32;
    let mut i = 0usize;
    let mut c = 0u8;
    while n <= w {
        c = at(z, i);
        if c >= 0xc0 {
            let (len, u) = decode_utf8(z, i);
            let wcw = qrf_wcwidth(u);
            if wcw + n > w {
                break;
            }
            i += len;
            n += wcw;
            continue;
        }
        if c >= b' ' {
            if n == w {
                break;
            }
            n += 1;
            i += 1;
            continue;
        }
        if c == 0 || c == b'\n' {
            break;
        }
        if c == b'\r' && at(z, i + 1) == b'\n' {
            i += 1;
            c = at(z, i);
            break;
        }
        if c == b'\t' {
            let wcw = 8 - (n & 7);
            if n + wcw > w {
                break;
            }
            n += wcw;
            i += 1;
            continue;
        }
        let k = if c == 0x1b { is_vt100(z, i) } else { 0 };
        if k > 0 {
            i += k;
        } else if n == w {
            break;
        } else {
            n += 1;
            i += 1;
        }
    }
    if c == 0 {
        return (i, n, i);
    }
    if c == b'\n' {
        return (i, n, i + 1);
    }
    if b_wrap && at(z, i) != 0 && !qrf_space(at(z, i)) && qrf_alnum(c) == qrf_alnum(at(z, i)) {
        let half = (i / 2) as i64;
        let mut k = i as i64 - 1;
        while k >= half {
            if qrf_space(at(z, k as usize)) {
                break;
            }
            k -= 1;
        }
        if k < half && half > 0 {
            k = i as i64;
            while k >= half {
                if qrf_alnum(at(z, (k - 1) as usize)) != qrf_alnum(at(z, k as usize))
                    && (at(z, k as usize) & 0xc0) != 0x80
                {
                    break;
                }
                k -= 1;
            }
        }
        if k >= half {
            i = k as usize;
            n = display_width(&z[..i.min(z.len())]).0;
        }
    }
    let n_this = i;
    while matches!(at(z, i), b' ' | b'\t' | b'\r') {
        i += 1;
    }
    (n_this, n, i)
}

/// `qrfAppendWithTabs()`.
fn append_with_tabs(out: &mut Vec<u8>, z: &[u8]) {
    let mut col = 0u32;
    let mut i = 0usize;
    while i < z.len() {
        let c = z[i];
        if c < b' ' {
            let k = if c == 0x1b { is_vt100(z, i) } else { 0 };
            if k > 0 && i + k <= z.len() {
                out.extend_from_slice(&z[i..i + k]);
                i += k;
            } else if c == b'\t' {
                let k = 8 - (col & 7);
                out.resize(out.len() + k as usize, b' ');
                col += k;
                i += 1;
            } else if c == b'\r' && i + 1 == z.len() {
                i += 1;
            } else {
                col += 1;
                out.extend_from_slice(&[0xe2, 0x90, 0x80 + c]);
                i += 1;
            }
        } else if c & 0x80 == 0 {
            out.push(c);
            i += 1;
            col += 1;
        } else {
            let (len, u) = decode_utf8(z, i);
            let end = (i + len).min(z.len());
            out.extend_from_slice(&z[i..end]);
            i = end;
            col += qrf_wcwidth(u) as u32;
        }
    }
}

/// `qrfPrintAligned()`.
fn print_aligned(out: &mut Vec<u8>, col: &PerCol, val: &[u8], n_ws: i32) {
    let n_ws = n_ws.max(0) as usize;
    let mut e = col.e & QRF_ALIGN_HMASK;
    if e == QRF_AUTO && col.b_num {
        e = QRF_ALIGN_RIGHT;
    }
    if e == QRF_ALIGN_CENTER {
        out.resize(out.len() + n_ws / 2, b' ');
        append_with_tabs(out, val);
        out.resize(out.len() + (n_ws - n_ws / 2), b' ');
    } else if e == QRF_ALIGN_RIGHT {
        out.resize(out.len() + n_ws, b' ');
        append_with_tabs(out, val);
    } else {
        append_with_tabs(out, val);
        out.resize(out.len() + n_ws, b' ');
    }
}

fn row_separator(out: &mut Vec<u8>, p: &ColData, border: bool, c_sep: u8) {
    if p.n_col > 0 {
        if border {
            out.push(c_sep);
        }
        out.resize(out.len() + (p.a[0].w + p.n_margin).max(0) as usize, b'-');
        for i in 1..p.n_col {
            out.push(c_sep);
            out.resize(out.len() + (p.a[i].w + p.n_margin).max(0) as usize, b'-');
        }
        if border {
            out.push(c_sep);
        }
    }
    out.push(b'\n');
}

fn box_separator(
    out: &mut Vec<u8>,
    p: &ColData,
    border: bool,
    s1: &str,
    s2: &str,
    s3: &str,
    dbl: bool,
) {
    let dash = if dbl { "\u{2550}" } else { "\u{2500}" };
    if p.n_col > 0 {
        if border {
            out.extend_from_slice(s1.as_bytes());
        }
        for i in 0..p.n_col {
            if i > 0 {
                out.extend_from_slice(s2.as_bytes());
            }
            for _ in 0..(p.a[i].w + p.n_margin).max(0) {
                out.extend_from_slice(dash.as_bytes());
            }
        }
        if border {
            out.extend_from_slice(s3.as_bytes());
        }
    }
    out.push(b'\n');
}

fn column_separator(out: &mut Vec<u8>, p: &ColData, col_sep: &[u8], row_sep: &[u8]) {
    for j in 0..p.n_col {
        out.resize(out.len() + p.a[j].w.max(0) as usize, b'-');
        if j + 1 < p.n_col {
            out.extend_from_slice(col_sep);
        } else {
            rtrim(out);
            out.extend_from_slice(row_sep);
        }
    }
}

/// `sqlite3_format_query_result()`: runs the statement and writes the formatted result.
/// `Err` carries the message that the C function leaves in `*pzErr` (the error of the
/// statement) and its result code.
pub fn format_query_result(
    stmt: &mut Statement,
    is_explain: u8,
    spec_in: &QrfSpec,
    w: &mut dyn Write,
) -> Result<(), SqliteError> {
    let names: Vec<Vec<u8>> = stmt
        .column_names()
        .iter()
        .map(|s| cstr(s.as_bytes()).to_vec())
        .collect();
    let n_col = stmt.column_count();
    let mut src = StmtSource { stmt, err: None };
    let mut spec = spec_in.clone();
    let mut null = spec.z_null.clone().unwrap_or_default().into_bytes();
    let mx_width = if spec.n_screen_width <= 0 {
        i32::MAX
    } else {
        i32::from(spec.n_screen_width)
    };
    let mx_height = if spec.n_line_limit <= 0 {
        i32::MAX
    } else {
        i32::from(spec.n_line_limit)
    };
    if spec.e_style > QRF_STYLE_TABLE {
        spec.e_style = QRF_AUTO;
    }
    if spec.e_esc > QRF_ESC_SYMBOL {
        spec.e_esc = QRF_AUTO;
    }
    if spec.e_text > QRF_TEXT_RELAXED {
        spec.e_text = QRF_AUTO;
    }
    if spec.e_title > QRF_TEXT_RELAXED {
        spec.e_title = QRF_AUTO;
    }
    if spec.e_blob > QRF_BLOB_SIZE {
        spec.e_blob = QRF_AUTO;
    }
    if spec
        .z_fpfmt
        .as_deref()
        .is_some_and(|f| qrf_ckformat(f) != 2)
    {
        spec.z_fpfmt = None;
    }
    if spec.e_style == QRF_AUTO {
        spec.e_style = match is_explain {
            0 => QRF_STYLE_BOX,
            1 => QRF_STYLE_EXPLAIN,
            _ => QRF_STYLE_EQP,
        };
    }
    let mut col_sep = spec.z_column_sep.clone();
    let mut row_sep = spec.z_row_sep.clone();
    match spec.e_style {
        QRF_STYLE_LIST => {
            col_sep.get_or_insert_with(|| "|".to_owned());
            row_sep.get_or_insert_with(|| "\n".to_owned());
        }
        QRF_STYLE_JOBJECT | QRF_STYLE_JSON => {
            spec.e_text = QRF_TEXT_JSON;
            null = b"null".to_vec();
            if spec.z_fpfmt.is_none() {
                spec.z_fpfmt = Some("%0.16g".to_owned());
            }
        }
        QRF_STYLE_HTML => {
            spec.e_text = QRF_TEXT_HTML;
            null = b"null".to_vec();
        }
        QRF_STYLE_INSERT => {
            spec.e_text = QRF_TEXT_SQL;
            null = b"NULL".to_vec();
            if spec.z_table_name.as_deref().is_none_or(str::is_empty) {
                spec.z_table_name = Some("tab".to_owned());
            }
            if spec.z_fpfmt.is_none() {
                spec.z_fpfmt = Some("%0.16g".to_owned());
            }
        }
        QRF_STYLE_LINE => {
            col_sep.get_or_insert_with(|| ": ".to_owned());
        }
        QRF_STYLE_CSV => {
            spec.e_style = QRF_STYLE_LIST;
            spec.e_text = QRF_TEXT_CSV;
            col_sep = Some(",".to_owned());
            row_sep = Some("\r\n".to_owned());
            null = Vec::new();
            if spec.e_esc == QRF_AUTO {
                spec.e_esc = QRF_ESC_OFF;
            }
        }
        QRF_STYLE_QUOTE => {
            spec.e_text = QRF_TEXT_SQL;
            null = b"NULL".to_vec();
            col_sep = Some(",".to_owned());
            row_sep = Some("\n".to_owned());
        }
        _ => {}
    }
    if spec.e_esc == QRF_AUTO {
        spec.e_esc = QRF_ESC_ASCII;
    }
    if spec.e_text == QRF_AUTO {
        spec.e_text = QRF_TEXT_PLAIN;
    }
    if spec.e_title == QRF_AUTO {
        spec.e_title = match spec.e_style {
            QRF_STYLE_BOX | QRF_STYLE_COLUMN | QRF_STYLE_TABLE => QRF_TEXT_PLAIN,
            _ => spec.e_text,
        };
    }
    if spec.e_blob == QRF_AUTO {
        spec.e_blob = match spec.e_text {
            QRF_TEXT_SQL => QRF_BLOB_SQL,
            QRF_TEXT_CSV | QRF_TEXT_TCL => QRF_BLOB_TCL,
            QRF_TEXT_JSON => QRF_BLOB_JSON,
            _ => QRF_BLOB_TEXT,
        };
    }
    if spec.b_titles == QRF_AUTO {
        spec.b_titles = match spec.e_style {
            QRF_STYLE_BOX | QRF_STYLE_CSV | QRF_STYLE_COLUMN | QRF_STYLE_TABLE
            | QRF_STYLE_MARKDOWN => QRF_YES,
            _ => QRF_NO,
        };
    }
    if spec.b_word_wrap == QRF_AUTO {
        spec.b_word_wrap = QRF_YES;
    }
    if spec.b_text_jsonb == QRF_AUTO {
        spec.b_text_jsonb = QRF_NO;
    }
    let ifmt = match spec.z_ifmt.as_deref() {
        Some(f) if qrf_ckformat(f) == 1 => {
            // "%5d" -> "%5lld"
            let (head, last) = f.split_at(f.len() - 1);
            Some(format!("{head}ll{last}"))
        }
        _ => None,
    };
    let style = spec.e_style;
    let b_titles = spec.b_titles;
    let b_row_count = spec.b_row_count;
    let mut qrf = Qrf {
        src: &mut src,
        names,
        out: Vec::new(),
        w,
        i_err: None,
        n_col,
        mx_width,
        mx_height,
        n_row: 0,
        col_sep: col_sep.unwrap_or_else(|| ",".to_owned()).into_bytes(),
        row_sep: row_sep.unwrap_or_else(|| "\n".to_owned()).into_bytes(),
        null,
        ifmt,
        fpfmt: spec.z_fpfmt.clone(),
        spec,
        line_cols: None,
        graph: Vec::new(),
        n_ins: 0,
        jtrans: None,
    };
    match style {
        QRF_STYLE_BOX | QRF_STYLE_COLUMN | QRF_STYLE_MARKDOWN | QRF_STYLE_TABLE => qrf.columnar(),
        QRF_STYLE_EXPLAIN => qrf.explain(),
        QRF_STYLE_STATSVM | QRF_STYLE_STATS | QRF_STYLE_STATSEST => {
            qrf.i_err = Some((1, "not available in this build".to_owned()));
        }
        _ => {
            while qrf.i_err.is_none() {
                let Some(vals) = qrf.src.step() else { break };
                qrf.one_simple_row(&vals);
            }
            if qrf.n_col > 0 && qrf.n_row == 0 && b_titles == QRF_ALWAYS {
                qrf.simple_title();
            }
            if qrf.n_col > 0 && b_row_count == QRF_YES {
                let n = qrf.n_row;
                let pl = if n == 1 { "" } else { "s" };
                match style {
                    QRF_STYLE_LINE | QRF_STYLE_CSV | QRF_STYLE_LIST | QRF_STYLE_QUOTE => {
                        if style == QRF_STYLE_LINE && n > 0 {
                            qrf.out.push(b'\n');
                        }
                        qrf.out
                            .extend_from_slice(format!("({n} row{pl})\n").as_bytes());
                    }
                    QRF_STYLE_HTML => qrf
                        .out
                        .extend_from_slice(format!("<!-- {n} row{pl} -->\n").as_bytes()),
                    QRF_STYLE_INSERT => {
                        qrf.out
                            .extend_from_slice(format!("/* {n} row{pl} inserted */\n").as_bytes());
                    }
                    _ => {}
                }
            }
        }
    }
    // qrfResetStmt + qrfFinalize
    match style {
        QRF_STYLE_COUNT => {
            let n = qrf.n_row;
            qrf.out.extend_from_slice(format!("{n}\n").as_bytes());
        }
        QRF_STYLE_JSON => {
            if qrf.n_row > 0 {
                qrf.out.extend_from_slice(b"}]\n");
            }
        }
        QRF_STYLE_JOBJECT => {
            if qrf.n_row > 0 {
                qrf.out.extend_from_slice(b"}\n");
            }
        }
        QRF_STYLE_INSERT => {
            if qrf.n_ins != 0 {
                qrf.out.extend_from_slice(b";\n");
            }
        }
        QRF_STYLE_EQP => qrf.eqp_render(),
        _ => {}
    }
    qrf.write();
    let own_err = qrf.i_err.take();
    drop(qrf);
    if let Some(e) = src.err.take() {
        return Err(e);
    }
    if let Some((_, msg)) = own_err {
        return Err(SqliteError::new(sqliters::ErrorCode::Error, msg));
    }
    Ok(())
}
