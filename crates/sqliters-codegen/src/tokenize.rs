//! `tokenize.c`: the SQL tokenizer (`sqlite3GetToken`) and the loop feeding tokens to the
//! LALR(1) parser (`sqlite3RunParser`).

use crate::consts::*;
use crate::parse::YyParser;
use crate::sqliteint::*;
use crate::tk::*;
use crate::util::{is_digit, is_space, is_xdigit};

// Character classes for tokenizing (`CC_*`).
const CC_X: u8 = 0; // The letter 'x', or start of BLOB literal
const CC_KYWD0: u8 = 1; // First letter of a keyword
const CC_KYWD: u8 = 2; // Alphabetics or '_'.  Usable in a keyword
const CC_DIGIT: u8 = 3; // Digits
const CC_DOLLAR: u8 = 4; // '$'
const CC_VARALPHA: u8 = 5; // '@', '#', ':'.  Alphabetic SQL variables
const CC_VARNUM: u8 = 6; // '?'.  Numeric SQL variables
const CC_SPACE: u8 = 7; // Space characters
const CC_QUOTE: u8 = 8; // '"', '\'', or '`'.  String literals, quoted ids
const CC_QUOTE2: u8 = 9; // '['.   [...] style quoted ids
const CC_PIPE: u8 = 10; // '|'.   Bitwise OR or concatenate
const CC_MINUS: u8 = 11; // '-'.  Minus or SQL-style comment
const CC_LT: u8 = 12; // '<'.  Part of < or <= or <>
const CC_GT: u8 = 13; // '>'.  Part of > or >=
const CC_EQ: u8 = 14; // '='.  Part of = or ==
const CC_BANG: u8 = 15; // '!'.  Part of !=
const CC_SLASH: u8 = 16; // '/'.  / or c-style comment
const CC_LP: u8 = 17; // '('
const CC_RP: u8 = 18; // ')'
const CC_SEMI: u8 = 19; // ';'
const CC_PLUS: u8 = 20; // '+'
const CC_STAR: u8 = 21; // '*'
const CC_PERCENT: u8 = 22; // '%'
const CC_COMMA: u8 = 23; // ','
const CC_AND: u8 = 24; // '&'
const CC_TILDA: u8 = 25; // '~'
const CC_DOT: u8 = 26; // '.'
const CC_ID: u8 = 27; // unicode characters usable in IDs
#[allow(dead_code)]
const CC_ILLEGAL: u8 = 28; // Illegal character
const CC_NUL: u8 = 29; // 0x00
const CC_BOM: u8 = 30; // First byte of UTF8 BOM:  0xEF 0xBB 0xBF

#[rustfmt::skip]
static AI_CLASS: [u8; 256] = [
/*         x0  x1  x2  x3  x4  x5  x6  x7  x8  x9  xa  xb  xc  xd  xe  xf */
/* 0x */   29, 28, 28, 28, 28, 28, 28, 28, 28,  7,  7, 28,  7,  7, 28, 28,
/* 1x */   28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
/* 2x */    7, 15,  8,  5,  4, 22, 24,  8, 17, 18, 21, 20, 23, 11, 26, 16,
/* 3x */    3,  3,  3,  3,  3,  3,  3,  3,  3,  3,  5, 19, 12, 14, 13,  6,
/* 4x */    5,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,
/* 5x */    1,  1,  1,  1,  1,  1,  1,  1,  0,  2,  2,  9, 28, 28, 28,  2,
/* 6x */    8,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,
/* 7x */    1,  1,  1,  1,  1,  1,  1,  1,  0,  2,  2, 28, 10, 28, 25, 28,
/* 8x */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
/* 9x */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
/* Ax */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
/* Bx */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
/* Cx */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
/* Dx */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
/* Ex */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 30,
/* Fx */   27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
];

/// `IdChar(C)`: `(sqlite3CtypeMap[C] & 0x46)!=0` — letters, digits, `_`, `$` and every byte
/// with the high bit set.
#[inline]
pub fn id_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// `keywordCode(z, n, &type)`: the token code of the keyword `z`, `TK_ID` if it is not one.
pub(crate) fn keyword_code(z: &[u8]) -> u8 {
    if z.len() < 2 || z.len() > 17 {
        return TK_ID;
    }
    let mut buf = [0u8; 17];
    for (i, c) in z.iter().enumerate() {
        buf[i] = c.to_ascii_uppercase();
    }
    let key = &buf[..z.len()];
    match KEYWORDS.binary_search_by(|(k, _)| k.as_bytes().cmp(key)) {
        Ok(i) => KEYWORDS[i].1,
        Err(_) => TK_ID,
    }
}

/// `sqlite3GetToken(z, &tokenType)`: the length in bytes of the token that begins at `z[0]`
/// and its type. Reading past the end of `z` yields the NUL terminator of C.
pub fn sqlite3_get_token(z: &[u8]) -> (usize, u8) {
    let at = |i: usize| -> u8 { if i < z.len() { z[i] } else { 0 } };
    let mut i: usize;
    match AI_CLASS[at(0) as usize] {
        CC_SPACE => {
            i = 1;
            while is_space(at(i)) {
                i += 1;
            }
            return (i, TK_SPACE);
        }
        CC_MINUS => {
            if at(1) == b'-' {
                i = 2;
                loop {
                    let c = at(i);
                    if c == 0 || c == b'\n' {
                        break;
                    }
                    i += 1;
                }
                return (i, TK_COMMENT);
            } else if at(1) == b'>' {
                return (2 + (at(2) == b'>') as usize, TK_PTR);
            }
            return (1, TK_MINUS);
        }
        CC_LP => return (1, TK_LP),
        CC_RP => return (1, TK_RP),
        CC_SEMI => return (1, TK_SEMI),
        CC_PLUS => return (1, TK_PLUS),
        CC_STAR => return (1, TK_STAR),
        CC_SLASH => {
            if at(1) != b'*' || at(2) == 0 {
                return (1, TK_SLASH);
            }
            i = 3;
            let mut c = at(2);
            while (c != b'*' || at(i) != b'/') && {
                c = at(i);
                c != 0
            } {
                i += 1;
            }
            if c != 0 {
                i += 1;
            }
            return (i, TK_COMMENT);
        }
        CC_PERCENT => return (1, TK_REM),
        CC_EQ => return (1 + (at(1) == b'=') as usize, TK_EQ),
        CC_LT => {
            let c = at(1);
            return if c == b'=' {
                (2, TK_LE)
            } else if c == b'>' {
                (2, TK_NE)
            } else if c == b'<' {
                (2, TK_LSHIFT)
            } else {
                (1, TK_LT)
            };
        }
        CC_GT => {
            let c = at(1);
            return if c == b'=' {
                (2, TK_GE)
            } else if c == b'>' {
                (2, TK_RSHIFT)
            } else {
                (1, TK_GT)
            };
        }
        CC_BANG => {
            return if at(1) != b'=' { (1, TK_ILLEGAL) } else { (2, TK_NE) };
        }
        CC_PIPE => {
            return if at(1) != b'|' { (1, TK_BITOR) } else { (2, TK_CONCAT) };
        }
        CC_COMMA => return (1, TK_COMMA),
        CC_AND => return (1, TK_BITAND),
        CC_TILDA => return (1, TK_BITNOT),
        CC_QUOTE => {
            let delim = at(0);
            i = 1;
            let mut c;
            loop {
                c = at(i);
                if c == 0 {
                    break;
                }
                if c == delim {
                    if at(i + 1) == delim {
                        i += 1;
                    } else {
                        break;
                    }
                }
                i += 1;
            }
            return if c == b'\'' {
                (i + 1, TK_STRING)
            } else if c != 0 {
                (i + 1, TK_ID)
            } else {
                (i, TK_ILLEGAL)
            };
        }
        cc @ (CC_DOT | CC_DIGIT) => {
            if cc == CC_DOT && !is_digit(at(1)) {
                return (1, TK_DOT);
            }
            // If the next character is a digit, this is a floating point number that begins
            // with ".". Fall thru into the next case.
            let mut token_type = TK_INTEGER;
            let sep = SQLITE_DIGIT_SEPARATOR as u8;
            if at(0) == b'0' && (at(1) == b'x' || at(1) == b'X') && is_xdigit(at(2)) {
                i = 3;
                loop {
                    if !is_xdigit(at(i)) {
                        if at(i) == sep {
                            token_type = TK_QNUMBER;
                        } else {
                            break;
                        }
                    }
                    i += 1;
                }
            } else {
                i = 0;
                loop {
                    if !is_digit(at(i)) {
                        if at(i) == sep {
                            token_type = TK_QNUMBER;
                        } else {
                            break;
                        }
                    }
                    i += 1;
                }
                if at(i) == b'.' {
                    if token_type == TK_INTEGER {
                        token_type = TK_FLOAT;
                    }
                    i += 1;
                    loop {
                        if !is_digit(at(i)) {
                            if at(i) == sep {
                                token_type = TK_QNUMBER;
                            } else {
                                break;
                            }
                        }
                        i += 1;
                    }
                }
                if (at(i) == b'e' || at(i) == b'E')
                    && (is_digit(at(i + 1))
                        || ((at(i + 1) == b'+' || at(i + 1) == b'-') && is_digit(at(i + 2))))
                {
                    if token_type == TK_INTEGER {
                        token_type = TK_FLOAT;
                    }
                    i += 2;
                    loop {
                        if !is_digit(at(i)) {
                            if at(i) == sep {
                                token_type = TK_QNUMBER;
                            } else {
                                break;
                            }
                        }
                        i += 1;
                    }
                }
            }
            while id_char(at(i)) {
                token_type = TK_ILLEGAL;
                i += 1;
            }
            return (i, token_type);
        }
        CC_QUOTE2 => {
            i = 1;
            let mut c = at(0);
            while c != b']' && {
                c = at(i);
                c != 0
            } {
                i += 1;
            }
            return (i, if c == b']' { TK_ID } else { TK_ILLEGAL });
        }
        CC_VARNUM => {
            i = 1;
            while is_digit(at(i)) {
                i += 1;
            }
            return (i, TK_VARIABLE);
        }
        CC_DOLLAR | CC_VARALPHA => {
            let mut n = 0;
            let mut token_type = TK_VARIABLE;
            i = 1;
            loop {
                let mut c = at(i);
                if c == 0 {
                    break;
                }
                if id_char(c) {
                    n += 1;
                } else if c == b'(' && n > 0 {
                    loop {
                        i += 1;
                        c = at(i);
                        if !(c != 0 && !is_space(c) && c != b')') {
                            break;
                        }
                    }
                    if c == b')' {
                        i += 1;
                    } else {
                        token_type = TK_ILLEGAL;
                    }
                    break;
                } else if c == b':' && at(i + 1) == b':' {
                    i += 1;
                } else {
                    break;
                }
                i += 1;
            }
            if n == 0 {
                token_type = TK_ILLEGAL;
            }
            return (i, token_type);
        }
        CC_KYWD0 => {
            if AI_CLASS[at(1) as usize] > CC_KYWD {
                i = 1;
            } else {
                i = 2;
                while AI_CLASS[at(i) as usize] <= CC_KYWD {
                    i += 1;
                }
                if id_char(at(i)) {
                    // This token started out using characters that can appear in keywords,
                    // but z[i] is a character not allowed within keywords, so this must be an
                    // identifier instead
                    i += 1;
                } else {
                    return (i, keyword_code(&z[..i]));
                }
            }
        }
        CC_X => {
            if at(1) == b'\'' {
                let mut token_type = TK_BLOB;
                i = 2;
                while is_xdigit(at(i)) {
                    i += 1;
                }
                if at(i) != b'\'' || i % 2 != 0 {
                    token_type = TK_ILLEGAL;
                    while at(i) != 0 && at(i) != b'\'' {
                        i += 1;
                    }
                }
                if at(i) != 0 {
                    i += 1;
                }
                return (i, token_type);
            }
            // If it is not a BLOB literal, then it must be an ID, since no SQL keywords start
            // with the letter 'x'.
            i = 1;
        }
        CC_KYWD | CC_ID => {
            i = 1;
        }
        CC_BOM => {
            if at(1) == 0xbb && at(2) == 0xbf {
                return (3, TK_SPACE);
            }
            i = 1;
        }
        CC_NUL => return (0, TK_ILLEGAL),
        _ => return (1, TK_ILLEGAL),
    }
    while id_char(at(i)) {
        i += 1;
    }
    (i, TK_ID)
}

/// `getToken(&z)`: the id of the next token, skipping space and comments; identifiers and
/// everything that can fall back to an identifier are reported as `TK_ID`.
fn get_token(z: &[u8], pos: &mut usize) -> u8 {
    let mut t;
    loop {
        let (n, ty) = sqlite3_get_token(&z[(*pos).min(z.len())..]);
        *pos += n;
        t = ty;
        if !(t == TK_SPACE || t == TK_COMMENT) {
            break;
        }
    }
    if t == TK_ID
        || t == TK_STRING
        || t == TK_JOIN_KW
        || t == TK_WINDOW
        || t == TK_OVER
        || crate::parse::sqlite3_parser_fallback(t) == TK_ID
    {
        t = TK_ID;
    }
    t
}

/// `analyzeWindowKeyword(z)`: WINDOW is a keyword if the following token is an identifier and
/// the token after that one is `AS`.
fn analyze_window_keyword(z: &[u8], mut pos: usize) -> u8 {
    let t = get_token(z, &mut pos);
    if t != TK_ID {
        return TK_ID;
    }
    let t = get_token(z, &mut pos);
    if t != TK_AS {
        return TK_ID;
    }
    TK_WINDOW
}
/// `analyzeOverKeyword(z, lastToken)`: OVER is a keyword if the previous token was `)` and the
/// next token is either `(` or an identifier.
fn analyze_over_keyword(z: &[u8], mut pos: usize, last_token: i32) -> u8 {
    if last_token == TK_RP as i32 {
        let t = get_token(z, &mut pos);
        if t == TK_LP || t == TK_ID {
            return TK_OVER;
        }
    }
    TK_ID
}
/// `analyzeFilterKeyword(z, lastToken)`: FILTER is a keyword if the previous token was `)` and
/// the next token is `(`.
fn analyze_filter_keyword(z: &[u8], mut pos: usize, last_token: i32) -> u8 {
    if last_token == TK_RP as i32 && get_token(z, &mut pos) == TK_LP {
        return TK_FILTER;
    }
    TK_ID
}

/// `sqlite3RunParser(pParse, zSql)`: run the parser (and, from its reduce actions, the code
/// generator) on the statement text `pParse.z_sql`. Returns the number of errors.
pub fn sqlite3_run_parser(p: &mut Parse) -> i32 {
    let z: Vec<u8> = p.z_sql.as_bytes().to_vec();
    let mut pos: usize = 0;
    let mut last_token_parsed: i32 = -1;
    let mut mx_sql_len: i64 = p.db.a_limit[SQLITE_LIMIT_SQL_LENGTH as usize] as i64;
    p.rc = SQLITE_OK;
    let interrupted = p.db.host.prepare_interrupt_pending();
    let mut engine = YyParser::new();
    loop {
        let (mut n, mut token_type) = sqlite3_get_token(&z[pos.min(z.len())..]);
        mx_sql_len -= n as i64;
        if mx_sql_len < 0 {
            p.rc = SQLITE_TOOBIG;
            p.n_err += 1;
            break;
        }
        if token_type >= TK_WINDOW {
            debug_assert!(matches!(
                token_type,
                TK_SPACE | TK_OVER | TK_FILTER | TK_ILLEGAL | TK_WINDOW | TK_QNUMBER | TK_COMMENT
            ));
            // tokenize.c checks this before handling whitespace, EOF or illegal tokens.
            // Rebuilding an already-loaded schema here is local to the code generator.
            if interrupted && (!p.db.init_busy || p.db.schema_not_loaded) {
                p.rc = SQLITE_INTERRUPT;
                p.n_err += 1;
                break;
            }
            if token_type == TK_SPACE {
                pos += n;
                continue;
            }
            if pos >= z.len() || z[pos] == 0 {
                // Upon reaching the end of input, call the parser two more times with tokens
                // TK_SEMI and 0, in that order.
                if last_token_parsed == TK_SEMI as i32 {
                    token_type = 0;
                } else if last_token_parsed == 0 {
                    break;
                } else {
                    token_type = TK_SEMI;
                }
                n = 0;
            } else if token_type == TK_WINDOW {
                token_type = analyze_window_keyword(&z, pos + 6);
            } else if token_type == TK_OVER {
                token_type = analyze_over_keyword(&z, pos + 4, last_token_parsed);
            } else if token_type == TK_FILTER {
                token_type = analyze_filter_keyword(&z, pos + 6, last_token_parsed);
            } else if token_type == TK_COMMENT && (p.db.init_busy || (p.db.flags & SQLITE_Comments) != 0) {
                // Ignore SQL comments if either (1) we are reparsing the schema or (2)
                // SQLITE_DBCONFIG_ENABLE_COMMENTS is turned on (the default).
                pos += n;
                continue;
            } else if token_type != TK_QNUMBER {
                let x = Token { z: pos as u32, n: n as u32 };
                let msg = format!("unrecognized token: \"{}\"", p.tok_str(&x));
                p.error_msg_tok(msg, &x);
                break;
            }
        }
        p.s_last_token = Token { z: pos as u32, n: n as u32 };
        let tok = p.s_last_token;
        engine.sqlite3_parser(p, token_type, tok);
        last_token_parsed = token_type as i32;
        pos += n;
        if p.rc != SQLITE_OK {
            break;
        }
    }
    p.z_tail = pos.min(z.len());
    let mut n_err = 0;
    if p.z_err_msg.is_some() || (p.rc != SQLITE_OK && p.rc != SQLITE_DONE) {
        if p.z_err_msg.is_none() {
            // sqlite3ErrStr(pParse->rc)
            p.z_err_msg = Some(
                match p.rc {
                    SQLITE_TOOBIG => "string or blob too big",
                    SQLITE_NOMEM => "out of memory",
                    SQLITE_INTERRUPT => "interrupted",
                    _ => "SQL logic error",
                }
                .to_owned(),
            );
        }
        n_err += 1;
    }
    n_err
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(sql: &str) -> Vec<(String, u8)> {
        let z = sql.as_bytes();
        let mut pos = 0;
        let mut out = Vec::new();
        while pos < z.len() {
            let (n, t) = sqlite3_get_token(&z[pos..]);
            if n == 0 {
                break;
            }
            if t != TK_SPACE {
                out.push((sql[pos..pos + n].to_owned(), t));
            }
            pos += n;
        }
        out
    }

    #[test]
    fn tokens() {
        let t = toks("SELECT x'ab', 1.5e3, 0x1F, 1_000, a->>b, ?12, :nm, $v(1), [q w], \"d\", 'it''s' -- c");
        let kinds: Vec<u8> = t.iter().map(|x| x.1).collect();
        assert_eq!(
            kinds,
            [
                TK_SELECT, TK_BLOB, TK_COMMA, TK_FLOAT, TK_COMMA, TK_INTEGER, TK_COMMA, TK_QNUMBER, TK_COMMA, TK_ID,
                TK_PTR, TK_ID, TK_COMMA, TK_VARIABLE, TK_COMMA, TK_VARIABLE, TK_COMMA, TK_VARIABLE, TK_COMMA, TK_ID,
                TK_COMMA, TK_ID, TK_COMMA, TK_STRING, TK_COMMENT
            ]
        );
        assert_eq!(t[10].0, "->>");
        assert_eq!(toks("1abc")[0].1, TK_ILLEGAL);
        assert_eq!(toks("x'abc'")[0].1, TK_ILLEGAL);
        assert_eq!(toks("'abc")[0].1, TK_ILLEGAL);
        assert_eq!(toks("!")[0].1, TK_ILLEGAL);
        assert_eq!(toks("between")[0].1, TK_BETWEEN);
        assert_eq!(toks("betweenx")[0].1, TK_ID);
        assert_eq!(toks("select_")[0].1, TK_ID);
        assert_eq!(toks("select$")[0].1, TK_ID);
        assert_eq!(toks("/* c */ 1").iter().map(|x| x.1).collect::<Vec<_>>(), [TK_COMMENT, TK_INTEGER]);
    }
}
