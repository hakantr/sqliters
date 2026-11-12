//! The built-in FTS5 tokenizers `unicode61`, `ascii`, `porter` and `trigram`: a port of
//! `sqlite/ext/fts5/fts5_tokenize.c`.
//!
//! The terms a tokenizer emits are stored in the index, so every function here follows the
//! C code byte for byte (including its treatment of code points the C tables do not know,
//! the byte-wise Porter stemmer and its length limits).
#![forbid(unsafe_code)]

use crate::fts5_unicode::{cat_parse, is_diacritic, unicode_ascii, unicode_category, unicode_fold};

/// Tokens longer than this many bytes are truncated by the FTS5 core before they are
/// indexed or queried (`FTS5_MAX_TOKEN_SIZE`, fts5Int.h).
pub(crate) const FTS5_MAX_TOKEN_SIZE: usize = 32768;

/// Tokens larger than this (in bytes) are passed through the Porter stemmer unchanged
/// (`FTS5_PORTER_MAX_TOKEN`).
const FTS5_PORTER_MAX_TOKEN: usize = 64;

/// `aAsciiTokenChar`: default token characters of the `ascii` tokenizer.
const fn ascii_default_token_chars() -> [u8; 128] {
    let mut t = [0u8; 128];
    let mut i = 0usize;
    while i < 128 {
        let c = i as u8;
        if c.is_ascii_digit() || c.is_ascii_uppercase() || c.is_ascii_lowercase() {
            t[i] = 1;
        }
        i += 1;
    }
    t
}

/// Why a tokenizer could not be instantiated (`sqlite3Fts5LoadTokenizer`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenizerError {
    /// `no such tokenizer: <name>`
    NoSuch(String),
    /// `error in tokenizer constructor`
    Constructor,
}

/// `Fts5TokenizerConfig.ePattern`: whether LIKE / GLOB can be answered by the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenizerPattern {
    None,
    Like,
    Glob,
}

#[derive(Debug, Clone)]
pub(crate) struct Unicode61 {
    token_char: [u8; 128],
    remove_diacritic: u8,
    /// Sorted code points whose token/separator status is inverted (`aiException`).
    exceptions: Vec<u32>,
    category: [u8; 32],
}

#[derive(Debug, Clone)]
pub(crate) struct AsciiTokenizer {
    token_char: [u8; 128],
}

#[derive(Debug, Clone)]
pub(crate) enum Tokenizer {
    Unicode61(Box<Unicode61>),
    Ascii(Box<AsciiTokenizer>),
    Porter(Box<Tokenizer>),
    Trigram { fold: bool, fold_param: u8 },
}

/// `FTS5_READ_UTF8`: reads one code point at `*pos`, advancing it.
#[inline]
fn read_utf8(text: &[u8], pos: &mut usize) -> u32 {
    const TRANS1: [u8; 64] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        0x0d, 0x0e, 0x0f, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x00, 0x01, 0x02, 0x03,
        0x00, 0x01, 0x00, 0x00,
    ];
    let mut c = u32::from(text[*pos]);
    *pos += 1;
    if c >= 0xc0 {
        c = u32::from(TRANS1[(c - 0xc0) as usize]);
        while *pos < text.len() && (text[*pos] & 0xc0) == 0x80 {
            c = (c << 6).wrapping_add(u32::from(0x3f & text[*pos]));
            *pos += 1;
        }
        if c < 0x80 || (c & 0xFFFF_F800) == 0xD800 || (c & 0xFFFF_FFFE) == 0xFFFE {
            c = 0xFFFD;
        }
    }
    c
}

/// `FTS5_WRITE_UTF8`.
#[inline]
fn write_utf8(out: &mut Vec<u8>, c: u32) {
    if c < 0x80 {
        out.push((c & 0xFF) as u8);
    } else if c < 0x800 {
        out.push(0xC0 + ((c >> 6) & 0x1F) as u8);
        out.push(0x80 + (c & 0x3F) as u8);
    } else if c < 0x10000 {
        out.push(0xE0 + ((c >> 12) & 0x0F) as u8);
        out.push(0x80 + ((c >> 6) & 0x3F) as u8);
        out.push(0x80 + (c & 0x3F) as u8);
    } else {
        out.push(0xF0 + ((c >> 18) & 0x07) as u8);
        out.push(0x80 + ((c >> 12) & 0x3F) as u8);
        out.push(0x80 + ((c >> 6) & 0x3F) as u8);
        out.push(0x80 + (c & 0x3F) as u8);
    }
}

impl Unicode61 {
    /// `fts5UnicodeCreate`.
    fn create(args: &[String]) -> Option<Self> {
        if !args.len().is_multiple_of(2) {
            return None;
        }
        let mut p = Self {
            token_char: [0u8; 128],
            remove_diacritic: 1,
            exceptions: Vec::new(),
            category: [0u8; 32],
        };
        let mut cat = "L* N* Co";
        for pair in args.chunks(2) {
            if pair[0].eq_ignore_ascii_case("categories") {
                cat = pair[1].as_str();
            }
        }
        if !p.set_categories(cat.as_bytes()) {
            return None;
        }
        for pair in args.chunks(2) {
            let arg = pair[1].as_bytes();
            if pair[0].eq_ignore_ascii_case("remove_diacritics") {
                if arg.len() != 1 || !matches!(arg[0], b'0' | b'1' | b'2') {
                    return None;
                }
                p.remove_diacritic = arg[0] - b'0';
            } else if pair[0].eq_ignore_ascii_case("tokenchars") {
                p.add_exceptions(arg, true);
            } else if pair[0].eq_ignore_ascii_case("separators") {
                p.add_exceptions(arg, false);
            } else if pair[0].eq_ignore_ascii_case("categories") {
                // handled above
            } else {
                return None;
            }
        }
        Some(p)
    }

    /// `unicodeSetCategories`.
    fn set_categories(&mut self, cat: &[u8]) -> bool {
        let mut i = 0usize;
        while i < cat.len() {
            while i < cat.len() && (cat[i] == b' ' || cat[i] == b'\t') {
                i += 1;
            }
            if i < cat.len() && !cat_parse(&cat[i..], &mut self.category) {
                return false;
            }
            while i < cat.len() && cat[i] != b' ' && cat[i] != b'\t' {
                i += 1;
            }
        }
        unicode_ascii(&self.category, &mut self.token_char);
        true
    }

    /// `fts5UnicodeAddExceptions`.
    fn add_exceptions(&mut self, z: &[u8], token_chars: bool) {
        let mut pos = 0usize;
        while pos < z.len() {
            let code = read_utf8(z, &mut pos);
            if code < 128 {
                self.token_char[code as usize] = u8::from(token_chars);
            } else {
                let is_token = self.category[unicode_category(code)] != 0;
                if is_token != token_chars && !is_diacritic(code) {
                    let i = self
                        .exceptions
                        .iter()
                        .position(|&e| e > code)
                        .unwrap_or(self.exceptions.len());
                    self.exceptions.insert(i, code);
                }
            }
        }
    }

    /// `fts5UnicodeIsAlnum`.
    #[inline]
    fn is_alnum(&self, code: u32) -> bool {
        let base = self.category[unicode_category(code)] != 0;
        if self.exceptions.is_empty() {
            base
        } else {
            base ^ self.exceptions.binary_search(&code).is_ok()
        }
    }

    /// `fts5UnicodeTokenize`.
    fn tokenize(&self, text: &[u8], cb: &mut dyn FnMut(&[u8], usize, usize)) {
        let a = &self.token_char;
        let n = text.len();
        let mut csr = 0usize;
        let mut fold: Vec<u8> = Vec::with_capacity(64);
        loop {
            fold.clear();
            let start;
            let mut end = 0usize;
            // Skip any separator characters; `pending` carries a non-ASCII token character
            // that was already read from the input.
            let mut pending: Option<u32> = None;
            loop {
                if csr >= n {
                    return;
                }
                let b = text[csr];
                if b & 0x80 != 0 {
                    let is = csr;
                    let code = read_utf8(text, &mut csr);
                    if self.is_alnum(code) {
                        start = is;
                        pending = Some(code);
                        break;
                    }
                } else {
                    if a[b as usize] != 0 {
                        start = csr;
                        break;
                    }
                    csr += 1;
                }
            }
            if let Some(code) = pending {
                let folded = unicode_fold(code, self.remove_diacritic);
                if folded != 0 {
                    write_utf8(&mut fold, folded);
                }
                end = csr;
            } else {
                // Fast path with the same result: a token made only of ASCII token characters
                // without upper-case letters is its own folded form.
                let mut j = csr;
                let mut plain = true;
                while j < n {
                    let b = text[j];
                    if b & 0x80 != 0 || b.is_ascii_uppercase() {
                        plain = false;
                        break;
                    }
                    if a[b as usize] == 0 {
                        break;
                    }
                    j += 1;
                }
                if plain {
                    csr = j;
                    cb(&text[start..j], start, j);
                    continue;
                }
            }
            // Run through the token characters, folding them into the output buffer.
            while csr < n {
                let b = text[csr];
                if b & 0x80 != 0 {
                    let code = read_utf8(text, &mut csr);
                    if self.is_alnum(code) || is_diacritic(code) {
                        let folded = unicode_fold(code, self.remove_diacritic);
                        if folded != 0 {
                            write_utf8(&mut fold, folded);
                        }
                    } else {
                        break;
                    }
                } else if a[b as usize] == 0 {
                    break;
                } else {
                    fold.push(if b.is_ascii_uppercase() { b + 32 } else { b });
                    csr += 1;
                }
                end = csr;
            }
            cb(&fold, start, end);
        }
    }
}

impl AsciiTokenizer {
    /// `fts5AsciiCreate`.
    fn create(args: &[String]) -> Option<Self> {
        if !args.len().is_multiple_of(2) {
            return None;
        }
        let mut p = Self {
            token_char: ascii_default_token_chars(),
        };
        for pair in args.chunks(2) {
            let token_chars = if pair[0].eq_ignore_ascii_case("tokenchars") {
                true
            } else if pair[0].eq_ignore_ascii_case("separators") {
                false
            } else {
                return None;
            };
            for &b in pair[1].as_bytes() {
                if b & 0x80 == 0 {
                    p.token_char[b as usize] = u8::from(token_chars);
                }
            }
        }
        Some(p)
    }

    /// `fts5AsciiTokenize`.
    fn tokenize(&self, text: &[u8], cb: &mut dyn FnMut(&[u8], usize, usize)) {
        let a = &self.token_char;
        let n = text.len();
        let mut is = 0usize;
        let mut fold: Vec<u8> = Vec::with_capacity(64);
        while is < n {
            while is < n && (text[is] & 0x80) == 0 && a[text[is] as usize] == 0 {
                is += 1;
            }
            if is == n {
                break;
            }
            let mut ie = is + 1;
            while ie < n && ((text[ie] & 0x80) != 0 || a[text[ie] as usize] != 0) {
                ie += 1;
            }
            fold.clear();
            fold.extend(text[is..ie].iter().map(
                |&c| {
                    if c.is_ascii_uppercase() { c + 32 } else { c }
                },
            ));
            cb(&fold, is, ie);
            is = ie + 1;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Porter stemmer (`fts5PorterCb` and its generated step functions)
// ---------------------------------------------------------------------------------------------

#[inline]
fn porter_is_vowel(c: u8, y_is_vowel: bool) -> bool {
    matches!(c, b'a' | b'e' | b'i' | b'o' | b'u') || (y_is_vowel && c == b'y')
}

/// `fts5PorterGobbleVC`.
fn porter_gobble_vc(stem: &[u8], prev_cons: bool) -> usize {
    let n = stem.len();
    let mut cons = prev_cons;
    let mut i = 0usize;
    // Scan for a vowel
    while i < n {
        cons = !porter_is_vowel(stem[i], cons);
        if !cons {
            break;
        }
        i += 1;
    }
    // Scan for a consonant
    i += 1;
    while i < n {
        cons = !porter_is_vowel(stem[i], cons);
        if cons {
            return i + 1;
        }
        i += 1;
    }
    0
}

/// porter rule condition: (m > 0)
fn porter_m_gt0(stem: &[u8]) -> bool {
    porter_gobble_vc(stem, false) != 0
}

/// porter rule condition: (m > 1)
fn porter_m_gt1(stem: &[u8]) -> bool {
    let n = porter_gobble_vc(stem, false);
    n != 0 && porter_gobble_vc(&stem[n..], true) != 0
}

/// porter rule condition: (m = 1)
fn porter_m_eq1(stem: &[u8]) -> bool {
    let n = porter_gobble_vc(stem, false);
    n != 0 && porter_gobble_vc(&stem[n..], true) == 0
}

/// porter rule condition: (*o)
fn porter_ostar(stem: &[u8]) -> bool {
    let Some(&last) = stem.last() else {
        return false;
    };
    if matches!(last, b'w' | b'x' | b'y') {
        return false;
    }
    let mut mask = 0u32;
    let mut cons = false;
    for &c in stem {
        cons = !porter_is_vowel(c, cons);
        mask = ((mask << 1) + u32::from(cons)) & 0x0007;
    }
    mask == 0x0005
}

/// porter rule condition: (m > 1 and (*S or *T))
fn porter_m_gt1_and_s_or_t(stem: &[u8]) -> bool {
    matches!(stem.last(), Some(b's' | b't')) && porter_m_gt1(stem)
}

/// porter rule condition: (*v*)
fn porter_vowel(stem: &[u8]) -> bool {
    stem.iter()
        .enumerate()
        .any(|(i, &c)| porter_is_vowel(c, i > 0))
}

/// The byte two positions before the end of the buffer (`aBuf[nBuf-2]`); the C code reads
/// the byte in front of the buffer when one byte is left, which is never a letter.
#[inline]
fn porter_key(buf: &[u8]) -> u8 {
    if buf.len() >= 2 {
        buf[buf.len() - 2]
    } else {
        0
    }
}

/// `nBuf>len(suffix) && 0==memcmp(suffix, &aBuf[nBuf-len], len)`
#[inline]
fn porter_ends(buf: &[u8], suffix: &[u8]) -> bool {
    buf.len() > suffix.len() && buf.ends_with(suffix)
}

/// One rule of steps 2 and 3: when `buf` ends with `suffix` (and is longer than it) the
/// rule is selected; the suffix is replaced by `out` if the stem has m > 0. Returns true
/// when the rule was selected (no other rule of the same `case` is tried).
fn porter_rule_m_gt0(buf: &mut Vec<u8>, suffix: &[u8], out: &[u8]) -> bool {
    if !porter_ends(buf, suffix) {
        return false;
    }
    let stem = buf.len() - suffix.len();
    if porter_m_gt0(&buf[..stem]) {
        buf.truncate(stem);
        buf.extend_from_slice(out);
    }
    true
}

/// One rule of step 4: remove `suffix` if the stem has m > 1.
fn porter_rule_m_gt1(buf: &mut Vec<u8>, suffix: &[u8]) -> bool {
    if !porter_ends(buf, suffix) {
        return false;
    }
    let stem = buf.len() - suffix.len();
    if porter_m_gt1(&buf[..stem]) {
        buf.truncate(stem);
    }
    true
}

/// `fts5PorterStep1A`.
fn porter_step1a(buf: &mut Vec<u8>) {
    let n = buf.len();
    if buf[n - 1] == b's' {
        if buf[n - 2] == b'e' {
            if (n > 4 && buf[n - 4] == b's' && buf[n - 3] == b's') || (n > 3 && buf[n - 3] == b'i')
            {
                buf.truncate(n - 2);
            } else {
                buf.truncate(n - 1);
            }
        } else if buf[n - 2] != b's' {
            buf.truncate(n - 1);
        }
    }
}

/// `fts5PorterStep1B`.
fn porter_step1b(buf: &mut Vec<u8>) -> bool {
    match porter_key(buf) {
        b'e' => {
            if porter_ends(buf, b"eed") {
                let stem = buf.len() - 3;
                if porter_m_gt0(&buf[..stem]) {
                    buf.truncate(stem + 2);
                }
            } else if porter_ends(buf, b"ed") {
                let stem = buf.len() - 2;
                if porter_vowel(&buf[..stem]) {
                    buf.truncate(stem);
                    return true;
                }
            }
        }
        b'n' if porter_ends(buf, b"ing") => {
            let stem = buf.len() - 3;
            if porter_vowel(&buf[..stem]) {
                buf.truncate(stem);
                return true;
            }
        }
        _ => {}
    }
    false
}

/// `fts5PorterStep1B2`.
fn porter_step1b2(buf: &mut Vec<u8>) -> bool {
    let hit = match porter_key(buf) {
        b'a' => porter_ends(buf, b"at"),
        b'b' => porter_ends(buf, b"bl"),
        b'i' => porter_ends(buf, b"iz"),
        _ => false,
    };
    if hit {
        buf.push(b'e');
    }
    hit
}

/// `fts5PorterStep2`.
fn porter_step2(buf: &mut Vec<u8>) {
    match porter_key(buf) {
        b'a' => {
            let _ = porter_rule_m_gt0(buf, b"ational", b"ate")
                || porter_rule_m_gt0(buf, b"tional", b"tion");
        }
        b'c' => {
            let _ = porter_rule_m_gt0(buf, b"enci", b"ence")
                || porter_rule_m_gt0(buf, b"anci", b"ance");
        }
        b'e' => {
            let _ = porter_rule_m_gt0(buf, b"izer", b"ize");
        }
        b'g' => {
            let _ = porter_rule_m_gt0(buf, b"logi", b"log");
        }
        b'l' => {
            let _ = porter_rule_m_gt0(buf, b"bli", b"ble")
                || porter_rule_m_gt0(buf, b"alli", b"al")
                || porter_rule_m_gt0(buf, b"entli", b"ent")
                || porter_rule_m_gt0(buf, b"eli", b"e")
                || porter_rule_m_gt0(buf, b"ousli", b"ous");
        }
        b'o' => {
            let _ = porter_rule_m_gt0(buf, b"ization", b"ize")
                || porter_rule_m_gt0(buf, b"ation", b"ate")
                || porter_rule_m_gt0(buf, b"ator", b"ate");
        }
        b's' => {
            let _ = porter_rule_m_gt0(buf, b"alism", b"al")
                || porter_rule_m_gt0(buf, b"iveness", b"ive")
                || porter_rule_m_gt0(buf, b"fulness", b"ful")
                || porter_rule_m_gt0(buf, b"ousness", b"ous");
        }
        b't' => {
            let _ = porter_rule_m_gt0(buf, b"aliti", b"al")
                || porter_rule_m_gt0(buf, b"iviti", b"ive")
                || porter_rule_m_gt0(buf, b"biliti", b"ble");
        }
        _ => {}
    }
}

/// `fts5PorterStep3`.
fn porter_step3(buf: &mut Vec<u8>) {
    match porter_key(buf) {
        b'a' => {
            let _ = porter_rule_m_gt0(buf, b"ical", b"ic");
        }
        b's' => {
            let _ = porter_rule_m_gt0(buf, b"ness", b"");
        }
        b't' => {
            let _ =
                porter_rule_m_gt0(buf, b"icate", b"ic") || porter_rule_m_gt0(buf, b"iciti", b"ic");
        }
        b'u' => {
            let _ = porter_rule_m_gt0(buf, b"ful", b"");
        }
        b'v' => {
            let _ = porter_rule_m_gt0(buf, b"ative", b"");
        }
        b'z' => {
            let _ = porter_rule_m_gt0(buf, b"alize", b"al");
        }
        _ => {}
    }
}

/// `fts5PorterStep4`.
fn porter_step4(buf: &mut Vec<u8>) {
    match porter_key(buf) {
        b'a' => {
            let _ = porter_rule_m_gt1(buf, b"al");
        }
        b'c' => {
            let _ = porter_rule_m_gt1(buf, b"ance") || porter_rule_m_gt1(buf, b"ence");
        }
        b'e' => {
            let _ = porter_rule_m_gt1(buf, b"er");
        }
        b'i' => {
            let _ = porter_rule_m_gt1(buf, b"ic");
        }
        b'l' => {
            let _ = porter_rule_m_gt1(buf, b"able") || porter_rule_m_gt1(buf, b"ible");
        }
        b'n' => {
            let _ = porter_rule_m_gt1(buf, b"ant")
                || porter_rule_m_gt1(buf, b"ement")
                || porter_rule_m_gt1(buf, b"ment")
                || porter_rule_m_gt1(buf, b"ent");
        }
        b'o' => {
            if porter_ends(buf, b"ion") {
                let stem = buf.len() - 3;
                if porter_m_gt1_and_s_or_t(&buf[..stem]) {
                    buf.truncate(stem);
                }
            } else {
                let _ = porter_rule_m_gt1(buf, b"ou");
            }
        }
        b's' => {
            let _ = porter_rule_m_gt1(buf, b"ism");
        }
        b't' => {
            let _ = porter_rule_m_gt1(buf, b"ate") || porter_rule_m_gt1(buf, b"iti");
        }
        b'u' => {
            let _ = porter_rule_m_gt1(buf, b"ous");
        }
        b'v' => {
            let _ = porter_rule_m_gt1(buf, b"ive");
        }
        b'z' => {
            let _ = porter_rule_m_gt1(buf, b"ize");
        }
        _ => {}
    }
}

/// `fts5PorterCb`: stems `token` into `buf`; returns false when the token is passed through
/// unchanged (shorter than 3 or longer than 64 bytes).
fn porter_stem(token: &[u8], buf: &mut Vec<u8>) -> bool {
    if token.len() > FTS5_PORTER_MAX_TOKEN || token.len() < 3 {
        return false;
    }
    buf.clear();
    buf.extend_from_slice(token);

    // Step 1.
    porter_step1a(buf);
    if porter_step1b(buf) && !porter_step1b2(buf) {
        let n = buf.len();
        let c = buf[n - 1];
        let prev = if n >= 2 { buf[n - 2] } else { 0 };
        if !porter_is_vowel(c, false) && c != b'l' && c != b's' && c != b'z' && c == prev {
            buf.truncate(n - 1);
        } else if porter_m_eq1(buf) && porter_ostar(buf) {
            buf.push(b'e');
        }
    }

    // Step 1C.
    let n = buf.len();
    if buf[n - 1] == b'y' && porter_vowel(&buf[..n - 1]) {
        buf[n - 1] = b'i';
    }

    // Steps 2 through 4.
    porter_step2(buf);
    porter_step3(buf);
    porter_step4(buf);

    // Step 5a.
    let n = buf.len();
    if buf[n - 1] == b'e'
        && (porter_m_gt1(&buf[..n - 1])
            || (porter_m_eq1(&buf[..n - 1]) && !porter_ostar(&buf[..n - 1])))
    {
        buf.truncate(n - 1);
    }

    // Step 5b.
    let n = buf.len();
    if n > 1 && buf[n - 1] == b'l' && buf[n - 2] == b'l' && porter_m_gt1(&buf[..n - 1]) {
        buf.truncate(n - 1);
    }
    true
}

/// `fts5TriTokenize`.
fn trigram_tokenize(
    fold: bool,
    fold_param: u8,
    text: &[u8],
    cb: &mut dyn FnMut(&[u8], usize, usize),
) {
    let n = text.len();
    let mut pos = 0usize;
    let mut buf: Vec<u8> = Vec::with_capacity(16);
    let mut starts = [0usize; 3];
    // Populate the buffer with the characters for the first trigram.
    for start in &mut starts {
        loop {
            *start = pos;
            if pos >= n {
                return;
            }
            let mut code = read_utf8(text, &mut pos);
            if fold {
                code = unicode_fold(code, fold_param);
            }
            if code != 0 {
                write_utf8(&mut buf, code);
                break;
            }
        }
    }
    loop {
        // Read characters from the input up until the first non-diacritic.
        let mut next;
        let mut code;
        loop {
            next = pos;
            if pos >= n {
                code = 0;
                break;
            }
            code = read_utf8(text, &mut pos);
            if fold {
                code = unicode_fold(code, fold_param);
            }
            if code != 0 {
                break;
            }
        }
        cb(&buf, starts[0], next);
        if code == 0 {
            break;
        }
        // Remove the first character from the buffer, append the new one.
        let mut first_len = 1usize;
        if buf[0] >= 0xc0 {
            while first_len < buf.len() && (buf[first_len] & 0xc0) == 0x80 {
                first_len += 1;
            }
        }
        buf.drain(..first_len);
        write_utf8(&mut buf, code);
        starts[0] = starts[1];
        starts[1] = starts[2];
        starts[2] = next;
    }
}

impl Tokenizer {
    /// `sqlite3Fts5LoadTokenizer`: `args[0]` is the tokenizer name (default `unicode61`),
    /// the rest are its arguments.
    pub(crate) fn load(args: &[String]) -> std::result::Result<Self, TokenizerError> {
        let Some(name) = args.first() else {
            return Unicode61::create(&[])
                .map(|u| Self::Unicode61(Box::new(u)))
                .ok_or(TokenizerError::Constructor);
        };
        Self::create_named(name, &args[1..])
            .ok_or_else(|| TokenizerError::NoSuch(name.clone()))?
            .ok_or(TokenizerError::Constructor)
    }

    /// Outer `None`: unknown tokenizer name; inner `None`: the constructor failed.
    fn create_named(name: &str, args: &[String]) -> Option<Option<Self>> {
        if name.eq_ignore_ascii_case("unicode61") {
            Some(Unicode61::create(args).map(|u| Self::Unicode61(Box::new(u))))
        } else if name.eq_ignore_ascii_case("ascii") {
            Some(AsciiTokenizer::create(args).map(|a| Self::Ascii(Box::new(a))))
        } else if name.eq_ignore_ascii_case("trigram") {
            Some(Self::create_trigram(args))
        } else if name.eq_ignore_ascii_case("porter") {
            Some(Self::create_porter(args))
        } else {
            None
        }
    }

    /// `fts5PorterCreate`.
    fn create_porter(args: &[String]) -> Option<Self> {
        let mut rest = args;
        let mut base = "unicode61";
        while let Some(first) = rest.first() {
            if first.eq_ignore_ascii_case("porter") {
                rest = &rest[1..];
            } else {
                base = first.as_str();
                break;
            }
        }
        let base_args = if rest.is_empty() { rest } else { &rest[1..] };
        let inner = Self::create_named(base, base_args)??;
        Some(Self::Porter(Box::new(inner)))
    }

    /// `fts5TriCreate`.
    fn create_trigram(args: &[String]) -> Option<Self> {
        if !args.len().is_multiple_of(2) {
            return None;
        }
        let mut fold = true;
        let mut fold_param = 0u8;
        for pair in args.chunks(2) {
            let arg = pair[1].as_bytes();
            if pair[0].eq_ignore_ascii_case("case_sensitive") {
                if arg.len() != 1 || !matches!(arg[0], b'0' | b'1') {
                    return None;
                }
                fold = arg[0] == b'0';
            } else if pair[0].eq_ignore_ascii_case("remove_diacritics") {
                if arg.len() != 1 || !matches!(arg[0], b'0' | b'1' | b'2') {
                    return None;
                }
                fold_param = if arg[0] != b'0' { 2 } else { 0 };
            } else {
                return None;
            }
        }
        if fold_param != 0 && !fold {
            return None;
        }
        Some(Self::Trigram { fold, fold_param })
    }

    /// `sqlite3Fts5TokenizerPattern`.
    pub(crate) fn pattern(&self) -> TokenizerPattern {
        match self {
            Self::Trigram {
                fold,
                fold_param: 0,
            } => {
                if *fold {
                    TokenizerPattern::Like
                } else {
                    TokenizerPattern::Glob
                }
            }
            _ => TokenizerPattern::None,
        }
    }

    /// Invokes `cb(token, start_byte, end_byte)` for every token of `text`, in order.
    pub(crate) fn tokenize(&self, text: &[u8], cb: &mut dyn FnMut(&[u8], usize, usize)) {
        match self {
            Self::Unicode61(u) => u.tokenize(text, cb),
            Self::Ascii(a) => a.tokenize(text, cb),
            Self::Trigram { fold, fold_param } => trigram_tokenize(*fold, *fold_param, text, cb),
            Self::Porter(inner) => {
                let mut buf: Vec<u8> = Vec::with_capacity(FTS5_PORTER_MAX_TOKEN + 64);
                inner.tokenize(text, &mut |tok, s, e| {
                    if porter_stem(tok, &mut buf) {
                        cb(&buf, s, e);
                    } else {
                        cb(tok, s, e);
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(spec: &[&str], text: &str) -> Vec<String> {
        let args: Vec<String> = spec.iter().map(|s| (*s).to_owned()).collect();
        let t = Tokenizer::load(&args).expect("tokenizer");
        let mut out = Vec::new();
        t.tokenize(text.as_bytes(), &mut |tok, _, _| {
            out.push(String::from_utf8_lossy(tok).into_owned());
        });
        out
    }

    // Expected values below were produced by C SQLite 3.54.1 (`fts5vocab` of a one-row table).
    #[test]
    fn unicode61_matches_c_classification_and_folding() {
        // default is remove_diacritics=1; İ folds to "i", ſ to "s", µ to μ; ø and đ are kept
        assert_eq!(
            toks(&[], "Ünïcödé straße İstanbul søren đak µm ſtop x² ½cup"),
            [
                "unicode", "straße", "istanbul", "søren", "đak", "μm", "stop", "x²", "½cup"
            ]
        );
        assert_eq!(
            toks(&["unicode61", "remove_diacritics", "2"], "søren đak łódź ǖ"),
            ["søren", "đak", "łodz", "u"]
        );
        assert_eq!(
            toks(
                &["unicode61", "remove_diacritics", "0"],
                "Ünïcödé cafe\u{301}"
            ),
            ["ünïcödé", "cafe\u{301}"]
        );
        // symbols / emoji are separators; other numbers (①) and private-use code points are
        // token characters
        assert_eq!(
            toks(&[], "a😀b ①x ㎏ \u{e000}z"),
            ["a", "b", "①x", "\u{e000}z"]
        );
    }

    #[test]
    fn porter_matches_c_edge_cases() {
        assert_eq!(
            toks(
                &["porter"],
                "eed feed agreed ies ties sses caresses yying studying aed 1ing"
            ),
            [
                "e", "feed", "agre", "ie", "ti", "sse", "caress", "y", "studi", "a", "1ing"
            ]
        );
        // tokens over 64 bytes are not stemmed
        let long = "a".repeat(62) + "ing";
        assert_eq!(toks(&["porter"], &long), std::slice::from_ref(&long));
    }

    #[test]
    fn trigram_and_ascii_match_c() {
        assert_eq!(toks(&["trigram"], "AbÇd"), ["abç", "bçd"]);
        assert_eq!(
            toks(&["trigram", "case_sensitive", "1"], "AbCd"),
            ["AbC", "bCd"]
        );
        assert_eq!(
            toks(&["trigram", "remove_diacritics", "1"], "éa\u{301}bc"),
            ["eab", "abc"]
        );
        assert_eq!(toks(&["ascii"], "Hello, Wörld-9"), ["hello", "wörld", "9"]);
        assert!(matches!(
            Tokenizer::load(&["nosuch".to_owned()]),
            Err(TokenizerError::NoSuch(_))
        ));
        assert!(matches!(
            Tokenizer::load(&["unicode61".to_owned(), "remove_diacritics".to_owned()]),
            Err(TokenizerError::Constructor)
        ));
    }
}
