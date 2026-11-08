//! TCL canonical list parsing/formatting, SQLite `fpnum_compare` (`test1.c`),
//! and RFC 1321 MD5 (`md5sum` / `md5`) in safe zero-dependency Rust.

/// Parses a TCL list string into its element strings.
pub fn parse_list(input: &str) -> Result<Vec<String>, String> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut out = Vec::new();

    while i < len {
        while i < len && is_tcl_list_space(bytes[i]) {
            i += 1;
        }
        if i >= len {
            break;
        }

        if bytes[i] == b'{' {
            i += 1;
            let start = i;
            let mut depth = 1usize;
            let mut has_bs_newline = false;
            while i < len && depth > 0 {
                if bytes[i] == b'\\' && i + 1 < len {
                    if bytes[i + 1] == b'\n' {
                        has_bs_newline = true;
                    }
                    i += 2;
                    continue;
                }
                if bytes[i] == b'{' {
                    depth += 1;
                } else if bytes[i] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            if depth != 0 {
                return Err("unmatched open brace in list".to_string());
            }
            let raw = &input[start..i];
            i += 1; // consume '}'
            if has_bs_newline {
                out.push(collapse_backslash_newlines(raw));
            } else {
                out.push(raw.to_string());
            }
        } else if bytes[i] == b'"' {
            i += 1;
            let mut elem = String::new();
            let mut closed = false;
            while i < len {
                if bytes[i] == b'"' {
                    closed = true;
                    i += 1;
                    break;
                } else if bytes[i] == b'\\' {
                    let (ch, next_i) = parse_backslash_escape(input, i);
                    elem.push_str(&ch);
                    i = next_i;
                } else {
                    let ch = input[i..].chars().next().unwrap();
                    elem.push(ch);
                    i += ch.len_utf8();
                }
            }
            if !closed {
                return Err("unmatched open quote in list".to_string());
            }
            out.push(elem);
        } else {
            let mut elem = String::new();
            while i < len && !is_tcl_list_space(bytes[i]) {
                if bytes[i] == b'\\' {
                    let (ch, next_i) = parse_backslash_escape(input, i);
                    elem.push_str(&ch);
                    i = next_i;
                } else {
                    let ch = input[i..].chars().next().unwrap();
                    elem.push(ch);
                    i += ch.len_utf8();
                }
            }
            out.push(elem);
        }
    }

    Ok(out)
}

/// Formats a slice of strings into a canonical TCL list string (`[list ...]`).
pub fn format_list<S: AsRef<str>>(items: &[S]) -> String {
    let mut out = String::new();
    for (idx, item) in items.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        out.push_str(&format_list_elem(item.as_ref()));
    }
    out
}

/// Formats a single element for a canonical TCL list.
pub fn format_list_elem(s: &str) -> String {
    if s.is_empty() {
        return "{}".to_string();
    }

    let needs_quoting = s.starts_with('#')
        || s.bytes().any(|b| {
            is_tcl_list_space(b)
                || matches!(b, b'{' | b'}' | b'[' | b']' | b'$' | b';' | b'"' | b'\\')
        });

    if !needs_quoting {
        return s.to_string();
    }

    if can_brace_elem(s) {
        let mut res = String::with_capacity(s.len() + 2);
        res.push('{');
        res.push_str(s);
        res.push('}');
        return res;
    }

    let mut res = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '{' | '}' | '[' | ']' | '$' | ';' | '"' | '\\' | ' ' => {
                res.push('\\');
                res.push(ch);
            }
            '\n' => res.push_str("\\n"),
            '\r' => res.push_str("\\r"),
            '\t' => res.push_str("\\t"),
            '\x07' => res.push_str("\\a"),
            '\x08' => res.push_str("\\b"),
            '\x0c' => res.push_str("\\f"),
            '\x0b' => res.push_str("\\v"),
            '#' if res.is_empty() => res.push_str("\\#"),
            other => res.push(other),
        }
    }
    res
}

fn can_brace_elem(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            if i + 1 >= bytes.len() {
                // Trailing backslash would escape closing brace
                return false;
            }
            if bytes[i + 1] == b'\n' {
                // Backslash-newline gets collapsed even inside braces in TCL
                return false;
            }
            i += 2;
            continue;
        }
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
            if depth < 0 {
                return false;
            }
        }
        i += 1;
    }
    depth == 0
}

#[inline]
pub fn is_tcl_list_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

pub fn collapse_backslash_newlines(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
            out.push(' ');
            i += 2;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Parses a TCL backslash escape starting at `input[idx] == '\\'`.
/// Returns `(replacement, next_byte_index)`.
pub fn parse_backslash_escape(input: &str, idx: usize) -> (String, usize) {
    let bytes = input.as_bytes();
    if idx + 1 >= bytes.len() {
        return ("\\".to_string(), idx + 1);
    }
    match bytes[idx + 1] {
        b'a' => ("\x07".to_string(), idx + 2),
        b'b' => ("\x08".to_string(), idx + 2),
        b'f' => ("\x0c".to_string(), idx + 2),
        b'n' => ("\n".to_string(), idx + 2),
        b'r' => ("\r".to_string(), idx + 2),
        b't' => ("\t".to_string(), idx + 2),
        b'v' => ("\x0b".to_string(), idx + 2),
        b'\n' => {
            let mut j = idx + 2;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            (" ".to_string(), j)
        }
        b'x' => {
            let mut j = idx + 2;
            let mut val = 0u32;
            let mut count = 0;
            while j < bytes.len() && count < 2 && bytes[j].is_ascii_hexdigit() {
                val = val * 16 + hex_val(bytes[j]);
                j += 1;
                count += 1;
            }
            if count == 0 {
                ("x".to_string(), idx + 2)
            } else {
                let ch = char::from_u32(val).unwrap_or('\u{FFFD}');
                (ch.to_string(), j)
            }
        }
        b'u' => {
            let mut j = idx + 2;
            let mut val = 0u32;
            let mut count = 0;
            while j < bytes.len() && count < 4 && bytes[j].is_ascii_hexdigit() {
                val = val * 16 + hex_val(bytes[j]);
                j += 1;
                count += 1;
            }
            if count == 0 {
                ("u".to_string(), idx + 2)
            } else {
                let ch = char::from_u32(val).unwrap_or('\u{FFFD}');
                (ch.to_string(), j)
            }
        }
        b'0'..=b'7' => {
            let mut j = idx + 1;
            let mut val = 0u32;
            let mut count = 0;
            while j < bytes.len() && count < 3 && (b'0'..=b'7').contains(&bytes[j]) {
                val = val * 8 + u32::from(bytes[j] - b'0');
                j += 1;
                count += 1;
            }
            let ch = char::from_u32(val & 0xff).unwrap_or('\0');
            (ch.to_string(), j)
        }
        _ => {
            let ch = input[idx + 1..].chars().next().unwrap();
            (ch.to_string(), idx + 1 + ch.len_utf8())
        }
    }
}

fn hex_val(b: u8) -> u32 {
    match b {
        b'0'..=b'9' => u32::from(b - b'0'),
        b'a'..=b'f' => u32::from(b - b'a' + 10),
        b'A'..=b'F' => u32::from(b - b'A' + 10),
        _ => 0,
    }
}

/// Port of SQLite's `fpnum_compare` (`sqlite/src/test1.c:6298`).
pub fn fpnum_compare(a: &str, b: &str) -> bool {
    let mut za = a.as_bytes().to_vec();
    let mut zb = b.as_bytes().to_vec();
    za.push(0);
    zb.push(0);
    let mut i = 0usize;
    let mut j = 0usize;

    loop {
        while za[i].is_ascii_whitespace() {
            i += 1;
        }
        while zb[j].is_ascii_whitespace() {
            j += 1;
        }

        if za[i] != zb[j] {
            break;
        }
        if za[i] == b'-' && za[i + 1].is_ascii_digit() {
            i += 1;
            j += 1;
        }
        if !za[i].is_ascii_digit() {
            while !za[i].is_ascii_whitespace() && za[i] != 0 && za[i] == zb[j] {
                i += 1;
                j += 1;
            }
            if za[i] != zb[j] {
                break;
            }
            if za[i].is_ascii_whitespace() {
                continue;
            }
            break;
        }

        let mut ndigit = 0usize;
        while za[i] == zb[j] && za[i].is_ascii_digit() {
            i += 1;
            j += 1;
            ndigit += 1;
        }
        if za[i] != zb[j] {
            break;
        }
        if za[i] == 0 {
            break;
        }
        if za[i] == b'.' && zb[j] == b'.' {
            i += 1;
            j += 1;
            while za[i] == zb[j] && za[i].is_ascii_digit() {
                i += 1;
                j += 1;
                ndigit += 1;
            }
            if za[i] == 0 {
                while zb[j] == b'0' || (zb[j].is_ascii_digit() && ndigit >= 15) {
                    j += 1;
                    ndigit += 1;
                }
                break;
            }
            if zb[j] == 0 {
                while za[i] == b'0' || (za[i].is_ascii_digit() && ndigit >= 15) {
                    i += 1;
                    ndigit += 1;
                }
                break;
            }
            if za[i].is_ascii_whitespace() && zb[j].is_ascii_whitespace() {
                continue;
            }

            if za[i].is_ascii_digit() && zb[j].is_ascii_digit() {
                if za[i] == zb[j] + 1 && !za[i + 1].is_ascii_digit() && zb[j + 1].is_ascii_digit() {
                    j += 1;
                    while zb[j] == b'9' {
                        j += 1;
                        ndigit += 1;
                    }
                    if ndigit < 14 && (!zb[j].is_ascii_digit() || zb[j] < b'5') {
                        break;
                    }
                    while zb[j].is_ascii_digit() {
                        j += 1;
                    }
                    i += 1;
                } else if zb[j] == za[i] + 1
                    && !zb[j + 1].is_ascii_digit()
                    && za[i + 1].is_ascii_digit()
                {
                    i += 1;
                    while za[i] == b'9' {
                        i += 1;
                        ndigit += 1;
                    }
                    if ndigit < 14 && (!za[i].is_ascii_digit() || za[i] < b'5') {
                        break;
                    }
                    while za[i].is_ascii_digit() {
                        i += 1;
                    }
                    j += 1;
                } else {
                    break;
                }
            } else if !za[i].is_ascii_digit() && zb[j].is_ascii_digit() {
                while zb[j] == b'0' {
                    j += 1;
                    ndigit += 1;
                }
                if ndigit < 15 {
                    break;
                }
                while zb[j].is_ascii_digit() {
                    j += 1;
                }
            } else if !zb[j].is_ascii_digit() && za[i].is_ascii_digit() {
                while za[i] == b'0' {
                    i += 1;
                    ndigit += 1;
                }
                if ndigit < 15 {
                    break;
                }
                while za[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                break;
            }
        }
        if (za[i] == b'e' || za[i] == b'E') && (zb[j] == b'e' || zb[j] == b'E') {
            i += 1;
            j += 1;
            if (za[i] == b'+' || za[i] == b'-') && zb[j] == za[i] {
                i += 1;
                j += 1;
            }
            if za[i] != zb[j] {
                if za[i] == b'0' && za[i + 1] == zb[j] {
                    i += 1;
                }
                if zb[j] == b'0' && zb[j + 1] == za[i] {
                    j += 1;
                }
            }
            while za[i] == zb[j] && za[i].is_ascii_digit() {
                i += 1;
                j += 1;
            }
            if za[i] != zb[j] {
                break;
            }
            if za[i] == 0 {
                break;
            }
            continue;
        }
    }

    while za[i].is_ascii_whitespace() {
        i += 1;
    }
    while zb[j].is_ascii_whitespace() {
        j += 1;
    }
    za[i] == 0 && zb[j] == 0
}

/// Pure safe Rust RFC 1321 MD5 digest (hex lowercase), matching SQLite's `test_md5.c`.
pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            let off = i * 4;
            *w = u32::from_le_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
        }
        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | ((!b) & d), i),
                16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());

    let mut hex = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}
