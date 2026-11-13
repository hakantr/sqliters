//! Helpers shared by the shell: error context, statement classification.

/// `shell_error_context()`: the two extra lines of a parse error that show the SQL around the
/// error offset with a caret, or nothing when the engine reported no offset.
pub fn shell_error_context(sql: &str, offset: Option<usize>) -> String {
    let Some(mut off) = offset else {
        return String::new();
    };
    // (The C shell joins the input lines with '\n' and has no newline after the last one.)
    let sql = sql.trim_end_matches(['\r', '\n']);
    let bytes = sql.as_bytes();
    if off >= bytes.len() {
        return String::new();
    }
    let mut start = 0usize;
    while off > 50 {
        off -= 1;
        start += 1;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
            off = off.saturating_sub(1);
        }
    }
    let rest = &bytes[start..];
    let mut len = rest.len();
    if len > 78 {
        len = 78;
        while len > 0 && (rest[len] & 0xc0) == 0x80 {
            len -= 1;
        }
    }
    let code: Vec<u8> = rest[..len]
        .iter()
        .map(|&c| {
            if c.is_ascii_whitespace() || c == 0x0b {
                b' '
            } else {
                c
            }
        })
        .collect();
    let code = String::from_utf8_lossy(&code);
    if off < 25 {
        format!("\n  {code}\n  {:off$}^--- error here", "")
    } else {
        format!("\n  {code}\n  {:w$}error here ---^", "", w = off - 14)
    }
}

/// Where the C library would report the error offset (`sqlite3_error_offset`) for the
/// messages that name a token, when the engine reported none. The engine does not record the
/// position of the offending identifier for these, so the first matching token of the
/// statement is taken: the first use of the column / function, or the name in a CREATE
/// statement. `None` for every other message (C has no offset for those either, e.g. "no such
/// table").
pub fn guess_error_offset(sql: &str, msg: &str) -> Option<usize> {
    use sqliters::syntax::{TokenKind, dequote, tokenize};
    enum Want<'m> {
        /// A (possibly qualified) column name.
        Column(&'m str),
        /// A function name followed by `(`.
        Function(&'m str),
        /// The object name of a CREATE statement.
        Created(&'m str),
    }
    let want = if let Some(rest) = msg.strip_prefix("no such column: ") {
        let name = rest.split(" - should this be").next().unwrap_or(rest);
        Want::Column(name.trim_matches('"'))
    } else if let Some(rest) = msg.strip_prefix("ambiguous column name: ") {
        Want::Column(rest)
    } else if let Some(rest) = msg.strip_prefix("no such function: ") {
        Want::Function(rest)
    } else if let Some(rest) = msg
        .strip_prefix("wrong number of arguments to function ")
        .or_else(|| msg.strip_prefix("misuse of aggregate function "))
        .or_else(|| msg.strip_prefix("misuse of window function "))
        .or_else(|| msg.strip_prefix("misuse of aggregate: "))
    {
        Want::Function(rest.strip_suffix("()")?)
    } else {
        let rest = msg.strip_suffix(" already exists")?;
        let name = rest
            .strip_prefix("table ")
            .or_else(|| rest.strip_prefix("index "))
            .or_else(|| rest.strip_prefix("view "))
            .or_else(|| rest.strip_prefix("trigger "))?;
        Want::Created(name)
    };
    let mut tokens = tokenize(sql).ok()?;
    // Only the statement that failed (the text starts with it).
    if let Some(semi) = tokens.iter().position(|t| t.kind == TokenKind::Semi) {
        tokens.truncate(semi);
    }
    let is_name = |i: usize, name: &str| -> bool {
        tokens.get(i).is_some_and(|t| {
            (t.kind == TokenKind::Id
                || t.kind == TokenKind::QuotedId
                || (t.kind != TokenKind::StringLiteral
                    && t.text
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    && !t.text.is_empty()
                    && !t.text.as_bytes()[0].is_ascii_digit()))
                && dequote(t.text).eq_ignore_ascii_case(name)
        })
    };
    match want {
        Want::Function(name) => (0..tokens.len())
            .find(|&i| {
                is_name(i, name) && tokens.get(i + 1).is_some_and(|t| t.kind == TokenKind::Lp)
            })
            .map(|i| tokens[i].offset),
        Want::Created(name) => (0..tokens.len())
            .find(|&i| is_name(i, name))
            .map(|i| tokens[i].offset),
        Want::Column(full) => {
            let parts: Vec<&str> = full.split('.').collect();
            (0..tokens.len())
                .find(|&i| {
                    parts.iter().enumerate().all(|(k, p)| {
                        is_name(i + 2 * k, p)
                            && (k == 0
                                || tokens
                                    .get(i + 2 * k - 1)
                                    .is_some_and(|t| t.kind == TokenKind::Dot))
                    }) && !tokens
                        .get(i + 2 * parts.len() - 1)
                        .is_some_and(|t| t.kind == TokenKind::Lp)
                })
                .map(|i| tokens[i].offset)
        }
    }
}

/// English ordinal as printf's `%r` renders it (1st, 2nd, 3rd, 4th, 11th ...).
pub fn ordinal(n: usize) -> String {
    let x = n % 10;
    let suffix = if x >= 4 || (n / 10) % 10 == 1 {
        "th"
    } else {
        ["th", "st", "nd", "rd"][x]
    };
    format!("{n}{suffix}")
}

/// Returns `true` when every statement in `sql` is a semicolon-terminated non-returning DML or
/// transaction-control statement (`INSERT`, `REPLACE`, `UPDATE`, `DELETE`, `BEGIN`, `COMMIT`,
/// `END`, `ROLLBACK`) containing no unquoted `RETURNING` keyword, so it never emits result rows or
/// column headers and can be executed directly via [`Connection::execute_batch`].
pub fn is_pure_non_returning_batch_sql(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut saw_stmt = false;

    loop {
        while i < bytes.len() {
            match bytes[i] {
                b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\x0b' | b';' => i += 1,
                b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                    i += 2;
                    let mut closed = false;
                    while i + 1 < bytes.len() {
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            closed = true;
                            break;
                        }
                        i += 1;
                    }
                    if !closed {
                        return false;
                    }
                }
                _ => break,
            }
        }
        if i >= bytes.len() {
            return saw_stmt;
        }

        if !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
            return false;
        }
        let kw_start = i;
        i += 1;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let kw = &bytes[kw_start..i];
        let is_dml = kw.eq_ignore_ascii_case(b"INSERT")
            || kw.eq_ignore_ascii_case(b"REPLACE")
            || kw.eq_ignore_ascii_case(b"UPDATE")
            || kw.eq_ignore_ascii_case(b"DELETE");
        let is_tcl = kw.eq_ignore_ascii_case(b"BEGIN")
            || kw.eq_ignore_ascii_case(b"COMMIT")
            || kw.eq_ignore_ascii_case(b"END")
            || kw.eq_ignore_ascii_case(b"ROLLBACK");
        if !is_dml && !is_tcl {
            return false;
        }
        saw_stmt = true;
        let mut terminated = false;

        while i < bytes.len() {
            match bytes[i] {
                b';' => {
                    i += 1;
                    terminated = true;
                    break;
                }
                b'\'' | b'"' | b'`' => {
                    let delim = bytes[i];
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
                        return false;
                    }
                }
                b'[' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b']' {
                        i += 1;
                    }
                    if i >= bytes.len() {
                        return false;
                    }
                    i += 1;
                }
                b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                    i += 2;
                    let mut closed = false;
                    while i + 1 < bytes.len() {
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            closed = true;
                            break;
                        }
                        i += 1;
                    }
                    if !closed {
                        return false;
                    }
                }
                c if c.is_ascii_alphabetic() || c == b'_' => {
                    let id_start = i;
                    i += 1;
                    while i < bytes.len()
                        && (bytes[i].is_ascii_alphanumeric()
                            || bytes[i] == b'_'
                            || bytes[i] == b'$')
                    {
                        i += 1;
                    }
                    if is_dml
                        && i - id_start == 9
                        && bytes[id_start..i].eq_ignore_ascii_case(b"RETURNING")
                    {
                        return false;
                    }
                }
                // A bound parameter: the shell binds those from `temp.sqlite_parameters`,
                // which the batch executor does not do.
                b'?' | b':' | b'@' | b'$' => return false,
                _ => i += 1,
            }
        }
        if !terminated {
            return false;
        }
    }
}

/// The keywords of the C library as built for the reference shell (`sqlite3_keyword_check`).
const KEYWORDS: &[&str] = &[
    "ABORT",
    "ACTION",
    "ADD",
    "AFTER",
    "ALL",
    "ALTER",
    "ALWAYS",
    "ANALYZE",
    "AND",
    "AS",
    "ASC",
    "ATTACH",
    "AUTOINCREMENT",
    "BEFORE",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "CONFLICT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATABASE",
    "DEFAULT",
    "DEFERRABLE",
    "DEFERRED",
    "DELETE",
    "DESC",
    "DETACH",
    "DISTINCT",
    "DO",
    "DROP",
    "EACH",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXCLUDE",
    "EXCLUSIVE",
    "EXISTS",
    "EXPLAIN",
    "FAIL",
    "FILTER",
    "FIRST",
    "FOLLOWING",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "GENERATED",
    "GLOB",
    "GROUP",
    "GROUPS",
    "HAVING",
    "IF",
    "IGNORE",
    "IMMEDIATE",
    "IN",
    "INDEX",
    "INDEXED",
    "INITIALLY",
    "INNER",
    "INSERT",
    "INSTEAD",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "KEY",
    "LAST",
    "LEFT",
    "LIKE",
    "LIMIT",
    "MATCH",
    "MATERIALIZED",
    "NATURAL",
    "NO",
    "NOT",
    "NOTHING",
    "NOTNULL",
    "NULL",
    "NULLS",
    "OF",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OTHERS",
    "OUTER",
    "OVER",
    "PARTITION",
    "PLAN",
    "PRAGMA",
    "PRECEDING",
    "PRIMARY",
    "QUERY",
    "RAISE",
    "RANGE",
    "RECURSIVE",
    "REFERENCES",
    "REGEXP",
    "REINDEX",
    "RELEASE",
    "RENAME",
    "REPLACE",
    "RESTRICT",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SAVEPOINT",
    "SELECT",
    "SET",
    "TABLE",
    "TEMP",
    "TEMPORARY",
    "THEN",
    "TIES",
    "TO",
    "TRANSACTION",
    "TRIGGER",
    "UNBOUNDED",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VACUUM",
    "VALUES",
    "VIEW",
    "VIRTUAL",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "WITHOUT",
];

/// `sqlite3_keyword_check()`.
pub fn keyword_check(name: &str) -> bool {
    name.len() <= 17 && KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(name))
}

/// `true` when the first keyword of `sql` (after white space and comments) is `kw`.
pub fn first_keyword_is(sql: &str, kw: &str) -> bool {
    let b = sql.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
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
        } else {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            return b[start..i].eq_ignore_ascii_case(kw.as_bytes());
        }
    }
    false
}
