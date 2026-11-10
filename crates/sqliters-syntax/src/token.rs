//! SQLite 3.54.1 lexical analyzer ported from `sqlite/src/tokenize.c` and `tool/mkkeywordhash.c`.
#![forbid(unsafe_code)]

use sqliters_types::{Result, SqliteError};

use crate::complete::is_id_char;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Punctuation & operators
    Semi,
    Lp,
    Rp,
    Comma,
    Dot,
    Plus,
    Minus,
    Star,
    Slash,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitAnd,
    BitOr,
    BitNot,
    LShift,
    RShift,
    Concat,
    Arrow,       // ->
    DoubleArrow, // ->>

    // Literals, identifiers, bind variables
    Id,
    QuotedId, // "...", `...`, [...]
    StringLiteral,
    BlobLiteral,
    Integer,
    Float,
    Variable, // ?, ?123, :name, @name, $name

    // Keywords
    Abort,
    Action,
    Add,
    After,
    All,
    Alter,
    Always,
    Analyze,
    And,
    As,
    Asc,
    Attach,
    Autoincrement,
    Before,
    Begin,
    Between,
    By,
    Cascade,
    Case,
    Cast,
    Check,
    Collate,
    ColumnKw,
    Commit,
    Conflict,
    Constraint,
    Create,
    Cross,
    Current,
    CurrentDate,
    CurrentTime,
    CurrentTimestamp,
    Database,
    Default,
    Deferrable,
    Deferred,
    Delete,
    Desc,
    Detach,
    Distinct,
    Do,
    Drop,
    Each,
    Else,
    End,
    Escape,
    Except,
    Exclude,
    Exclusive,
    Exists,
    Explain,
    Fail,
    False,
    Filter,
    First,
    Following,
    For,
    Foreign,
    From,
    Full,
    Generated,
    Glob,
    Group,
    Groups,
    Having,
    If,
    Ignore,
    Immediate,
    In,
    Index,
    Indexed,
    Initially,
    Inner,
    Insert,
    Instead,
    Intersect,
    Into,
    Is,
    IsNull,
    Join,
    Key,
    Last,
    Left,
    Like,
    Limit,
    Match,
    Materialized,
    Natural,
    No,
    Not,
    Nothing,
    NotNull,
    Null,
    Nulls,
    Of,
    Offset,
    On,
    Or,
    Order,
    Others,
    Outer,
    Over,
    Partition,
    Plan,
    Pragma,
    Preceding,
    Primary,
    Query,
    Raise,
    Range,
    Recursive,
    References,
    Regexp,
    Reindex,
    Release,
    Rename,
    Replace,
    Restrict,
    Returning,
    Right,
    Rollback,
    Row,
    Rowid,
    Rows,
    Savepoint,
    Select,
    Set,
    Strict,
    Table,
    Temp,
    Then,
    Ties,
    To,
    Transaction,
    Trigger,
    True,
    Unbounded,
    Union,
    Unique,
    Update,
    Using,
    Vacuum,
    Values,
    View,
    Virtual,
    When,
    Where,
    Window,
    With,
    Without,

    Eof,
}

#[allow(non_upper_case_globals)]
impl TokenKind {
    pub const Ptr: Self = Self::Arrow;
    pub const PtrText: Self = Self::DoubleArrow;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    pub offset: usize,
}

/// Maps an ASCII identifier to an SQLite keyword token (`keywordCode` in `keywordhash.h`).
pub fn keyword_kind(word: &str) -> Option<TokenKind> {
    if word.len() > 17 || !word.is_ascii() {
        return None;
    }
    let upper = word.to_ascii_uppercase();
    let kind = match upper.as_str() {
        "ABORT" => TokenKind::Abort,
        "ACTION" => TokenKind::Action,
        "ADD" => TokenKind::Add,
        "AFTER" => TokenKind::After,
        "ALL" => TokenKind::All,
        "ALTER" => TokenKind::Alter,
        "ALWAYS" => TokenKind::Always,
        "ANALYZE" => TokenKind::Analyze,
        "AND" => TokenKind::And,
        "AS" => TokenKind::As,
        "ASC" => TokenKind::Asc,
        "ATTACH" => TokenKind::Attach,
        "AUTOINCREMENT" => TokenKind::Autoincrement,
        "BEFORE" => TokenKind::Before,
        "BEGIN" => TokenKind::Begin,
        "BETWEEN" => TokenKind::Between,
        "BY" => TokenKind::By,
        "CASCADE" => TokenKind::Cascade,
        "CASE" => TokenKind::Case,
        "CAST" => TokenKind::Cast,
        "CHECK" => TokenKind::Check,
        "COLLATE" => TokenKind::Collate,
        "COLUMN" => TokenKind::ColumnKw,
        "COMMIT" => TokenKind::Commit,
        "CONFLICT" => TokenKind::Conflict,
        "CONSTRAINT" => TokenKind::Constraint,
        "CREATE" => TokenKind::Create,
        "CROSS" => TokenKind::Cross,
        "CURRENT" => TokenKind::Current,
        "CURRENT_DATE" => TokenKind::CurrentDate,
        "CURRENT_TIME" => TokenKind::CurrentTime,
        "CURRENT_TIMESTAMP" => TokenKind::CurrentTimestamp,
        "DATABASE" => TokenKind::Database,
        "DEFAULT" => TokenKind::Default,
        "DEFERRABLE" => TokenKind::Deferrable,
        "DEFERRED" => TokenKind::Deferred,
        "DELETE" => TokenKind::Delete,
        "DESC" => TokenKind::Desc,
        "DETACH" => TokenKind::Detach,
        "DISTINCT" => TokenKind::Distinct,
        "DO" => TokenKind::Do,
        "DROP" => TokenKind::Drop,
        "EACH" => TokenKind::Each,
        "ELSE" => TokenKind::Else,
        "END" => TokenKind::End,
        "ESCAPE" => TokenKind::Escape,
        "EXCEPT" => TokenKind::Except,
        "EXCLUDE" => TokenKind::Exclude,
        "EXCLUSIVE" => TokenKind::Exclusive,
        "EXISTS" => TokenKind::Exists,
        "EXPLAIN" => TokenKind::Explain,
        "FAIL" => TokenKind::Fail,
        "FALSE" => TokenKind::False,
        "FILTER" => TokenKind::Filter,
        "FIRST" => TokenKind::First,
        "FOLLOWING" => TokenKind::Following,
        "FOR" => TokenKind::For,
        "FOREIGN" => TokenKind::Foreign,
        "FROM" => TokenKind::From,
        "FULL" => TokenKind::Full,
        "GENERATED" => TokenKind::Generated,
        "GLOB" => TokenKind::Glob,
        "GROUP" => TokenKind::Group,
        "GROUPS" => TokenKind::Groups,
        "HAVING" => TokenKind::Having,
        "IF" => TokenKind::If,
        "IGNORE" => TokenKind::Ignore,
        "IMMEDIATE" => TokenKind::Immediate,
        "IN" => TokenKind::In,
        "INDEX" => TokenKind::Index,
        "INDEXED" => TokenKind::Indexed,
        "INITIALLY" => TokenKind::Initially,
        "INNER" => TokenKind::Inner,
        "INSERT" => TokenKind::Insert,
        "INSTEAD" => TokenKind::Instead,
        "INTERSECT" => TokenKind::Intersect,
        "INTO" => TokenKind::Into,
        "IS" => TokenKind::Is,
        "ISNULL" => TokenKind::IsNull,
        "JOIN" => TokenKind::Join,
        "KEY" => TokenKind::Key,
        "LAST" => TokenKind::Last,
        "LEFT" => TokenKind::Left,
        "LIKE" => TokenKind::Like,
        "LIMIT" => TokenKind::Limit,
        "MATCH" => TokenKind::Match,
        "MATERIALIZED" => TokenKind::Materialized,
        "NATURAL" => TokenKind::Natural,
        "NO" => TokenKind::No,
        "NOT" => TokenKind::Not,
        "NOTHING" => TokenKind::Nothing,
        "NOTNULL" => TokenKind::NotNull,
        "NULL" => TokenKind::Null,
        "NULLS" => TokenKind::Nulls,
        "OF" => TokenKind::Of,
        "OFFSET" => TokenKind::Offset,
        "ON" => TokenKind::On,
        "OR" => TokenKind::Or,
        "ORDER" => TokenKind::Order,
        "OTHERS" => TokenKind::Others,
        "OUTER" => TokenKind::Outer,
        "OVER" => TokenKind::Over,
        "PARTITION" => TokenKind::Partition,
        "PLAN" => TokenKind::Plan,
        "PRAGMA" => TokenKind::Pragma,
        "PRECEDING" => TokenKind::Preceding,
        "PRIMARY" => TokenKind::Primary,
        "QUERY" => TokenKind::Query,
        "RAISE" => TokenKind::Raise,
        "RANGE" => TokenKind::Range,
        "RECURSIVE" => TokenKind::Recursive,
        "REFERENCES" => TokenKind::References,
        "REGEXP" => TokenKind::Regexp,
        "REINDEX" => TokenKind::Reindex,
        "RELEASE" => TokenKind::Release,
        "RENAME" => TokenKind::Rename,
        "REPLACE" => TokenKind::Replace,
        "RESTRICT" => TokenKind::Restrict,
        "RETURNING" => TokenKind::Returning,
        "RIGHT" => TokenKind::Right,
        "ROLLBACK" => TokenKind::Rollback,
        "ROW" => TokenKind::Row,
        "ROWID" => TokenKind::Rowid,
        "ROWS" => TokenKind::Rows,
        "SAVEPOINT" => TokenKind::Savepoint,
        "SELECT" => TokenKind::Select,
        "SET" => TokenKind::Set,
        "STRICT" => TokenKind::Strict,
        "TABLE" => TokenKind::Table,
        "TEMP" | "TEMPORARY" => TokenKind::Temp,
        "THEN" => TokenKind::Then,
        "TIES" => TokenKind::Ties,
        "TO" => TokenKind::To,
        "TRANSACTION" => TokenKind::Transaction,
        "TRIGGER" => TokenKind::Trigger,
        "TRUE" => TokenKind::True,
        "UNBOUNDED" => TokenKind::Unbounded,
        "UNION" => TokenKind::Union,
        "UNIQUE" => TokenKind::Unique,
        "UPDATE" => TokenKind::Update,
        "USING" => TokenKind::Using,
        "VACUUM" => TokenKind::Vacuum,
        "VALUES" => TokenKind::Values,
        "VIEW" => TokenKind::View,
        "VIRTUAL" => TokenKind::Virtual,
        "WHEN" => TokenKind::When,
        "WHERE" => TokenKind::Where,
        "WINDOW" => TokenKind::Window,
        "WITH" => TokenKind::With,
        "WITHOUT" => TokenKind::Without,
        _ => return None,
    };
    Some(kind)
}

/// Tokenizes `sql` into a `Vec<Token<'a>>` (always ending with `TokenKind::Eof`),
/// skipping whitespace, UTF-8 BOM, `--` line comments, and `/* ... */` block comments.
pub fn tokenize(sql: &str) -> Result<Vec<Token<'_>>> {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut tokens = Vec::new();

    // Optional leading UTF-8 BOM (0xEF 0xBB 0xBF, CC_BOM in tokenize.c:580)
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        i = 3;
    }

    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' => {
                i += 1;
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
                {
                    i += 1;
                }
            }
            b'-' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'-' {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    if i + 2 < bytes.len() && bytes[i + 2] == b'>' {
                        i += 3;
                        tokens.push(Token {
                            kind: TokenKind::DoubleArrow,
                            text: &sql[start..i],
                            offset: start,
                        });
                    } else {
                        i += 2;
                        tokens.push(Token {
                            kind: TokenKind::Arrow,
                            text: &sql[start..i],
                            offset: start,
                        });
                    }
                } else {
                    i += 1;
                    tokens.push(Token {
                        kind: TokenKind::Minus,
                        text: &sql[start..i],
                        offset: start,
                    });
                }
            }
            b'/' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        i += 1;
                    }
                    if i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        return Err(SqliteError::syntax("unterminated /* comment", start));
                    }
                } else {
                    i += 1;
                    tokens.push(Token {
                        kind: TokenKind::Slash,
                        text: &sql[start..i],
                        offset: start,
                    });
                }
            }
            b'(' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Lp,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b')' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Rp,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b';' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Semi,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'+' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Plus,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'*' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Star,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'%' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Rem,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b',' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'&' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::BitAnd,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'~' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::BitNot,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'=' => {
                i += if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    2
                } else {
                    1
                };
                tokens.push(Token {
                    kind: TokenKind::Eq,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    i += 2;
                    tokens.push(Token {
                        kind: TokenKind::Le,
                        text: &sql[start..i],
                        offset: start,
                    });
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    i += 2;
                    tokens.push(Token {
                        kind: TokenKind::Ne,
                        text: &sql[start..i],
                        offset: start,
                    });
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'<' {
                    i += 2;
                    tokens.push(Token {
                        kind: TokenKind::LShift,
                        text: &sql[start..i],
                        offset: start,
                    });
                } else {
                    i += 1;
                    tokens.push(Token {
                        kind: TokenKind::Lt,
                        text: &sql[start..i],
                        offset: start,
                    });
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    i += 2;
                    tokens.push(Token {
                        kind: TokenKind::Ge,
                        text: &sql[start..i],
                        offset: start,
                    });
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    i += 2;
                    tokens.push(Token {
                        kind: TokenKind::RShift,
                        text: &sql[start..i],
                        offset: start,
                    });
                } else {
                    i += 1;
                    tokens.push(Token {
                        kind: TokenKind::Gt,
                        text: &sql[start..i],
                        offset: start,
                    });
                }
            }
            b'!' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    i += 2;
                    tokens.push(Token {
                        kind: TokenKind::Ne,
                        text: &sql[start..i],
                        offset: start,
                    });
                } else {
                    return Err(SqliteError::syntax(
                        format!("unrecognized token: \"{}\"", &sql[start..start + 1]),
                        start,
                    ));
                }
            }
            b'|' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    i += 2;
                    tokens.push(Token {
                        kind: TokenKind::Concat,
                        text: &sql[start..i],
                        offset: start,
                    });
                } else {
                    i += 1;
                    tokens.push(Token {
                        kind: TokenKind::BitOr,
                        text: &sql[start..i],
                        offset: start,
                    });
                }
            }
            delim @ (b'\'' | b'"' | b'`') => {
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == delim {
                        if i + 1 < bytes.len() && bytes[i + 1] == delim {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Err(SqliteError::syntax(
                        format!("unrecognized token: \"{}\"", &sql[start..]),
                        start,
                    ));
                }
                let kind = if delim == b'\'' {
                    TokenKind::StringLiteral
                } else {
                    TokenKind::QuotedId
                };
                tokens.push(Token {
                    kind,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'[' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(SqliteError::syntax(
                        format!("unrecognized token: \"{}\"", &sql[start..]),
                        start,
                    ));
                }
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::QuotedId,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'?' => {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Variable,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b':' | b'@' | b'$' | b'#' => {
                i += 1;
                let mut n = 0usize;
                while i < bytes.len() {
                    let c = bytes[i];
                    if is_id_char(c) {
                        n += 1;
                        i += 1;
                    } else if c == b'(' && n > 0 {
                        i += 1;
                        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b')'
                        {
                            i += 1;
                        }
                        if i < bytes.len() && bytes[i] == b')' {
                            i += 1;
                        } else {
                            return Err(SqliteError::syntax(
                                format!("unrecognized token: \"{}\"", &sql[start..i]),
                                start,
                            ));
                        }
                        break;
                    } else if c == b':' && i + 1 < bytes.len() && bytes[i + 1] == b':' {
                        i += 2;
                    } else {
                        break;
                    }
                }
                if n == 0 {
                    return Err(SqliteError::syntax(
                        format!("unrecognized token: \"{}\"", &sql[start..i]),
                        start,
                    ));
                }
                tokens.push(Token {
                    kind: TokenKind::Variable,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'.' if !(i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Dot,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'.' | b'0'..=b'9' => {
                // Supports decimal integers, 0x hex integers, floats, and SQLite 3.54 `_` digit separators!
                let mut kind = TokenKind::Integer;
                if b == b'0'
                    && i + 2 < bytes.len()
                    && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X')
                    && bytes[i + 2].is_ascii_hexdigit()
                {
                    i += 2;
                    while i < bytes.len() && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
                        if bytes[i] == b'_'
                            && (bytes[i - 1] == b'_'
                                || i + 1 >= bytes.len()
                                || !bytes[i + 1].is_ascii_hexdigit())
                        {
                            return Err(SqliteError::syntax(
                                "invalid underscore in hex literal",
                                start,
                            ));
                        }
                        i += 1;
                    }
                } else {
                    if b == b'.' {
                        kind = TokenKind::Float;
                        i += 1;
                        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                            i += 1;
                        }
                    } else {
                        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                            i += 1;
                        }
                        if i < bytes.len() && bytes[i] == b'.' {
                            kind = TokenKind::Float;
                            i += 1;
                            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_')
                            {
                                i += 1;
                            }
                        }
                    }
                    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                        let next_ok = if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                            true
                        } else {
                            i + 2 < bytes.len()
                                && (bytes[i + 1] == b'+' || bytes[i + 1] == b'-')
                                && bytes[i + 2].is_ascii_digit()
                        };
                        if next_ok {
                            kind = TokenKind::Float;
                            i += 1;
                            if bytes[i] == b'+' || bytes[i] == b'-' {
                                i += 1;
                            }
                            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_')
                            {
                                i += 1;
                            }
                        }
                    }
                }
                if i < bytes.len() && is_id_char(bytes[i]) {
                    while i < bytes.len() && is_id_char(bytes[i]) {
                        i += 1;
                    }
                    return Err(SqliteError::syntax(
                        format!("unrecognized token: \"{}\"", &sql[start..i]),
                        start,
                    ));
                }
                tokens.push(Token {
                    kind,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            b'x' | b'X' if i + 1 < bytes.len() && bytes[i + 1] == b'\'' => {
                i += 2;
                let hex_start = i;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let hex_len = i - hex_start;
                if i >= bytes.len() || bytes[i] != b'\'' || !hex_len.is_multiple_of(2) {
                    return Err(SqliteError::syntax(
                        format!("unrecognized blob literal starting at offset {start}"),
                        start,
                    ));
                }
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::BlobLiteral,
                    text: &sql[start..i],
                    offset: start,
                });
            }
            c if is_id_char(c) => {
                i += 1;
                while i < bytes.len() && is_id_char(bytes[i]) {
                    i += 1;
                }
                let text = &sql[start..i];
                let kind = keyword_kind(text).unwrap_or(TokenKind::Id);
                tokens.push(Token {
                    kind,
                    text,
                    offset: start,
                });
            }
            _ => {
                let ch_len = sql[start..].chars().next().map_or(1, char::len_utf8);
                return Err(SqliteError::syntax(
                    format!("unrecognized token: \"{}\"", &sql[start..start + ch_len]),
                    start,
                ));
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        text: "",
        offset: bytes.len(),
    });
    Ok(tokens)
}

/// Removes outer quotes (`"..."`, `'...'`, `` `...` ``, `[...]`) and unescapes doubled delimiters (`sqlite3Dequote`).
pub fn dequote(raw: &str) -> String {
    let bytes = raw.as_bytes();
    if bytes.len() < 2 {
        return raw.to_owned();
    }
    match bytes[0] {
        b'[' if bytes.last() == Some(&b']') => raw[1..raw.len() - 1].to_owned(),
        delim @ (b'\'' | b'"' | b'`') if bytes.last() == Some(&delim) => {
            let inner = &raw[1..raw.len() - 1];
            let dch = delim as char;
            let mut out = String::with_capacity(inner.len());
            let mut chars = inner.chars().peekable();
            while let Some(c) = chars.next() {
                if c == dch && chars.peek() == Some(&dch) {
                    let _ = chars.next();
                }
                out.push(c);
            }
            out
        }
        _ => raw.to_owned(),
    }
}

/// Decodes an `X'...'` / `x'...'` SQLite hex blob literal into bytes.
pub fn decode_blob_literal(raw: &str) -> Result<Vec<u8>> {
    let inner = &raw[2..raw.len() - 1];
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(SqliteError::syntax("invalid hex digit in blob literal", 0)),
    }
}

/// Parses an integer token (decimal, `0x` hex, with optional `_` separators).
/// If a decimal integer overflows `i64`, returns `Ok(None)` so the caller can promote to `f64` per SQLite rules!
pub fn parse_int_literal(raw: &str) -> Result<Option<i64>> {
    let cleaned: String = raw.chars().filter(|&c| c != '_').collect();
    if let Some(hex) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        let sig = hex.trim_start_matches('0');
        if sig.len() > 16 {
            return Err(SqliteError::new(
                sqliters_types::ErrorCode::Error,
                format!("hex literal too big: {raw}"),
            ));
        }
        let u = if sig.is_empty() {
            0
        } else {
            u64::from_str_radix(sig, 16)
                .map_err(|_| SqliteError::syntax(format!("invalid hex literal: {raw}"), 0))?
        };
        return Ok(Some(u as i64));
    }
    Ok(cleaned.parse::<i64>().ok())
}

/// Parses a floating-point token (with optional `_` separators).
pub fn parse_float_literal(raw: &str) -> Result<f64> {
    let cleaned: String = raw.chars().filter(|&c| c != '_').collect();
    cleaned
        .parse::<f64>()
        .map_err(|_| SqliteError::syntax(format!("invalid float literal: {raw}"), 0))
}
