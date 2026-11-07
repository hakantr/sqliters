//! Statement completion checker ported from `sqlite/src/complete.c` (`sqlite3_complete`).

/// Returns `true` if `sql` contains one or more complete SQL statements terminated by `;`
/// (and `; END;` when inside a `CREATE TRIGGER` body), ignoring semicolons inside comments,
/// string literals, and quoted identifiers.
pub fn is_complete(sql: &str) -> bool {
    // 8-state machine directly mirroring `trans[8][8]` in `sqlite/src/complete.c:154-165`.
    const TRANS: [[u8; 8]; 8] = [
        // SEMI WS OTHER EXPLAIN CREATE TEMP TRIGGER END
        [1, 0, 2, 3, 4, 2, 2, 2], // 0 NOTHING
        [1, 1, 2, 3, 4, 2, 2, 2], // 1 START (complete!)
        [1, 2, 2, 2, 2, 2, 2, 2], // 2 NORMAL
        [1, 3, 3, 2, 4, 2, 2, 2], // 3 EXPLAIN
        [1, 4, 2, 2, 2, 4, 5, 2], // 4 CREATE
        [6, 5, 5, 5, 5, 5, 5, 5], // 5 TRIGGER
        [6, 6, 5, 5, 5, 5, 5, 7], // 6 SEMI (in trigger)
        [1, 7, 5, 5, 5, 5, 5, 5], // 7 END (in trigger)
    ];

    const TK_SEMI: usize = 0;
    const TK_WS: usize = 1;
    const TK_OTHER: usize = 2;
    const TK_EXPLAIN: usize = 3;
    const TK_CREATE: usize = 4;
    const TK_TEMP: usize = 5;
    const TK_TRIGGER: usize = 6;
    const TK_END: usize = 7;

    let bytes = sql.as_bytes();
    let mut i = 0usize;
    let mut state = 0u8;

    while i < bytes.len() {
        let token = match bytes[i] {
            b';' => {
                i += 1;
                TK_SEMI
            }
            b' ' | b'\r' | b'\t' | b'\n' | b'\x0c' => {
                i += 1;
                TK_WS
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
                TK_WS
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                if i >= bytes.len() {
                    // Unterminated `--` comment without trailing newline is incomplete per complete.c:231!
                    return false;
                }
                i += 1;
                TK_WS
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
                TK_OTHER
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
                    return false;
                }
                TK_OTHER
            }
            c if is_id_char(c) => {
                let start = i;
                i += 1;
                while i < bytes.len() && is_id_char(bytes[i]) {
                    i += 1;
                }
                let word = &sql[start..i];
                if word.eq_ignore_ascii_case("EXPLAIN") {
                    TK_EXPLAIN
                } else if word.eq_ignore_ascii_case("CREATE") {
                    TK_CREATE
                } else if word.eq_ignore_ascii_case("TEMP")
                    || word.eq_ignore_ascii_case("TEMPORARY")
                {
                    TK_TEMP
                } else if word.eq_ignore_ascii_case("TRIGGER") {
                    TK_TRIGGER
                } else if word.eq_ignore_ascii_case("END") {
                    TK_END
                } else {
                    TK_OTHER
                }
            }
            _ => {
                i += 1;
                TK_OTHER
            }
        };
        state = TRANS[state as usize][token];
    }

    state == 1
}

#[inline]
pub const fn is_id_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}
