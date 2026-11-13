//! Dot-commands: a port of `do_meta_command()` and its helpers from `shell.c.in` (3.54.1).
//! The chain of prefix tests is kept in the order of the C source so that an abbreviated
//! command selects the same command as in the C shell.
#![allow(clippy::too_many_lines, clippy::cognitive_complexity)]

use std::io::{BufReader, Write};
use std::process::{Command, Stdio};

use sqliters::{Backup, BackupStepResult, Connection, OpenFlags, SQLITE_VERSION, Value};

use crate::qrf::{
    self, QRF_ALIGN_CENTER, QRF_ALIGN_LEFT, QRF_ALIGN_RIGHT, QRF_ALWAYS, QRF_AUTO, QRF_BLOB_AUTO,
    QRF_BLOB_SQL, QRF_MAX_WIDTH, QRF_NO, QRF_STYLE_BOX, QRF_STYLE_COLUMN, QRF_STYLE_INSERT,
    QRF_STYLE_MARKDOWN, QRF_STYLE_TABLE, QRF_TEXT_CSV, QRF_TEXT_HTML, QRF_TEXT_JSON,
    QRF_TEXT_PLAIN, QRF_TEXT_RELAXED, QRF_TEXT_SQL, QRF_TEXT_TCL, QRF_YES,
};
use crate::{
    A_MODE_INFO, A_MODE_STR, CLISTAT_ALL, CLISTAT_IO, CLISTAT_MEM, CLISTAT_ON, CLISTAT_RESET,
    CLISTAT_STMT, CLISTAT_VMSTEP, CLISTAT_XMEM, CLISTAT_XSTMT, CLISTAT_ZIPVFS, DFLT_CHAR_LIMIT,
    DFLT_LINE_LIMIT, DFLT_TITLE_LIMIT, MFLG_CRLF, MFLG_ECHO, MFLG_HDR, MODE_INSERT, MODE_N_USER,
    MODE_SPLIT, MODE_WWW, Redirect, SHFLG_COUNT_CHANGES, SHFLG_DUMP_DATA_ONLY, SHFLG_DUMP_NO_SYS,
    SHFLG_PRESERVE_ROWID, ShellState, StdioBuf, boolean_value, c_string, integer_value, is_space,
    process_input, resolve_backslashes, shell_exec, sqlite3_incomplete, text_of,
};

pub const QRF_ESC_NAMES: [&str; 4] = ["auto", "off", "ascii", "symbol"];
const QRF_QUOTE_NAMES: [&str; 8] = ["off", "off", "sql", "csv", "html", "tcl", "json", "relaxed"];

/// The `.dbconfig` options in the order of the C table.
const A_DBCONFIG: [&str; 22] = [
    "attach_create",
    "attach_write",
    "comments",
    "defensive",
    "dqs_ddl",
    "dqs_dml",
    "enable_fkey",
    "enable_qpsg",
    "enable_trigger",
    "enable_view",
    "fts3_tokenizer",
    "fp_digits",
    "legacy_alter_table",
    "legacy_file_format",
    "load_extension",
    "no_ckpt_on_close",
    "reset_database",
    "reverse_scanorder",
    "stmt_scanstatus",
    "trigger_eqp",
    "trusted_schema",
    "writable_schema",
];
pub const DBCFG_DQS_DDL: usize = 4;
/// Values of a freshly opened connection in the C shell (for the options that only exist as
/// shell-side copies here).
pub const DBCFG_DEFAULTS: [i32; 22] = [
    1, 1, 1, 1, 1, 1, 0, 0, 1, 1, 0, 17, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
];

const A_LIMIT: [&str; 15] = [
    "length",
    "sql_length",
    "column",
    "expr_depth",
    "parser_depth",
    "compound_select",
    "vdbe_op",
    "function_arg",
    "attached",
    "like_pattern_length",
    "variable_number",
    "trigger_depth",
    "worker_threads",
    "schema",
    "trigger_steps",
];
pub const LIMIT_DEFAULTS: [i32; 15] = [
    1_000_000_000,
    1_000_000_000,
    2000,
    1000,
    2500,
    500,
    250_000_000,
    1000,
    10,
    50000,
    32766,
    1000,
    0,
    10_000_000,
    65000,
];
/// Hard upper bounds (`aHardLimit[]` of main.c).
const LIMIT_MAX: [i32; 15] = [
    1_000_000_000,
    1_000_000_000,
    2000,
    1000,
    2500,
    500,
    250_000_000,
    1000,
    10,
    50000,
    32766,
    1000,
    8,
    10_000_000,
    65000,
];

/// `optionMatch()`.
fn option_match(z_str: &str, z_opt: &str) -> bool {
    let Some(mut z) = z_str.strip_prefix('-') else {
        return false;
    };
    if let Some(r) = z.strip_prefix('-') {
        z = r;
    }
    z == z_opt
}

/// `pickStr()`: index of `z_arg` in `choices`, or -1 (with the "should be ..." message).
fn pick_str(z_arg: &str, choices: &[&str]) -> Result<usize, String> {
    if let Some(i) = choices.iter().position(|c| *c == z_arg) {
        return Ok(i);
    }
    let n = choices.len();
    let mut msg = String::from("should be");
    for (i, c) in choices.iter().enumerate() {
        if i == n - 1 {
            msg.push_str(", or");
        } else if i > 0 {
            msg.push(',');
        }
        msg.push(' ');
        msg.push_str(c);
    }
    Err(msg)
}

/// `parseDotCmdArgs()`.
fn parse_dot_cmd_args(z_line: &[u8], p: &mut ShellState) {
    p.dot.z_orig = String::from_utf8_lossy(z_line).into_owned();
    let mut z = z_line.to_vec();
    let mut sz = z.len();
    while sz > 0 && is_space(z[sz - 1]) {
        sz -= 1;
    }
    if sz > 0 && z[sz - 1] == b';' {
        sz -= 1;
        while sz > 0 && is_space(z[sz - 1]) {
            sz -= 1;
        }
    }
    z.truncate(sz);
    p.dot.az_arg.clear();
    p.dot.ai_ofst.clear();
    p.dot.ab_quot.clear();
    let at = |z: &[u8], i: usize| z.get(i).copied().unwrap_or(0);
    let mut h = 1usize;
    while at(&z, h) != 0 {
        while is_space(at(&z, h)) {
            h += 1;
        }
        if at(&z, h) == 0 {
            break;
        }
        if at(&z, h) == b'\'' || at(&z, h) == b'"' {
            let delim = z[h];
            h += 1;
            let start = h;
            while at(&z, h) != 0 && z[h] != delim {
                if z[h] == b'\\' && delim == b'"' && at(&z, h + 1) != 0 {
                    h += 1;
                }
                h += 1;
            }
            let end = h;
            if at(&z, h) == delim {
                h += 1;
            }
            let raw = &z[start..end];
            let arg = if delim == b'"' {
                resolve_backslashes(raw)
            } else {
                raw.to_vec()
            };
            p.dot.ab_quot.push(true);
            p.dot
                .az_arg
                .push(String::from_utf8_lossy(&arg).into_owned());
            p.dot.ai_ofst.push(start);
        } else {
            let start = h;
            while at(&z, h) != 0 && !is_space(z[h]) {
                h += 1;
            }
            p.dot.ab_quot.push(false);
            p.dot
                .az_arg
                .push(String::from_utf8_lossy(&z[start..h]).into_owned());
            p.dot.ai_ofst.push(start);
            if at(&z, h) != 0 {
                h += 1;
            }
        }
    }
    p.dot.n_arg = p.dot.az_arg.len();
}

/// `strncmp(a, b, n)==0`.
fn pfx(arg0: &str, name: &str) -> bool {
    name.as_bytes().starts_with(arg0.as_bytes())
}

fn like(pat: &str, s: &str) -> bool {
    sqliters::vdbe::sqlite_like_match(pat, s, None)
}

fn like_esc(pat: &str, s: &str, esc: char) -> bool {
    sqliters::vdbe::sqlite_like_match(pat, s, Some(esc))
}

fn sql_q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn sql_w(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `quoteChar()`: true when the identifier needs double quotes.
fn quote_char(name: &str) -> bool {
    let b = name.as_bytes();
    if b.is_empty() || (!b[0].is_ascii_alphabetic() && b[0] != b'_') {
        return true;
    }
    if b.iter().any(|c| !c.is_ascii_alphanumeric() && *c != b'_') {
        return true;
    }
    crate::util::keyword_check(name)
}

fn append_text(out: &mut String, s: &str, quote: bool) {
    if quote {
        out.push_str(&sql_w(s));
    } else {
        out.push_str(s);
    }
}

/// Rows of a query as text (`sqlite3_column_text`), `None` for NULL.
fn query_text(db: &Connection, sql: &str) -> sqliters::Result<Vec<Vec<Option<String>>>> {
    let r = db.query(sql, &[])?;
    Ok(r.rows
        .iter()
        .map(|row| {
            row.values()
                .iter()
                .map(|v| {
                    if matches!(v, Value::Null) {
                        None
                    } else {
                        Some(text_of(v))
                    }
                })
                .collect()
        })
        .collect())
}

/// `db_int()`.
fn db_int(db: &Connection, sql: &str) -> i64 {
    match db.query_row(sql, &[]) {
        Ok(Some(row)) => match row.get(0) {
            Some(Value::Integer(i)) => *i,
            Some(Value::Real(r)) => *r as i64,
            Some(v @ Value::Text(_)) => i64::from(qrf::value_int(v)),
            _ => 0,
        },
        _ => 0,
    }
}

/// `appendCompletion()`.
fn append_completion(out: &mut String, z_base: &str, e_flags: i32) {
    let r = sqlite3_incomplete(z_base.as_bytes());
    let cc = ((r >> 16) & 0xff) as u8;
    let n_paren = (r >> 32) as i32;
    let e_semi = (r >> 8) & 0xff;
    let e_stat = r & 0xff;
    let b_semi = e_flags & 0x01 != 0;
    if cc == 0 {
    } else if cc == b'-' {
        if e_flags & 0x04 == 0 {
            out.push('\n');
        }
    } else if cc == b'/' {
        out.push_str("*/");
    } else {
        out.push(cc as char);
    }
    if n_paren > 0 {
        for _ in 0..n_paren {
            out.push(')');
        }
    }
    if e_stat == 0 || e_stat == 16 {
    } else if e_semi == 1 {
        if b_semi {
            out.push(';');
        }
    } else if e_semi == 2 {
        out.push_str(if b_semi { "END;" } else { "END" });
    } else if e_semi == 3 {
        out.push_str(if b_semi { ";END;" } else { ";END" });
    }
}

/// `shell_complete_sql(SQL, FLAGS)`.
fn shell_complete_sql(sql: &str, flags: i32) -> String {
    let mut out = String::new();
    if flags & 0x02 == 0 {
        out.push_str(sql);
    }
    append_completion(&mut out, sql, flags & 0x05);
    out
}

/// `shellFakeSchema()`.
fn shell_fake_schema(db: &Connection, z_schema: Option<&str>, z_name: &str) -> Option<String> {
    let sql = format!(
        "PRAGMA {}.table_info={};",
        sql_w(z_schema.unwrap_or("main")),
        sql_q(z_name)
    );
    let rows = query_text(db, &sql).unwrap_or_default();
    let mut s = String::new();
    if let Some(sc) = z_schema {
        let q = quote_char(sc) && !sc.eq_ignore_ascii_case("temp");
        append_text(&mut s, sc, q);
        s.push('.');
    }
    append_text(&mut s, z_name, quote_char(z_name));
    let mut div = "(";
    let mut n_row = 0;
    for r in &rows {
        let col = r.get(1).cloned().flatten().unwrap_or_default();
        n_row += 1;
        s.push_str(div);
        div = ",";
        append_text(&mut s, &col, quote_char(&col));
    }
    s.push(')');
    if n_row == 0 {
        return None;
    }
    while let Some(pos) = s.find("*/") {
        s.replace_range(pos + 1..pos + 2, "*");
    }
    Some(s)
}

/// `shell_add_schema(S,X,N)`.
fn shell_add_schema(
    db: &Connection,
    z_in: &str,
    z_schema: Option<&str>,
    z_name: Option<&str>,
) -> String {
    const PREFIX: [&str; 6] = [
        "TABLE",
        "INDEX",
        "UNIQUE INDEX",
        "VIEW",
        "TRIGGER",
        "VIRTUAL TABLE",
    ];
    if let Some(rest) = z_in.strip_prefix("CREATE ") {
        for pre in PREFIX {
            let n = pre.len();
            if rest.starts_with(pre) && rest.as_bytes().get(n) == Some(&b' ') {
                let mut out = String::new();
                if let Some(sc) = z_schema {
                    let head = &z_in[..n + 7];
                    let tail = &z_in[n + 8..];
                    if quote_char(sc) && !sc.eq_ignore_ascii_case("temp") {
                        out.push_str(&format!("{head} {}.{tail}", sql_w(sc)));
                    } else {
                        out.push_str(&format!("{head} {sc}.{tail}"));
                    }
                } else {
                    out.push_str(z_in);
                }
                let base = out.clone();
                append_completion(&mut out, &base, 1);
                if let Some(name) = z_name
                    && pre.starts_with('V')
                    && let Some(fake) = shell_fake_schema(db, z_schema, name)
                {
                    out.push_str(&format!("\n/* {fake} */"));
                }
                return out;
            }
        }
    }
    z_in.to_owned()
}

/// `wsToEol()`.
fn ws_to_eol(z: &[u8]) -> bool {
    let mut i = 0usize;
    while i < z.len() {
        if z[i] == b'\n' {
            return true;
        }
        if is_space(z[i]) {
            i += 1;
            continue;
        }
        return z[i] == b'-' && z.get(i + 1) == Some(&b'-');
    }
    true
}

/// `shell_format_schema(SQL,FLAGS)`.
fn shell_format_schema(z_sql: &str, flags: i32) -> String {
    if flags & 0x01 == 0 {
        return z_sql.to_owned();
    }
    if z_sql.is_empty() {
        return String::new();
    }
    if like("CREATE VIEW%", z_sql) || like("CREATE TRIG%", z_sql) {
        return z_sql.to_owned();
    }
    let is_index = like("CREATE INDEX%", z_sql) || like("CREATE UNIQUE INDEX%", z_sql);
    let src = z_sql.as_bytes();
    let mut z: Vec<u8> = Vec::with_capacity(src.len());
    let mut i = 0usize;
    while i < src.len() && is_space(src[i]) {
        i += 1;
    }
    while i < src.len() {
        let c = src[i];
        if is_space(c) {
            // (C reads z[j-1] even for j==0; the leading space was skipped, so j>0 here.)
            if let Some(last) = z.last_mut()
                && *last == b'\r'
            {
                *last = b'\n';
            }
            if z.last().is_none_or(|&l| is_space(l) || l == b'(') {
                i += 1;
                continue;
            }
        } else if (c == b'(' || c == b')') && z.last().is_some_and(|&l| is_space(l)) {
            z.pop();
        }
        z.push(c);
        i += 1;
    }
    while z.last().is_some_and(|&l| is_space(l)) {
        z.pop();
    }
    let mut out: Vec<u8> = Vec::new();
    if z.len() >= 79 {
        let zz = z.clone();
        let at = |i: usize| zz.get(i).copied().unwrap_or(0);
        let mut j = 0usize;
        let mut buf: Vec<u8> = vec![0; zz.len() + 1];
        let mut n_paren = 0i32;
        let mut c_end = 0u8;
        let mut n_line = 0;
        let mut is_where = false;
        let mut i = 0usize;
        while i < zz.len() {
            let c = zz[i];
            if c == c_end {
                c_end = 0;
            } else if c_end != 0 {
            } else if c == b'"' || c == b'\'' || c == b'`' {
                c_end = c;
            } else if c == b'[' {
                c_end = b']';
            } else if c == b'-' && at(i + 1) == b'-' {
                c_end = b'\n';
            } else if c == b'(' {
                n_paren += 1;
            } else if c == b')' {
                n_paren -= 1;
                if n_line > 0 && n_paren == 0 && j > 0 && !is_where {
                    out.extend_from_slice(&buf[..j]);
                    out.push(b'\n');
                    j = 0;
                }
            } else if (c == b'w' || c == b'W')
                && n_paren == 0
                && is_index
                && zz[i..].len() >= 5
                && zz[i..i + 5].eq_ignore_ascii_case(b"WHERE")
                && !at(i + 5).is_ascii_alphanumeric()
                && at(i + 5) != b'_'
            {
                is_where = true;
            } else if is_where
                && (c == b'A' || c == b'a')
                && n_paren == 0
                && zz[i..].len() >= 3
                && zz[i..i + 3].eq_ignore_ascii_case(b"AND")
                && !at(i + 3).is_ascii_alphanumeric()
                && at(i + 3) != b'_'
            {
                out.extend_from_slice(&buf[..j]);
                out.extend_from_slice(b"\n    ");
                j = 0;
            }
            buf[j] = c;
            j += 1;
            if n_paren == 1
                && c_end == 0
                && (c == b'(' || c == b'\n' || (c == b',' && !ws_to_eol(&zz[i + 1..])))
                && !is_where
            {
                if c == b'\n' {
                    j -= 1;
                }
                out.extend_from_slice(&buf[..j]);
                out.extend_from_slice(b"\n  ");
                j = 0;
                n_line += 1;
                while is_space(at(i + 1)) {
                    i += 1;
                }
            }
            i += 1;
        }
        out.extend_from_slice(&buf[..j]);
    } else {
        out = z;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `printSchemaLine()`.
fn print_schema_line(p: &mut ShellState, z: &str, tail: &str) {
    if sqliters::vdbe::sqlite_glob_match("CREATE TABLE ['\"]*", z) {
        p.outs(&format!("CREATE TABLE IF NOT EXISTS {}{tail}", &z[13..]));
    } else {
        p.outs(&format!("{z}{tail}"));
    }
}

/// `PRAGMA database_list` as (schema, file).
fn database_list(db: &Connection) -> sqliters::Result<Vec<(String, String)>> {
    let rows = query_text(db, "PRAGMA database_list")?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let name = r.get(1).cloned().flatten()?;
            let file = r.get(2).cloned().flatten()?;
            Some((name, file))
        })
        .collect())
}

/// Runs `sql` the way `shell_exec(p, zSql, 0)` does for an internal query (errors ignored).
fn exec_quiet(p: &mut ShellState, sql: &str) -> bool {
    shell_exec(p, sql).is_ok()
}

/// `showHelp()`.
fn show_help(p: &mut ShellState, z_pattern: Option<&str>) -> usize {
    use crate::help::{A_USAGE, AZ_HELP};
    let glob = sqliters::vdbe::sqlite_glob_match;
    let mut n = 0usize;
    let pat: String = match z_pattern {
        None => "[a-z]".to_owned(),
        Some("-a" | "-all" | "--all") => ".".to_owned(),
        Some("0") => {
            let mut show = false;
            for h in AZ_HELP {
                if h.starts_with('.') {
                    show = false;
                } else if let Some(rest) = h.strip_prefix(',') {
                    show = true;
                    p.outs(&format!(".{rest}\n"));
                    n += 1;
                } else if show {
                    p.outs(&format!("{h}\n"));
                }
            }
            return n;
        }
        Some(x) => x.to_owned(),
    };
    let z_pat = format!(".{}*", pat.strip_prefix('.').unwrap_or(&pat));
    let mut z_hit: Option<&str> = None;
    let mut j = 0usize;
    for (i, h) in AZ_HELP.iter().enumerate() {
        if glob(&z_pat, h) {
            if let Some(hit) = z_hit {
                p.outs(&format!("{hit}\n"));
            }
            z_hit = Some(h);
            j = i + 1;
            n += 1;
        }
    }
    if n > 0 {
        let hit = z_hit.unwrap_or("");
        if n == 1 {
            if let Some((_, usage)) = A_USAGE.iter().find(|(cmd, _)| glob(&z_pat, cmd)) {
                p.outs(usage);
            } else {
                p.outs(&format!("{hit}\n"));
                while j < AZ_HELP.len() - 1 && AZ_HELP[j].starts_with(' ') {
                    p.outs(&format!("{}\n", AZ_HELP[j]));
                    j += 1;
                }
            }
        } else {
            p.outs(&format!("{hit}\n"));
        }
        return n;
    }
    let z_pat = format!("%{pat}%");
    let mut i = 0usize;
    while i < AZ_HELP.len() {
        if AZ_HELP[i].starts_with(',') {
            while i < AZ_HELP.len() - 1 && AZ_HELP[i + 1].starts_with(' ') {
                i += 1;
            }
            i += 1;
            continue;
        }
        if AZ_HELP[i].starts_with('.') {
            j = i;
        }
        if like(&z_pat, AZ_HELP[i]) {
            p.outs(&format!("{}\n", AZ_HELP[j]));
            while j < AZ_HELP.len() - 1 && AZ_HELP[j + 1].starts_with(' ') {
                j += 1;
                p.outs(&format!("{}\n", AZ_HELP[j]));
            }
            i = j;
            n += 1;
        }
        i += 1;
    }
    n
}

/// `display_stats()`: the engine keeps none of the C library's memory / statement counters,
/// so only the labels that do not depend on them could be shown; nothing is printed.
pub fn display_stats(_p: &mut ShellState) {}

/// `modeTitleDsply()`.
fn mode_title_dsply(p: &ShellState, b_all: bool) -> u64 {
    let pi = &A_MODE_INFO[usize::from(p.mode.e_mode)];
    let mut b_t = p.mode.spec.b_titles;
    let mut e_t = p.mode.spec.e_title;
    let b_h = pi.b_hdr;
    let e_h = pi.e_hdr;
    let mut v: u64 = 0x0133_0133_1122_0102;
    if b_h == 0 {
        return 0;
    }
    if e_t == 0 {
        e_t = e_h;
    }
    if b_t == 0 {
        b_t = b_h;
    }
    if e_t != e_h {
        v >>= 32;
    }
    if b_t != b_h {
        v >>= 16;
    }
    if b_t < 2 {
        v >>= 8;
    }
    if !b_all {
        v >>= 4;
    }
    v &= 3;
    if p.mode.spec.b_titles == QRF_ALWAYS {
        if v == 3 {
            v = 5;
        }
        if v == 2 {
            v = 3;
        }
    } else if v == 3 {
        v = 4;
    }
    v
}

/// `dotCmdMode()`.
fn dot_cmd_mode(p: &mut ShellState) -> i32 {
    let n_arg = p.dot.n_arg;
    let az = p.dot.az_arg.clone();
    let mut i_mode: i32 = -1;
    let mut chng = false;
    let mut b_all = false;
    let mut i = 1usize;
    macro_rules! need_arg {
        ($at:expr) => {
            if i + 1 >= n_arg {
                p.dot_cmd_error($at, Some("missing argument"), None);
                return 1;
            }
        };
    }
    while i < n_arg {
        let z_full = az[i].as_str();
        let z = if z_full.starts_with("--") {
            &z_full[1..]
        } else {
            z_full
        };
        let e_mode = if !z.starts_with('-') && i_mode < 0 {
            p.mode_find(&az[i])
        } else {
            -1
        };
        if !z.starts_with('-') && i_mode < 0 && e_mode >= 0 && e_mode != i32::from(MODE_WWW) {
            i_mode = i as i32;
            p.mode_change(e_mode as u8);
            if i + 1 < n_arg && !az[i + 1].starts_with('-') {
                i += 1;
                p.mode.spec.z_table_name = Some(az[i].clone());
            }
            chng = true;
        } else if option_match(z, "align") {
            need_arg!(i);
            i += 1;
            let z_align = az[i].as_bytes();
            let n_align = z_align.len() & 0x3fff;
            let mut n_err = 0;
            let mut a = Vec::with_capacity(n_align);
            for &ch in &z_align[..n_align] {
                a.push(match ch {
                    b'l' | b'L' => QRF_ALIGN_LEFT,
                    b'c' | b'C' => QRF_ALIGN_CENTER,
                    b'r' | b'R' => QRF_ALIGN_RIGHT,
                    _ => {
                        n_err += 1;
                        0
                    }
                });
            }
            p.mode.spec.a_align = a;
            chng = true;
            if n_err != 0 {
                p.dot_cmd_error(
                    i,
                    Some("bad alignment string"),
                    Some("Should contain only characters L, C, and R."),
                );
                return 1;
            }
        } else if z == "-blob" || z == "-blob-quote" {
            i += 1;
            if i >= n_arg {
                p.dot_cmd_error(i - 1, Some("missing argument"), None);
                return 1;
            }
            if let Ok(k) = pick_str(
                &az[i],
                &["auto", "text", "sql", "hex", "tcl", "json", "size"],
            ) {
                p.mode.spec.e_blob = k as u8;
            }
            chng = true;
        } else if option_match(z, "border") {
            i += 1;
            if i >= n_arg {
                p.dot_cmd_error(i - 1, Some("missing argument"), None);
                return 1;
            }
            if let Ok(k) = pick_str(&az[i], &["auto", "off", "on"]) {
                p.mode.spec.b_border = k as u8 & 3;
            }
            chng = true;
        } else if let Ok(k) = pick_str(
            z,
            &["-charlimit", "-linelimit", "-titlelimit", "-multiinsert"],
        ) {
            need_arg!(i);
            i += 1;
            let w = integer_value(&az[i]) as i32;
            match k {
                0 => p.mode.spec.n_char_limit = w,
                1 => p.mode.spec.n_line_limit = w as i16,
                2 => p.mode.spec.n_title_limit = w as i16,
                _ => p.mode.spec.n_multi_insert = w as u32,
            }
            chng = true;
        } else if let Ok(k) = pick_str(z, &["-tablename", "-rowsep", "-colsep", "-null"]) {
            need_arg!(i);
            i += 1;
            let v = Some(az[i].clone());
            match k {
                0 => p.mode.spec.z_table_name = v,
                1 => p.mode.spec.z_row_sep = v,
                2 => p.mode.spec.z_column_sep = v,
                _ => p.mode.spec.z_null = v,
            }
            chng = true;
        } else if option_match(z, "escape") {
            i += 1;
            if i >= n_arg {
                p.dot_cmd_error(i - 1, Some("missing argument"), None);
                return 1;
            }
            match pick_str(&az[i], &["auto", "off", "ascii", "symbol"]) {
                Ok(k) => p.mode.spec.e_esc = k as u8,
                Err(e) => {
                    p.dot_cmd_error(i, Some("unknown escape type"), Some(&e));
                    return 1;
                }
            }
            chng = true;
        } else if option_match(z, "limits") {
            i += 1;
            if i >= n_arg {
                p.dot_cmd_error(i - 1, Some("missing argument"), None);
                return 1;
            }
            if p.mode_set_limit(Some(&az[i])) {
                p.dot_cmd_error(
                    i,
                    Some("bad argument"),
                    Some("Should be \"on\" or \"off\" or \"L,C,T\" where L, C, and T are unsigned integers"),
                );
                return 1;
            }
            chng = true;
        } else if option_match(z, "list") {
            let mut s = String::from("available modes:");
            for (ii, m) in A_MODE_INFO.iter().enumerate() {
                if ii == usize::from(MODE_WWW) {
                    continue;
                }
                s.push(' ');
                s.push_str(m.name);
            }
            for m in &p.saved_modes {
                s.push(' ');
                s.push_str(&m.0);
            }
            s.push_str(" batch tty\n");
            p.outs(&s);
            chng = true;
        } else if option_match(z, "once") {
            p.mode_push();
            p.n_pop_mode = 1;
        } else if option_match(z, "noquote") {
            p.mode.spec.e_text = QRF_TEXT_PLAIN;
            p.mode.spec.e_blob = QRF_BLOB_AUTO;
            chng = true;
        } else if option_match(z, "quote") {
            let k: usize;
            if i + 1 < n_arg
                && !az[i + 1].starts_with('-')
                && (i_mode > 0 || az[i + 1] == "off" || p.mode_find(&az[i + 1]) < 0)
            {
                i += 1;
                if let Ok(kk) = pick_str(&az[i], &["no", "yes", "0", "1"]) {
                    k = kk & 1;
                } else {
                    match pick_str(
                        &az[i],
                        &["off", "on", "sql", "csv", "html", "tcl", "json", "relaxed"],
                    ) {
                        Ok(kk) => k = kk,
                        Err(e) => {
                            p.dot_cmd_error(i, Some("unknown"), Some(&e));
                            return 1;
                        }
                    }
                }
            } else {
                k = 1;
            }
            match k {
                1 => {
                    p.mode.spec.z_null = Some("NULL".to_owned());
                    p.mode.spec.e_text = QRF_TEXT_SQL;
                }
                2 => p.mode.spec.e_text = QRF_TEXT_SQL,
                3 => p.mode.spec.e_text = QRF_TEXT_CSV,
                4 => p.mode.spec.e_text = QRF_TEXT_HTML,
                5 => p.mode.spec.e_text = QRF_TEXT_TCL,
                6 => p.mode.spec.e_text = QRF_TEXT_JSON,
                7 => p.mode.spec.e_text = QRF_TEXT_RELAXED,
                _ => p.mode.spec.e_text = QRF_TEXT_PLAIN,
            }
            chng = true;
        } else if option_match(z, "ifmt") || option_match(z, "fpfmt") {
            let is_i = option_match(z, "ifmt");
            need_arg!(i);
            i += 1;
            let want = if is_i { 1 } else { 2 };
            let v = if az[i].is_empty() || az[i] == "auto" {
                None
            } else if qrf::qrf_ckformat(&az[i]) != want {
                p.dot_cmd_error(
                    i,
                    Some(if is_i {
                        "not a valid integer format"
                    } else {
                        "not a valid floating-point format"
                    }),
                    None,
                );
                return 1;
            } else {
                Some(az[i].clone())
            };
            if is_i {
                p.mode.spec.z_ifmt = v;
            } else {
                p.mode.spec.z_fpfmt = v;
            }
            chng = true;
        } else if option_match(z, "reset") {
            let saved = p.mode.e_mode;
            p.mode_free();
            p.mode_change(saved);
        } else if option_match(z, "rowcount") {
            need_arg!(i);
            i += 1;
            p.mode.spec.b_row_count = if boolean_value(&az[i]) != 0 {
                QRF_YES
            } else {
                QRF_NO
            };
            chng = true;
        } else if option_match(z, "screenwidth") || option_match(z, "sw") {
            i += 1;
            if i >= n_arg {
                p.dot_cmd_error(i - 1, Some("missing argument"), None);
                return 1;
            }
            match az[i].as_str() {
                "off" => {
                    p.mode.b_auto_screen_width = false;
                    p.mode.spec.n_screen_width = 0;
                }
                "auto" => p.mode.b_auto_screen_width = true,
                other => {
                    let w = integer_value(other).clamp(0, QRF_MAX_WIDTH);
                    p.mode.b_auto_screen_width = false;
                    p.mode.spec.n_screen_width = w as i16;
                }
            }
            chng = true;
        } else if option_match(z, "tag") {
            need_arg!(i);
            i += 1;
            if p.mode_find(&az[i]) >= 0 {
                p.dot_cmd_error(i, Some("mode already exists"), None);
                return 1;
            }
            if p.saved_modes.len() > MODE_N_USER {
                p.dot_cmd_error(i - 1, Some("cannot add more modes"), None);
                return 1;
            }
            p.saved_modes.push((az[i].clone(), p.mode.clone()));
            chng = true;
        } else if option_match(z, "textjsonb") {
            need_arg!(i);
            i += 1;
            p.mode.spec.b_text_jsonb = if boolean_value(&az[i]) != 0 {
                QRF_YES
            } else {
                QRF_NO
            };
            chng = true;
        } else if option_match(z, "titles") || option_match(z, "title") {
            need_arg!(i);
            i += 1;
            let k = match pick_str(
                &az[i],
                &[
                    "off", "on", "always", "auto", "plain", "sql", "csv", "html", "tcl", "json",
                ],
            ) {
                Ok(k) => k,
                Err(e) => {
                    p.dot_cmd_error(i, Some("bad --titles value"), Some(&e));
                    return 1;
                }
            };
            p.mode.m_flags &= !MFLG_HDR;
            if k <= 2 {
                p.mode.spec.b_titles = QRF_NO + k as u8;
            } else if k == 3 {
                p.mode.spec.e_title = A_MODE_INFO[usize::from(p.mode.e_mode)].e_hdr;
                p.mode.spec.b_titles = A_MODE_INFO[usize::from(p.mode.e_mode)].b_hdr;
            } else {
                p.mode.spec.e_title = (k - 4) as u8 + QRF_TEXT_PLAIN;
                if p.mode.spec.b_titles == QRF_NO {
                    p.mode.spec.b_titles = QRF_ALWAYS;
                }
            }
            chng = true;
        } else if option_match(z, "widths") || option_match(z, "width") {
            need_arg!(i);
            i += 1;
            let zw = az[i].as_bytes();
            let at = |k: usize| zw.get(k).copied().unwrap_or(0);
            let mut pos = 0usize;
            let mut a_width: Vec<i16> = Vec::new();
            while is_space(at(pos)) {
                pos += 1;
            }
            while at(pos) != 0 {
                let mut w: i64 = 0;
                let mut n_digit = 0;
                let mut k = usize::from(at(pos) == b'-' && at(pos + 1).is_ascii_digit());
                while at(pos + k).is_ascii_digit() {
                    w = w * 10 + i64::from(at(pos + k) - b'0');
                    if w > QRF_MAX_WIDTH {
                        p.dot_cmd_error(
                            i + 1,
                            Some("width too big"),
                            Some(&format!("Maximum column width is {QRF_MAX_WIDTH}")),
                        );
                        return 1;
                    }
                    n_digit += 1;
                    k += 1;
                }
                if n_digit == 0 {
                    p.dot_cmd_error(
                        i + 1,
                        Some("syntax error"),
                        Some("should be a comma-separated list if integers"),
                    );
                    return 1;
                }
                if at(pos) == b'-' {
                    w = -w;
                }
                a_width.push(w as i16);
                pos += k;
                if at(pos) == b',' {
                    pos += 1;
                }
                while is_space(at(pos)) {
                    pos += 1;
                }
            }
            p.mode.spec.a_width = a_width;
            chng = true;
        } else if option_match(z, "wrap") {
            need_arg!(i);
            i += 1;
            let w =
                (integer_value(&az[i]) as i32).clamp(-(QRF_MAX_WIDTH as i32), QRF_MAX_WIDTH as i32);
            p.mode.spec.n_wrap = w as i16;
            chng = true;
        } else if option_match(z, "ww") {
            p.mode.spec.b_word_wrap = QRF_YES;
            chng = true;
        } else if option_match(z, "wordwrap") {
            need_arg!(i);
            i += 1;
            p.mode.spec.b_word_wrap = if boolean_value(&az[i]) as u8 != 0 {
                QRF_YES
            } else {
                QRF_NO
            };
            chng = true;
        } else if option_match(z, "v") || option_match(z, "verbose") {
            b_all = true;
        } else if z.starts_with('-') {
            p.dot_cmd_error(
                i,
                Some("bad option"),
                Some("Use \".help .mode\" for more info"),
            );
            return 1;
        } else {
            p.dot_cmd_error(
                i,
                Some(if i_mode > 0 {
                    "bad argument"
                } else {
                    "unknown mode"
                }),
                Some("Use \".help .mode\" for more info"),
            );
            return 1;
        }
        i += 1;
    }
    if !chng || b_all {
        let pi = &A_MODE_INFO[usize::from(p.mode.e_mode)];
        let sp = &p.mode.spec;
        let mut d = String::new();
        if p.n_pop_mode != 0 {
            d.push_str("--once ");
        }
        d.push_str(pi.name);
        if b_all || (!sp.a_align.is_empty() && pi.e_cx == 2) {
            d.push_str(" --align \"");
            for a in &sp.a_align {
                d.push(b"LLCR"[usize::from(a & 3)] as char);
            }
            d.push('"');
        }
        if b_all || (sp.b_border == QRF_NO) != (pi.m_flg & 1 != 0) {
            d.push_str(&format!(
                " --border {}",
                if sp.b_border == QRF_NO { "off" } else { "on" }
            ));
        }
        if b_all || sp.e_blob != QRF_BLOB_AUTO {
            const BQ: [&str; 7] = ["auto", "text", "sql", "hex", "tcl", "json", "size"];
            d.push_str(&format!(
                " --blob-quote {}",
                BQ[usize::from(sp.e_blob).min(6)]
            ));
        }
        let colsep = sp.z_column_sep.as_deref();
        let z_setting = A_MODE_STR[usize::from(pi.e_csep)];
        if b_all || z_setting.is_some_and(|s| Some(s) != colsep) {
            d.push_str(" --colsep ");
            d.push_str(&c_string(colsep.unwrap_or("")));
        }
        if b_all || sp.e_esc != QRF_AUTO {
            d.push_str(&format!(
                " --escape {}",
                QRF_ESC_NAMES[usize::from(sp.e_esc).min(3)]
            ));
        }
        if b_all || sp.z_fpfmt.is_some() {
            d.push_str(&format!(
                " --fpfmt {}",
                sp.z_fpfmt.as_deref().unwrap_or("auto")
            ));
        }
        if b_all || sp.z_ifmt.is_some() {
            d.push_str(&format!(
                " --ifmt {}",
                sp.z_ifmt.as_deref().unwrap_or("auto")
            ));
        }
        if b_all
            || (sp.n_line_limit > 0 && pi.e_cx > 0)
            || sp.n_char_limit > 0
            || (sp.n_title_limit > 0 && pi.e_cx > 0)
        {
            if sp.n_line_limit == 0 && sp.n_char_limit == 0 && sp.n_title_limit == 0 {
                d.push_str(" --limits off");
            } else if sp.n_line_limit == DFLT_LINE_LIMIT
                && sp.n_char_limit == DFLT_CHAR_LIMIT
                && sp.n_title_limit == DFLT_TITLE_LIMIT
            {
                d.push_str(" --limits on");
            } else {
                d.push_str(&format!(
                    " --limits {},{},{}",
                    sp.n_line_limit, sp.n_char_limit, sp.n_title_limit
                ));
            }
        }
        if b_all || (sp.n_multi_insert != 0 && sp.e_style == QRF_STYLE_INSERT) {
            d.push_str(&format!(" --multiinsert {}", sp.n_multi_insert));
        }
        let z_setting = A_MODE_STR[usize::from(pi.e_null)];
        if b_all || z_setting.is_some_and(|s| Some(s) != sp.z_null.as_deref()) {
            d.push_str(" --null ");
            d.push_str(&c_string(sp.z_null.as_deref().unwrap_or("")));
        }
        if b_all || (pi.e_text != sp.e_text && (pi.e_text > 1 || sp.e_text > 1)) {
            d.push_str(&format!(
                " --quote {}",
                QRF_QUOTE_NAMES[usize::from(sp.e_text).min(7)]
            ));
        }
        if b_all || ((sp.b_row_count == QRF_YES) != (pi.m_flg & 4 != 0)) {
            d.push_str(&format!(
                " --rowcount {}",
                if sp.b_row_count == QRF_YES {
                    "on"
                } else {
                    "off"
                }
            ));
        }
        let z_setting = A_MODE_STR[usize::from(pi.e_rsep)];
        if b_all || z_setting.is_some_and(|s| Some(s) != sp.z_row_sep.as_deref()) {
            d.push_str(" --rowsep ");
            d.push_str(&c_string(sp.z_row_sep.as_deref().unwrap_or("")));
        }
        if b_all || (pi.e_cx != 0 && (sp.n_screen_width > 0 || p.mode.b_auto_screen_width)) {
            if p.mode.b_auto_screen_width {
                d.push_str(" --sw auto");
            } else {
                d.push_str(&format!(" --sw {}", sp.n_screen_width));
            }
        }
        if b_all || p.mode.e_mode == MODE_INSERT {
            d.push_str(" --tablename ");
            d.push_str(&c_string(sp.z_table_name.as_deref().unwrap_or("")));
        }
        if b_all || sp.b_text_jsonb != 0 {
            d.push_str(&format!(
                " --textjsonb {}",
                if sp.b_text_jsonb == QRF_YES {
                    "on"
                } else {
                    "off"
                }
            ));
        }
        let k = mode_title_dsply(p, b_all);
        if k == 1 {
            d.push_str(" --titles off");
        } else if k == 2 {
            d.push_str(" --titles on");
        } else if k == 3 {
            d.push_str(" --titles always");
        } else if k >= 4 {
            const TT: [&str; 6] = ["plain", "sql", "csv", "html", "tcl", "json"];
            d.push_str(&format!(
                " --titles {}",
                TT[usize::from(sp.e_title.max(1) - 1).min(5)]
            ));
            if k == 5 {
                d.push_str(" --titles always");
            }
        }
        if !sp.a_width.is_empty() && (b_all || pi.e_cx == 2) {
            let mut sep = " --widths ";
            for w in &sp.a_width {
                d.push_str(&format!("{sep}{w}"));
                sep = ",";
            }
        } else if b_all {
            d.push_str(" --widths \"\"");
        }
        if b_all || (pi.e_cx > 0 && sp.b_word_wrap != 0) {
            if b_all {
                d.push_str(&format!(
                    " --wordwrap {}",
                    if sp.b_word_wrap == QRF_YES {
                        "on"
                    } else {
                        "off"
                    }
                ));
            }
            if sp.n_wrap != 0 {
                d.push_str(&format!(" --wrap {}", sp.n_wrap));
            }
            if !b_all {
                d.push_str(" --ww");
            }
        }
        p.outs(&format!(".mode {d}\n"));
        let _ = p.out().flush();
    }
    0
}

/// `output_file_open()` for a plain file name.
fn open_out_file(z_file: &str) -> Option<std::fs::File> {
    match std::fs::File::create(z_file) {
        Ok(f) => Some(f),
        Err(_) => {
            eprintln!("Error: cannot open \"{z_file}\"");
            None
        }
    }
}

/// `dotCmdOutput()` (`.output`, `.once`; the `-e`/`-w`/`-x` variants that launch other
/// programs are not available).
fn dot_cmd_output(p: &mut ShellState) -> i32 {
    let n_arg = p.dot.n_arg;
    let az = p.dot.az_arg.clone();
    let c = az[0].as_bytes()[0];
    let n = az[0].len();
    if p.fail_if_safe_mode(&format!("cannot run .{} in safe mode", az[0])) {
        return 1;
    }
    let mut b_once = 0;
    if c == b'e' || c == b'w' {
        eprintln!("Error: .{} is not supported by this shell", az[0]);
        return 1;
    } else if n >= 2 && pfx(&az[0], "once") {
        b_once = 1;
    }
    let mut z_file: Option<String> = None;
    let mut b_keep = false;
    let mut bom = false;
    let mut i = 1usize;
    while i < n_arg {
        let z_full = az[i].as_str();
        if z_full.starts_with('-') {
            let z = if z_full.starts_with("--") {
                &z_full[1..]
            } else {
                z_full
            };
            if z == "-bom" {
                bom = true;
            } else if z == "-plain" {
            } else if c == b'o' && matches!(z, "-e" | "-w" | "-x") {
                eprintln!("Error: .{} {z} is not supported by this shell", az[0]);
                return 1;
            } else if z == "-show" {
                if let Some(Redirect::Memory(buf)) = &p.redirect {
                    let b = buf.clone();
                    let _ = p.stdout.write_all(&b);
                }
            } else if z == "-keep" {
                b_keep = true;
            } else if option_match(z, "error-prefix") {
                if i + 1 >= n_arg {
                    p.dot_cmd_error(i, Some("missing argument"), None);
                    return 1;
                }
                i += 1;
                p.z_err_prefix = if az[i].is_empty() {
                    None
                } else {
                    Some(az[i].clone())
                };
            } else {
                p.dot_cmd_error(i, Some("unknown option"), None);
                return 1;
            }
        } else if z_file.is_none() {
            if b_keep {
                p.dot_cmd_error(i, Some("incompatible with prior options"), None);
                return 1;
            }
            if z_full == "memory" && b_once != 0 {
                p.dot_cmd_error(0, Some("cannot redirect to \"memory\""), None);
                return 1;
            }
            let mut f = if z_full == "off" {
                "/dev/null".to_owned()
            } else {
                z_full.to_owned()
            };
            if f.starts_with('|') {
                while i + 1 < n_arg {
                    i += 1;
                    f.push(' ');
                    f.push_str(&az[i]);
                }
                z_file = Some(f);
                break;
            }
            z_file = Some(f);
        } else {
            p.dot_cmd_error(i, Some("surplus argument"), None);
            return 1;
        }
        i += 1;
    }
    if z_file.is_none() && !b_keep {
        z_file = Some("stdout".to_owned());
    }
    p.n_pop_output = if b_once != 0 { 2 } else { 0 };
    if !b_keep {
        p.output_reset();
    }
    let Some(z_file) = z_file else { return 0 };
    if z_file == "memory" {
        p.redirect = Some(Redirect::Memory(Vec::new()));
        return 0;
    }
    if let Some(cmd) = z_file.strip_prefix('|') {
        match Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let _ = p.stdout.flush();
                let stdin = child.stdin.take();
                p.pipe_out = stdin.map(|s| StdioBuf::new(s, 4096));
                p.redirect = Some(Redirect::Pipe(child));
                if bom {
                    p.out_bytes(b"\xef\xbb\xbf");
                }
                p.outfile = z_file.clone();
            }
            Err(_) => {
                eprintln!("Error: cannot open pipe \"{cmd}\"");
                return 1;
            }
        }
    } else if z_file == "stdout" {
        if bom {
            p.out_bytes(b"\xef\xbb\xbf");
        }
        p.outfile = z_file;
    } else if z_file == "stderr" {
        // (Standard error as the output channel: not buffered separately here.)
        p.outfile = z_file;
    } else if p.b_safe_mode {
        return 1;
    } else {
        match open_out_file(&z_file) {
            None => {
                eprintln!("Error: cannot write to \"{z_file}\"");
                return 1;
            }
            Some(f) => {
                p.redirect = Some(Redirect::File(std::io::BufWriter::with_capacity(4096, f)));
                if bom {
                    p.out_bytes(b"\xef\xbb\xbf");
                }
                p.outfile = z_file;
            }
        }
    }
    0
}

/// `tableColumnList()`: `(rowid name to preserve, column names)`.
fn table_column_list(p: &ShellState, z_tab: &str) -> Option<(Option<&'static str>, Vec<String>)> {
    let db = p.db();
    let rows = query_text(db, &format!("PRAGMA table_info={}", sql_q(z_tab))).ok()?;
    let mut cols = Vec::new();
    let mut n_pk = 0;
    let mut is_ipk = false;
    for r in &rows {
        cols.push(r.get(1).cloned().flatten().unwrap_or_default());
        let pk = r
            .get(5)
            .cloned()
            .flatten()
            .and_then(|x| x.parse::<i64>().ok())
            .unwrap_or(0);
        if pk != 0 {
            n_pk += 1;
            is_ipk = n_pk == 1
                && r.get(2)
                    .cloned()
                    .flatten()
                    .unwrap_or_default()
                    .eq_ignore_ascii_case("INTEGER");
        }
    }
    if cols.is_empty() {
        return None;
    }
    let mut preserve = p.has_flag(SHFLG_PRESERVE_ROWID);
    if preserve && is_ipk {
        let sql = format!(
            "SELECT 1 FROM pragma_index_list({}) WHERE origin='pk'",
            sql_q(z_tab)
        );
        match db.query_row(&sql, &[]) {
            Ok(r) => preserve = r.is_some(),
            Err(_) => return None,
        }
    }
    let mut rowid = None;
    if preserve {
        for cand in ["rowid", "_rowid_", "oid"] {
            if !cols.iter().any(|c| c.eq_ignore_ascii_case(cand)) {
                let probe = format!("SELECT {cand} FROM {} LIMIT 0", sql_w(z_tab));
                if db.prepare(&probe).map(|s| s.finalize()).is_ok() {
                    rowid = Some(cand);
                }
                break;
            }
        }
    }
    Some((rowid, cols))
}

/// `dump_callback()`.
fn dump_callback(p: &mut ShellState, z_table: &str, z_type: &str, z_sql: &str) {
    let data_only = p.has_flag(SHFLG_DUMP_DATA_ONLY);
    let no_sys = p.has_flag(SHFLG_DUMP_NO_SYS);
    if z_table == "sqlite_sequence" && !no_sys {
        if db_int(p.db(), "SELECT count(*) FROM sqlite_sequence") > 0 {
            if !p.writable_schema {
                p.outs("PRAGMA writable_schema=ON;\n");
                p.writable_schema = true;
            }
            p.outs("CREATE TABLE IF NOT EXISTS sqlite_sequence(name,seq);\nDELETE FROM sqlite_sequence;\n");
        }
    } else if sqliters::vdbe::sqlite_glob_match("sqlite_stat?", z_table) && !no_sys {
        if !data_only {
            p.outs("ANALYZE sqlite_schema;\n");
        }
    } else if z_table.starts_with("sqlite_") {
        return;
    } else if data_only {
    } else if z_sql.starts_with("CREATE VIRTUAL TABLE") {
        if !p.writable_schema {
            p.outs("PRAGMA writable_schema=ON;\n");
            p.writable_schema = true;
        }
        let q = |s: &str| s.replace('\'', "''");
        p.outs(&format!(
            "INSERT INTO sqlite_schema(type,name,tbl_name,rootpage,sql)VALUES('table','{}','{}',0,'{}');\n",
            q(z_table),
            q(z_table),
            q(z_sql)
        ));
        return;
    } else {
        print_schema_line(p, z_sql, ";\n");
    }
    if z_type == "table" {
        let Some((rowid, cols)) = table_column_list(p, z_table) else {
            p.n_err += 1;
            return;
        };
        let mut s_select = String::from("SELECT ");
        if let Some(r) = rowid {
            s_select.push_str(r);
            s_select.push(',');
        }
        for (i, c) in cols.iter().enumerate() {
            append_text(&mut s_select, c, quote_char(c));
            if i + 1 < cols.len() {
                s_select.push(',');
            }
        }
        s_select.push_str(" FROM ");
        append_text(&mut s_select, z_table, quote_char(z_table));
        let saved = p.mode.clone();
        p.mode.spec.z_table_name = Some(z_table.to_owned());
        p.mode.e_mode = MODE_INSERT;
        p.mode.spec.e_text = QRF_TEXT_SQL;
        p.mode.spec.e_blob = QRF_BLOB_SQL;
        p.mode.spec.b_titles = QRF_NO;
        p.mode.spec.n_char_limit = 0;
        let rc = shell_exec(p, &s_select);
        if let Err(e) = &rc
            && (e.err.code as i32 & 0xff) == 11
        {
            p.outs("/****** CORRUPTION ERROR *******/\n");
        }
        p.mode = saved;
        if rc.is_err() {
            p.n_err += 1;
        }
    }
}

/// `.dump`.
fn dot_cmd_dump(p: &mut ShellState) -> i32 {
    let n_arg = p.dot.n_arg;
    let az = p.dot.az_arg.clone();
    let saved_flags = p.shell_flgs;
    p.shell_flgs &= !(SHFLG_PRESERVE_ROWID | SHFLG_DUMP_DATA_ONLY | SHFLG_DUMP_NO_SYS);
    let mut z_like: Option<String> = None;
    for i in 1..n_arg {
        if az[i].starts_with('-') {
            let mut z = &az[i][1..];
            if let Some(r) = z.strip_prefix('-') {
                z = r;
            }
            match z {
                "preserve-rowids" => p.shell_flgs |= SHFLG_PRESERVE_ROWID,
                "newlines" => {}
                "data-only" => p.shell_flgs |= SHFLG_DUMP_DATA_ONLY,
                "nosys" => p.shell_flgs |= SHFLG_DUMP_NO_SYS,
                _ => {
                    p.dot_cmd_error(i, Some("unknown option"), None);
                    return 1;
                }
            }
        } else {
            let a = sql_q(&az[i]);
            let expr = format!(
                "name LIKE {a} ESCAPE '\\' OR EXISTS (  SELECT 1 FROM sqlite_schema WHERE     name LIKE {a} ESCAPE '\\' AND    sql LIKE 'CREATE VIRTUAL TABLE%' AND    substr(o.name, 1, length(name)+1) == (name||'_'))"
            );
            z_like = Some(match z_like {
                Some(prev) => format!("{prev} OR {expr}"),
                None => expr,
            });
        }
    }
    if !p.open_db(false) {
        return 1;
    }
    let saved_mode = p.mode.clone();
    // outputDumpWarning()
    {
        let sql = format!(
            "SELECT 1 FROM sqlite_schema o WHERE sql LIKE 'CREATE VIRTUAL TABLE%' AND ({})",
            z_like.as_deref().unwrap_or("true")
        );
        match p.db().query_row(&sql, &[]) {
            Ok(Some(_)) => {
                p.outs(
                    "/* WARNING: Script requires that SQLITE_DBCONFIG_DEFENSIVE be disabled */\n",
                );
            }
            Ok(None) => {}
            // shellPrepare()
            Err(e) => eprintln!("sql error: {} ({})", e.message, e.code as i32),
        }
    }
    let data_only = p.has_flag(SHFLG_DUMP_DATA_ONLY);
    if !data_only {
        p.outs("PRAGMA foreign_keys=OFF;\n");
        p.outs("BEGIN TRANSACTION;\n");
    }
    p.writable_schema = false;
    p.mode.spec.b_titles = QRF_NO;
    let _ = p
        .db()
        .execute_batch("SAVEPOINT dump; PRAGMA writable_schema=ON");
    p.n_err = 0;
    let z_like = z_like.unwrap_or_else(|| "true".to_owned());
    let sql = format!(
        "SELECT name, type, sql FROM sqlite_schema AS o WHERE ({z_like}) AND type=='table'  AND sql NOT NULL ORDER BY tbl_name='sqlite_sequence', rowid"
    );
    match query_text(p.db(), &sql) {
        Ok(rows) => {
            for r in rows {
                let (Some(name), Some(ty), Some(sq)) = (
                    r.first().cloned().flatten(),
                    r.get(1).cloned().flatten(),
                    r.get(2).cloned().flatten(),
                ) else {
                    continue;
                };
                let sq = shell_complete_sql(&sq, 0);
                dump_callback(p, &name, &ty, &sq);
            }
        }
        Err(e) => {
            if (e.code as i32 & 0xff) == 11 {
                p.outs("/****** CORRUPTION ERROR *******/\n");
                p.outs(&format!("/****** {} ******/\n", e.message));
            }
        }
    }
    if !data_only {
        let sql = format!(
            "SELECT sql FROM sqlite_schema AS o WHERE ({z_like}) AND sql NOT NULL  AND type IN ('index','trigger','view') ORDER BY type COLLATE NOCASE DESC"
        );
        // run_table_dump_query()
        match query_text(p.db(), &sql) {
            Ok(rows) => {
                for r in rows {
                    let z =
                        shell_complete_sql(&r.first().cloned().flatten().unwrap_or_default(), 0);
                    p.outs(&z);
                    if z.contains("--") {
                        p.outs("\n;\n");
                    } else {
                        p.outs(";\n");
                    }
                }
            }
            Err(e) => {
                let rc = e.code as i32;
                p.outs(&format!("/**** ERROR: ({rc}) {} *****/\n", e.message));
                if (rc & 0xff) != 11 {
                    p.n_err += 1;
                }
            }
        }
    }
    if p.writable_schema {
        p.outs("PRAGMA writable_schema=OFF;\n");
        p.writable_schema = false;
    }
    let _ = p.db().execute_batch("PRAGMA writable_schema=OFF;");
    let _ = p.db().execute_batch("RELEASE dump;");
    if !data_only {
        p.outs(if p.n_err != 0 {
            ";\nROLLBACK; -- due to errors\n"
        } else {
            "COMMIT;\n"
        });
    }
    p.shell_flgs = saved_flags;
    p.mode = saved_mode;
    i32::from(p.n_err > 0)
}

/// One row of the union that `.schema` builds over all attached databases.
struct SchemaRow {
    sql: Option<String>,
    tbl_name: String,
    name: String,
    rowid: f64,
    snum: i32,
    sname: String,
}

/// `.schema`.
fn dot_cmd_schema(p: &mut ShellState) -> i32 {
    let n_arg = p.dot.n_arg;
    let az = p.dot.az_arg.clone();
    if !p.open_db(false) {
        return 1;
    }
    let mut z_name: Option<String> = None;
    let mut b_debug = false;
    let mut b_no_sys = false;
    let mut b_indent = 0;
    for a in az.iter().take(n_arg).skip(1) {
        if option_match(a, "indent") {
            b_indent = 1;
        } else if option_match(a, "debug") {
            b_debug = true;
        } else if option_match(a, "nosys") {
            b_no_sys = true;
        } else if a.starts_with('-') {
            eprintln!("Unknown option: \"{a}\"");
            return 1;
        } else if z_name.is_none() {
            z_name = Some(a.clone());
        } else {
            eprintln!("Usage: .schema ?--indent? ?--nosys? ?LIKE-PATTERN?");
            return 1;
        }
    }
    if let Some(name) = &z_name {
        let is_schema = like_esc(name, "sqlite_master", '\\')
            || like_esc(name, "sqlite_schema", '\\')
            || like_esc(name, "sqlite_temp_master", '\\')
            || like_esc(name, "sqlite_temp_schema", '\\');
        if is_schema {
            p.outs(&format!(
                "CREATE TABLE {}sqlite_schema (\n  type text,\n  name text,\n  tbl_name text,\n  rootpage integer,\n  sql text\n);\n",
                if like("sqlite_t%", name) { "temp." } else { "" }
            ));
        }
    }
    let dbs = match database_list(p.db()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: {}", e.message);
            return 1;
        }
    };
    if b_debug {
        p.outs("SQL: (not shown: the schema query is evaluated by the shell);\n");
        return 0;
    }
    let mut rows: Vec<SchemaRow> = Vec::new();
    let mut i_schema = 0;
    for (z_db, _) in &dbs {
        i_schema += 2;
        let sql = format!(
            "SELECT sql, tbl_name, name, rowid FROM {}.sqlite_schema",
            sql_w(z_db)
        );
        let res = match p.db().query(&sql, &[]) {
            Ok(r) => r,
            Err(e) => {
                // The C shell runs one statement over all schemas; its failure is reported as
                // save_err_msg(db, "in prepare", rc, zSql) through shellEmitError().
                let rc = e.code as i32;
                if rc > 1 {
                    eprintln!("Error: in prepare, {} ({rc})", e.message);
                } else {
                    eprintln!("Error: in prepare, {}", e.message);
                }
                return 1;
            }
        };
        for r in &res.rows {
            let v = r.values();
            let raw_sql = match &v[0] {
                Value::Null => None,
                x => Some(text_of(x)),
            };
            let name = text_of(&v[2]);
            // shell_format_schema(NULL, 1) is '' (an empty row), with flags 0 it stays NULL.
            let sql2 = match raw_sql {
                Some(s) => {
                    let sc = if z_db.eq_ignore_ascii_case("main") {
                        None
                    } else {
                        Some(z_db.as_str())
                    };
                    Some(shell_format_schema(
                        &shell_add_schema(p.db(), &s, sc, Some(&name)),
                        b_indent,
                    ))
                }
                None if b_indent != 0 => Some(String::new()),
                None => None,
            };
            rows.push(SchemaRow {
                sql: sql2,
                tbl_name: text_of(&v[1]),
                name,
                rowid: match &v[3] {
                    Value::Integer(i) => *i as f64,
                    _ => 0.0,
                },
                snum: i_schema,
                sname: z_db.clone(),
            });
        }
    }
    if z_name.is_some()
        && let Ok(mods) = query_text(p.db(), "SELECT name FROM pragma_module_list")
    {
        for m in mods {
            let Some(name) = m.first().cloned().flatten() else {
                continue;
            };
            let fake = shell_fake_schema(p.db(), None, &name).map(|f| format!("/* {f} */"));
            rows.push(SchemaRow {
                sql: fake,
                tbl_name: name.clone(),
                name: name.clone(),
                rowid: 0.0,
                snum: i32::MAX,
                sname: "main".to_owned(),
            });
        }
    }
    rows.sort_by(|a, b| {
        a.snum.cmp(&b.snum).then(
            a.rowid
                .partial_cmp(&b.rowid)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    for r in &rows {
        if let Some(name) = &z_name {
            let subject = if name.contains('.') {
                // The C shell builds this operand with "lower(format('%%s.%%s',sname,tbl_name))"
                // (the doubled percent signs reach SQL unchanged), which evaluates to the
                // constant '%s.%s' whatever the schema and table are.
                let _ = &r.sname;
                "%s.%s".to_owned()
            } else {
                r.tbl_name.to_ascii_lowercase()
            };
            let b_glob = name.contains('*') || name.contains('?') || name.contains('[');
            let ok = if b_glob {
                sqliters::vdbe::sqlite_glob_match(name, &subject)
            } else {
                like_esc(name, &subject, '\\')
            };
            if !ok {
                continue;
            }
        }
        if b_no_sys && like_esc("sqlite__%", &r.name, '_') {
            continue;
        }
        let Some(sql) = &r.sql else { continue };
        let line = shell_complete_sql(sql, 1);
        p.outs(&line);
        p.outs("\n");
    }
    0
}

/// `.dbinfo` (`shell_dbinfo_command`).
fn dot_cmd_dbinfo(p: &mut ShellState) -> i32 {
    let z_db = if p.dot.n_arg >= 2 {
        p.dot.az_arg[1].clone()
    } else {
        "main".to_owned()
    };
    if !p.open_db(false) {
        return 1;
    }
    if !z_db.eq_ignore_ascii_case("main") {
        match database_list(p.db()) {
            Ok(l) if l.iter().any(|(n, _)| n.eq_ignore_ascii_case(&z_db)) => {}
            _ => {
                eprintln!("unable to read database header");
                return 1;
            }
        }
    }
    let (hdr, pages) = match (p.db().database_header(), p.db().page_count()) {
        (Ok(h), Ok(n)) => (h, n),
        _ => {
            eprintln!("unable to read database header");
            return 1;
        }
    };
    if pages == 0 {
        eprintln!("unable to read database header");
        return 1;
    }
    let mut s = String::new();
    s.push_str(&format!(
        "{:<20} {}\n",
        "database page size:", hdr.page_size
    ));
    s.push_str(&format!("{:<20} {}\n", "write format:", hdr.write_version));
    s.push_str(&format!("{:<20} {}\n", "read format:", hdr.read_version));
    s.push_str(&format!(
        "{:<20} {}\n",
        "reserved bytes:", hdr.reserved_bytes
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "file change counter:", hdr.change_counter
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "database page count:", hdr.database_size_pages
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "freelist page count:", hdr.total_freelist_pages
    ));
    s.push_str(&format!("{:<20} {}\n", "schema cookie:", hdr.schema_cookie));
    s.push_str(&format!("{:<20} {}\n", "schema format:", hdr.schema_format));
    s.push_str(&format!(
        "{:<20} {}\n",
        "default cache size:", hdr.default_cache_size as u32
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "autovacuum top root:", hdr.largest_root_btree_page
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "incremental vacuum:", hdr.incremental_vacuum_mode
    ));
    let enc = match hdr.text_encoding {
        1 => " (utf8)",
        2 => " (utf16le)",
        3 => " (utf16be)",
        _ => "",
    };
    s.push_str(&format!(
        "{:<20} {}{enc}\n",
        "text encoding:", hdr.text_encoding
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "user version:", hdr.user_version as u32
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "application id:", hdr.application_id as u32
    ));
    s.push_str(&format!(
        "{:<20} {}\n",
        "software version:", hdr.sqlite_version_number
    ));
    let tab = if z_db == "temp" {
        "sqlite_temp_schema".to_owned()
    } else {
        format!("{}.sqlite_schema", sql_w(&z_db))
    };
    for (name, q) in [
        (
            "number of tables:",
            "SELECT count(*) FROM %s WHERE type='table'",
        ),
        (
            "number of indexes:",
            "SELECT count(*) FROM %s WHERE type='index'",
        ),
        (
            "number of triggers:",
            "SELECT count(*) FROM %s WHERE type='trigger'",
        ),
        (
            "number of views:",
            "SELECT count(*) FROM %s WHERE type='view'",
        ),
        ("schema size:", "SELECT total(length(sql)) FROM %s"),
    ] {
        let v = db_int(p.db(), &q.replace("%s", &tab));
        s.push_str(&format!("{name:<20} {v}\n"));
    }
    let dv = db_int(p.db(), "PRAGMA data_version");
    s.push_str(&format!("{:<20} {}\n", "data version", dv));
    p.outs(&s);
    0
}

/// `.backup` / `.save` and `.restore`: copies with the online backup API.
fn run_backup(
    src: &Connection,
    src_db: &str,
    dst: &Connection,
    dst_db: &str,
    retry_busy: bool,
) -> Result<(), (i32, String)> {
    let mut bk = Backup::init(src, src_db, dst, dst_db).map_err(|e| (e.code as i32, e.message))?;
    let mut n_timeout = 0;
    let rc: Result<(), (i32, String)> = loop {
        match bk.step(100) {
            Ok(BackupStepResult::Done) => break Ok(()),
            Ok(_) => {}
            Err(e) => {
                let code = e.code as i32;
                if retry_busy && code == 5 {
                    if n_timeout >= 3 {
                        break Err((code, e.message));
                    }
                    n_timeout += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
                break Err((code, e.message));
            }
        }
    };
    let _ = bk.finish();
    rc
}

/// `.import` argument: C `sqlite3_table_column_metadata(db, zSchema, zTable, 0, ...)`.
pub fn table_exists(db: &Connection, schema: Option<&str>, table: &str) -> bool {
    let sc = schema.unwrap_or("main");
    let sql = format!(
        "SELECT count(*) FROM pragma_table_info({},{})",
        sql_q(table),
        sql_q(sc)
    );
    if schema.is_none() {
        // Any schema: main, temp, attached.
        let sql = format!("SELECT count(*) FROM pragma_table_info({})", sql_q(table));
        if db_int(db, &sql) == 0 {
            return false;
        }
        let v = format!(
            "SELECT count(*) FROM sqlite_schema WHERE name={} COLLATE nocase AND type='view'",
            sql_q(table)
        );
        return db_int(db, &v) == 0
            || db_int(
                db,
                &format!(
                    "SELECT count(*) FROM sqlite_schema WHERE name={} COLLATE nocase AND type='table'",
                    sql_q(table)
                ),
            ) > 0;
    }
    if db_int(db, &sql) == 0 {
        return false;
    }
    let v = format!(
        "SELECT count(*) FROM {}.sqlite_schema WHERE name={} COLLATE nocase AND type='view'",
        sql_w(sc),
        sql_q(table)
    );
    db_int(db, &v) == 0
}

/// `do_meta_command()`: 1 on error, 2 to exit, 0 otherwise.
pub fn do_meta_command(z_line: &[u8], p: &mut ShellState) -> i32 {
    parse_dot_cmd_args(z_line, p);
    let n_arg = p.dot.n_arg;
    if n_arg == 0 {
        return 0;
    }
    let az = p.dot.az_arg.clone();
    let a0 = az[0].as_str();
    let n = a0.len();
    let c = a0.as_bytes().first().copied().unwrap_or(0);
    let mut rc = meta_command_body(p, &az, a0, n, c, n_arg);
    if p.n_pop_output != 0 {
        p.n_pop_output -= 1;
        if p.n_pop_output == 0 {
            p.output_reset();
        }
    }
    p.b_safe_mode = p.b_safe_mode_persist;
    p.dot.n_arg = 0;
    if p.exit_code.is_some() && rc == 0 {
        rc = 1;
    }
    rc
}

fn unsupported(p: &mut ShellState, name: &str) -> i32 {
    let _ = p.stdout.flush();
    eprintln!("Error: the \".{name}\" command is not supported by this shell");
    1
}

fn meta_command_body(
    p: &mut ShellState,
    az: &[String],
    a0: &str,
    n: usize,
    c: u8,
    n_arg: usize,
) -> i32 {
    let mut rc = 0;
    if c == b'a' && pfx(a0, "auth") {
        return unsupported(p, "auth");
    } else if (c == b'b' && n >= 3 && pfx(a0, "backup")) || (c == b's' && n >= 3 && pfx(a0, "save"))
    {
        let mut z_dest: Option<&str> = None;
        let mut z_db: Option<&str> = None;
        let mut b_async = false;
        if p.fail_if_safe_mode(&format!("cannot run .{a0} in safe mode")) {
            return 1;
        }
        for (j, zf) in az.iter().enumerate().take(n_arg).skip(1) {
            if zf.starts_with('-') {
                let z = if zf.starts_with("--") {
                    &zf[1..]
                } else {
                    zf.as_str()
                };
                if z == "-append" {
                    eprintln!("Error: the appendvfs is not supported by this shell");
                    return 1;
                } else if z == "-async" {
                    b_async = true;
                } else {
                    p.dot_cmd_error(
                        j,
                        Some("unknown option"),
                        Some("should be -append or -async"),
                    );
                    return 1;
                }
            } else if z_dest.is_none() {
                z_dest = Some(zf);
            } else if z_db.is_none() {
                z_db = z_dest;
                z_dest = Some(zf);
            } else {
                eprintln!("Usage: .backup ?DB? ?OPTIONS? FILENAME");
                return 1;
            }
        }
        let Some(z_dest) = z_dest else {
            eprintln!("missing FILENAME argument on .backup");
            return 1;
        };
        let z_db = z_db.unwrap_or("main");
        let dest = match Connection::open_with_flags(
            z_dest,
            OpenFlags(OpenFlags::READWRITE.0 | OpenFlags::CREATE.0 | OpenFlags::URI.0),
        ) {
            Ok(d) => d,
            Err(_) => {
                eprintln!("Error: cannot open \"{z_dest}\"");
                return 1;
            }
        };
        if b_async {
            let _ = dest.execute_batch("PRAGMA synchronous=OFF; PRAGMA journal_mode=OFF;");
        }
        if !p.open_db(false) {
            return 1;
        }
        match run_backup(p.db(), z_db, &dest, "main", false) {
            Ok(()) => rc = 0,
            Err((_, msg)) => {
                eprintln!("Error: {msg}");
                rc = 1;
            }
        }
        let _ = dest.try_close();
    } else if c == b'b' && n >= 3 && pfx(a0, "bail") {
        if n_arg == 2 {
            p.bail_on_error = boolean_value(&az[1]) != 0;
        } else {
            eprintln!("Usage: .bail on|off");
            rc = 1;
        }
    } else if c == b'b' && n >= 3 && pfx(a0, "binary") {
        eprintln!("The \".binary\" command is deprecated.");
        rc = 1;
    } else if c == b'b' && n >= 3 && pfx(a0, "breakpoint") {
    } else if c == b'c' && a0 == "cd" {
        if p.fail_if_safe_mode("cannot run .cd in safe mode") {
            return 1;
        }
        if n_arg == 2 {
            if std::env::set_current_dir(&az[1]).is_err() {
                eprintln!("Cannot change to directory \"{}\"", az[1]);
                rc = 1;
            }
        } else {
            eprintln!("Usage: .cd DIRECTORY");
            rc = 1;
        }
    } else if c == b'c' && n >= 3 && pfx(a0, "changes") {
        if n_arg == 2 {
            if boolean_value(&az[1]) != 0 {
                p.shell_flgs |= SHFLG_COUNT_CHANGES;
            } else {
                p.shell_flgs &= !SHFLG_COUNT_CHANGES;
            }
        } else {
            eprintln!("Usage: .changes on|off");
            rc = 1;
        }
    } else if c == b'c' && n >= 3 && pfx(a0, "check") {
        return unsupported(p, "check");
    } else if c == b'c' && pfx(a0, "clone") {
        return unsupported(p, "clone");
    } else if c == b'c' && pfx(a0, "connection") {
        if n_arg == 1 {
            for i in 0..p.aux.len() {
                let active = p.i_aux == i;
                let open = if active { true } else { p.aux[i].db.is_some() };
                let z_file = if !open {
                    "(not open)".to_owned()
                } else {
                    match p.aux[i].z_db_filename.as_deref() {
                        None => "(memory)".to_owned(),
                        Some("") => "(temporary-file)".to_owned(),
                        Some(f) => f.to_owned(),
                    }
                };
                if active {
                    p.stdouts(&format!("ACTIVE {i}: {z_file}\n"));
                } else if p.aux[i].db.is_some() {
                    p.stdouts(&format!("       {i}: {z_file}\n"));
                }
            }
        } else if n_arg == 2 && az[1].len() == 1 && az[1].as_bytes()[0].is_ascii_digit() {
            let i = usize::from(az[1].as_bytes()[0] - b'0');
            if p.i_aux != i && i < p.aux.len() {
                let cur = p.i_aux;
                p.aux[cur].db = p.db.take();
                p.i_aux = i;
                p.db = p.aux[i].db.take();
            }
        } else if n_arg == 3
            && az[1] == "close"
            && az[2].len() == 1
            && az[2].as_bytes()[0].is_ascii_digit()
        {
            let i = usize::from(az[2].as_bytes()[0] - b'0');
            if i >= p.aux.len() {
            } else if p.i_aux == i {
                eprintln!("cannot close the active database connection");
                rc = 1;
            } else if let Some(db) = p.aux[i].db.take() {
                let _ = db.try_close();
            }
        } else {
            eprintln!("Usage: .connection [close] [CONNECTION-NUMBER]");
            rc = 1;
        }
    } else if c == b'c' && n == 4 && (a0 == "crlf" || a0 == "crnl") {
        if n_arg == 2 {
            p.mode.m_flags &= !MFLG_CRLF;
        }
        eprintln!(
            "crlf is {}",
            if p.mode.m_flags & MFLG_CRLF != 0 {
                "ON"
            } else {
                "OFF"
            }
        );
    } else if c == b'd' && n > 1 && pfx(a0, "databases") {
        if !p.open_db(false) {
            return 1;
        }
        match database_list(p.db()) {
            Err(e) => {
                eprintln!("Error: {}", e.message);
                rc = 1;
            }
            Ok(list) => {
                let ro = p.db().is_readonly();
                let in_txn = !p.db().is_autocommit();
                for (i, (name, file)) in list.iter().enumerate() {
                    // (Transaction state per schema is not exposed by the engine: a write
                    // transaction is reported for main while a BEGIN is open.)
                    let txn = if in_txn && i == 0 { " write-txn" } else { "" };
                    let rdonly = ro && i == 0;
                    p.outs(&format!(
                        "{name}: {} {}{txn}\n",
                        if file.is_empty() {
                            "\"\""
                        } else {
                            file.as_str()
                        },
                        if rdonly { "r/o" } else { "r/w" }
                    ));
                }
            }
        }
    } else if c == b'd' && n >= 3 && pfx(a0, "dbconfig") {
        if !p.open_db(false) {
            return 1;
        }
        let mut found = false;
        for (ii, name) in A_DBCONFIG.iter().enumerate() {
            if n_arg > 1 && az[1] != *name {
                continue;
            }
            found = true;
            let is_fp = *name == "fp_digits";
            let set: Option<i32> = if n_arg >= 3 {
                Some(if is_fp {
                    crate::atoi(&az[2])
                } else {
                    boolean_value(&az[2])
                })
            } else {
                None
            };
            let v = dbconfig_access(p, ii, set);
            if is_fp {
                p.outs(&format!("{name:>19} {v}\n"));
            } else {
                p.outs(&format!(
                    "{name:>19} {}\n",
                    if v != 0 { "on" } else { "off" }
                ));
            }
            if n_arg > 1 {
                break;
            }
        }
        if n_arg > 1 && !found {
            p.dot_cmd_error(
                1,
                Some("unknown dbconfig"),
                Some("Enter \".dbconfig\" with no arguments for a list"),
            );
        }
    } else if c == b'd' && n >= 3 && pfx(a0, "dbinfo") {
        rc = dot_cmd_dbinfo(p);
    } else if c == b'd' && n >= 3 && pfx(a0, "dbtotxt") {
        return unsupported(p, "dbtotxt");
    } else if c == b'd' && n == 8 && pfx(a0, "diskused") {
        return unsupported(p, "diskused");
    } else if c == b'd' && pfx(a0, "dump") {
        rc = dot_cmd_dump(p);
    } else if c == b'e' && pfx(a0, "echo") {
        if n_arg == 2 {
            if boolean_value(&az[1]) != 0 {
                p.mode.m_flags |= MFLG_ECHO;
            } else {
                p.mode.m_flags &= !MFLG_ECHO;
            }
        } else {
            eprintln!("Usage: .echo on|off");
            rc = 1;
        }
    } else if c == b'e' && pfx(a0, "eqp") {
        if n_arg == 2 {
            p.mode.auto_eqp = match az[1].as_str() {
                "full" => 3,
                "trigger" => 2,
                other => boolean_value(other) as u8,
            };
        } else {
            eprintln!("Usage: .eqp off|on|trace|trigger|full");
            rc = 1;
        }
    } else if c == b'e' && pfx(a0, "exit") {
        if n_arg > 1 {
            let code = integer_value(&az[1]) as i32;
            if code != 0 {
                p.exit_code = Some(code);
                return 2;
            }
        }
        rc = 2;
    } else if c == b'e' && pfx(a0, "explain") {
        if n_arg >= 2 {
            p.mode.auto_explain = if az[1] == "auto" {
                1
            } else {
                boolean_value(&az[1]) as u8
            };
        }
    } else if c == b'e' && pfx(a0, "expert") {
        return unsupported(p, "expert");
    } else if c == b'f' && pfx(a0, "filectrl") {
        return unsupported(p, "filectrl");
    } else if c == b'f' && pfx(a0, "fullschema") {
        let mut na = n_arg;
        if na == 2 && option_match(&az[1], "indent") {
            na = 1;
        }
        if na != 1 {
            eprintln!("Usage: .fullschema ?--indent?");
            return 1;
        }
        if !p.open_db(false) {
            return 1;
        }
        let sql = "SELECT sql FROM  (SELECT sql sql, type type, tbl_name tbl_name, name name, rowid x     FROM sqlite_schema UNION ALL   SELECT sql, type, tbl_name, name, rowid FROM sqlite_temp_schema) WHERE type!='meta' AND sql NOTNULL  AND name NOT LIKE 'sqlite__%' ESCAPE '_' ORDER BY x";
        let ok = match query_text(p.db(), sql) {
            Ok(rows) => {
                for r in rows {
                    let s =
                        shell_complete_sql(&r.first().cloned().flatten().unwrap_or_default(), 0);
                    p.outs(&s);
                    p.outs(";\n");
                }
                true
            }
            Err(_) => false,
        };
        if ok {
            let mut has = [false; 5];
            let mut do_stats = false;
            if let Ok(rows) = query_text(
                p.db(),
                "SELECT substr(name,12,1) FROM sqlite_schema WHERE name GLOB 'sqlite_stat[134]'",
            ) {
                for r in rows {
                    if let Some(k) = r
                        .first()
                        .cloned()
                        .flatten()
                        .and_then(|x| x.parse::<usize>().ok())
                        && k < 5
                    {
                        has[k] = true;
                        do_stats = true;
                    }
                }
            }
            if !do_stats {
                p.outs("/* No STAT tables available */\n");
            } else {
                p.outs("ANALYZE sqlite_schema;\n");
                let saved = p.mode.clone();
                p.mode.spec.b_titles = QRF_NO;
                p.mode.e_mode = MODE_INSERT;
                p.mode.spec.e_text = QRF_TEXT_PLAIN;
                p.mode.spec.n_char_limit = 0;
                p.mode.spec.z_row_sep = Some(";\n".to_owned());
                if has[1] {
                    p.mode.spec.z_table_name = Some("sqlite_stat1".to_owned());
                    exec_quiet(p, "SELECT * FROM sqlite_stat1");
                }
                if has[4] {
                    p.mode.spec.z_table_name = Some("sqlite_stat4".to_owned());
                    exec_quiet(p, "SELECT * FROM sqlite_stat4");
                }
                p.mode = saved;
                p.outs("ANALYZE sqlite_schema;\n");
            }
        }
    } else if c == b'h' && pfx(a0, "headers") {
        if n_arg == 2 {
            if az[1] == "always" {
                p.mode.spec.b_titles = QRF_ALWAYS;
            } else {
                p.mode.spec.b_titles = if boolean_value(&az[1]) != 0 {
                    QRF_YES
                } else {
                    QRF_NO
                };
            }
            p.mode.m_flags |= MFLG_HDR;
            p.mode.spec.e_title = A_MODE_INFO[usize::from(p.mode.e_mode)].e_hdr;
        } else {
            eprintln!("Usage: .headers off|on|always");
            rc = 1;
        }
    } else if c == b'h' && pfx(a0, "help") {
        if n_arg >= 2 {
            if show_help(p, Some(&az[1])) == 0 {
                p.outs(&format!("Nothing matches '{}'\n", az[1]));
            }
        } else {
            show_help(p, None);
        }
    } else if c == b'i' && pfx(a0, "import") {
        rc = crate::import::dot_cmd_import(p);
    } else if c == b'i' && pfx(a0, "imposter") {
        return unsupported(p, "imposter");
    } else if c == b'i' && (pfx(a0, "indices") || pfx(a0, "indexes")) {
        let mut all_flag = false;
        let mut sys_flag = false;
        let mut expr_flag = false;
        let mut z_pattern: Option<&str> = None;
        for (i, a) in az.iter().enumerate().take(n_arg).skip(1) {
            if a.starts_with('-') {
                let mut z = &a[1..];
                if let Some(r) = z.strip_prefix('-') {
                    z = r;
                }
                match z {
                    "all" | "a" => all_flag = true,
                    "sys" => {
                        sys_flag = true;
                        all_flag = false;
                    }
                    "expr" => {
                        expr_flag = true;
                        all_flag = false;
                    }
                    "debug" => {}
                    _ => {
                        p.dot_cmd_error(i, Some("unknown option"), None);
                        return 1;
                    }
                }
            } else if z_pattern.is_none() {
                z_pattern = Some(a);
            } else {
                p.dot_cmd_error(i, Some("unknown argument"), None);
                return 1;
            }
        }
        if !p.open_db(false) {
            return 1;
        }
        let mut sql = String::from(
            "SELECT if(t.schema='main',i.name,t.schema||'.'||i.name)\nFROM pragma_table_list t, pragma_index_list(t.name,t.schema) i\n",
        );
        let mut sep = "WHERE";
        if expr_flag {
            all_flag = false;
            sql.push_str(&format!(
                "{sep} (EXISTS(SELECT 1 FROM pragma_index_xinfo(i.name) WHERE cid=-2)\n       OR\n       EXISTS(SELECT cid FROM pragma_table_xinfo(t.name) WHERE hidden=2 INTERSECT  SELECT cid FROM pragma_index_info(i.name)))\n"
            ));
            sep = "AND";
        }
        if sys_flag {
            sql.push_str(&format!(
                "{sep} i.name LIKE 'sqlite__autoindex__%' ESCAPE '_'\n"
            ));
            sep = "AND";
        } else if !all_flag {
            sql.push_str(&format!("{sep} i.name NOT LIKE 'sqlite__%' ESCAPE '_'\n"));
            sep = "AND";
        }
        if let Some(pat) = z_pattern {
            sql.push_str(&format!(
                "{sep} i.name LIKE '%{}%'\n",
                pat.replace('\'', "''")
            ));
        }
        sql.push_str("ORDER BY 1");
        p.mode_push();
        p.mode_change(MODE_SPLIT);
        exec_quiet(p, &sql);
        p.mode_pop();
    } else if c == b'i' && pfx(a0, "intck") {
        return unsupported(p, "intck");
    } else if c == b'l' && n >= 5 && pfx(a0, "limits") {
        if !p.open_db(false) {
            return 1;
        }
        if n_arg == 1 {
            for (i, name) in A_LIMIT.iter().enumerate() {
                let v = limit_access(p, i, None);
                p.stdouts(&format!("{name:>20} {v}\n"));
            }
        } else if n_arg > 3 {
            eprintln!("Usage: .limit NAME ?NEW-VALUE?");
            return 1;
        } else {
            let mut i_limit: Option<usize> = None;
            for (i, name) in A_LIMIT.iter().enumerate() {
                let n2 = az[1].len();
                if name.len() >= n2 && name.as_bytes()[..n2].eq_ignore_ascii_case(az[1].as_bytes())
                {
                    if i_limit.is_none() {
                        i_limit = Some(i);
                    } else {
                        eprintln!("ambiguous limit: \"{}\"", az[1]);
                        return 1;
                    }
                }
            }
            let Some(i_limit) = i_limit else {
                eprintln!(
                    "unknown limit: \"{}\"\nenter \".limits\" with no arguments for a list.",
                    az[1]
                );
                return 1;
            };
            if n_arg == 3 {
                limit_access(p, i_limit, Some(integer_value(&az[2]) as i32));
            } else {
                let v = limit_access(p, i_limit, None);
                p.stdouts(&format!("{:>20} {v}\n", A_LIMIT[i_limit]));
            }
        }
    } else if c == b'l' && n > 2 && pfx(a0, "lint") {
        return unsupported(p, "lint");
    } else if c == b'l' && pfx(a0, "load") {
        return unsupported(p, "load");
    } else if c == b'l' && pfx(a0, "log") {
        if n_arg != 2 {
            eprintln!("Usage: .log FILENAME");
            rc = 1;
        } else {
            return unsupported(p, "log");
        }
    } else if c == b'm' && pfx(a0, "mode") {
        rc = dot_cmd_mode(p);
    } else if c == b'n' && a0 == "nonce" {
        if n_arg != 2 {
            eprintln!("Usage: .nonce NONCE");
            rc = 1;
        } else {
            eprintln!("line {}: incorrect nonce: \"{}\"", p.lineno, az[1]);
            p.exit_code = Some(1);
            return 1;
        }
    } else if c == b'n' && pfx(a0, "nullvalue") {
        if n_arg == 2 {
            p.mode.spec.z_null = Some(az[1].clone());
        } else {
            eprintln!("Usage: .nullvalue STRING");
            rc = 1;
        }
    } else if c == b'o' && pfx(a0, "open") && n >= 2 {
        let mut z_fn: Option<&str> = None;
        let mut new_flag = false;
        let mut readonly = p.b_safe_mode;
        let mut create = !p.b_safe_mode;
        let mut nofollow = false;
        let mut deser = false;
        let mut i_name = 1usize;
        while i_name < n_arg {
            let z = az[i_name].as_str();
            if option_match(z, "new") {
                new_flag = true;
            } else if option_match(z, "append") && !p.b_safe_mode {
                eprintln!("Error: the appendvfs is not supported by this shell");
                return 1;
            } else if option_match(z, "readonly") {
                readonly = true;
                create = false;
            } else if option_match(z, "exclusive") {
            } else if option_match(z, "ifexists") {
                create = false;
            } else if option_match(z, "nofollow") {
                nofollow = true;
            } else if option_match(z, "deserialize") {
                deser = true;
            } else if option_match(z, "hexdb") {
                eprintln!("Error: --hexdb is not supported by this shell");
                return 1;
            } else if option_match(z, "normal") {
            } else if option_match(z, "maxsize") && i_name + 1 < n_arg {
                i_name += 1;
            } else if z.starts_with('-') {
                eprintln!("unknown option: {z}");
                return 1;
            } else if z_fn.is_some() {
                eprintln!("extra argument: \"{z}\"");
                return 1;
            } else {
                z_fn = Some(z);
            }
            i_name += 1;
        }
        if let Some(db) = p.db.take()
            && let Err(e) = db.try_close()
        {
            eprintln!("Error: sqlite3_close() returns 5: {e}");
        }
        let cur = p.i_aux;
        p.aux[cur].z_db_filename = None;
        p.open_readonly = readonly;
        p.open_create = create;
        p.open_flags_set = true;
        p.open_nofollow = nofollow;
        p.open_deserialize = deser;
        if let Some(f) = z_fn {
            if new_flag && !p.b_safe_mode {
                let del = f
                    .strip_prefix("file:")
                    .map_or(f, |r| r.split('?').next().unwrap_or(r));
                let _ = std::fs::remove_file(del);
                let _ = std::fs::remove_file(format!("{del}-journal"));
                let _ = std::fs::remove_file(format!("{del}-wal"));
                let _ = std::fs::remove_file(format!("{del}-shm"));
            }
            if p.b_safe_mode
                && f != ":memory:"
                && p.fail_if_safe_mode("cannot open disk-based database files in safe mode")
            {
                return 1;
            }
            p.aux[cur].z_db_filename = Some(f.to_owned());
            p.open_db(true);
        }
        if p.db.is_none() {
            p.aux[cur].z_db_filename = None;
            if !p.open_db(false) {
                return 1;
            }
        }
    } else if (c == b'o' && (pfx(a0, "output") || pfx(a0, "once")))
        || (c == b'e' && a0 == "excel")
        || (c == b'w' && a0 == "www")
    {
        rc = dot_cmd_output(p);
    } else if c == b'p' && n >= 3 && pfx(a0, "parameter") {
        if !p.open_db(false) {
            return 1;
        }
        let bind_table_init = |p: &mut ShellState| {
            let db = p.db();
            let def = db.db_config_defensive(None);
            db.db_config_defensive(Some(false));
            let wr = db.db_config_writable_schema(None);
            db.db_config_writable_schema(Some(true));
            let _ = db.execute_batch(
                "CREATE TABLE IF NOT EXISTS temp.sqlite_parameters(\n  key TEXT PRIMARY KEY,\n  value\n) WITHOUT ROWID;",
            );
            db.db_config_writable_schema(Some(wr));
            db.db_config_defensive(Some(def));
        };
        if n_arg == 2 && az[1] == "clear" {
            let _ = p
                .db()
                .execute_batch("DROP TABLE IF EXISTS temp.sqlite_parameters;");
        } else if n_arg == 2 && az[1] == "list" {
            let len = db_int(
                p.db(),
                "SELECT max(length(key)) FROM temp.sqlite_parameters;",
            )
            .min(40);
            if len > 0
                && let Ok(rows) = query_text(
                    p.db(),
                    "SELECT key, quote(value) FROM temp.sqlite_parameters;",
                )
            {
                for r in rows {
                    let k = r.first().cloned().flatten().unwrap_or_default();
                    let v = r.get(1).cloned().flatten().unwrap_or_default();
                    p.outs(&format!("{k:<w$} {v}\n", w = len as usize));
                }
            }
        } else if n_arg == 2 && az[1] == "init" {
            bind_table_init(p);
        } else if n_arg == 4 && az[1] == "set" {
            bind_table_init(p);
            let key = sql_q(&az[2]);
            let sql1 = format!(
                "REPLACE INTO temp.sqlite_parameters(key,value)VALUES({key},{});",
                az[3]
            );
            let db = p.db();
            let ok = match db.prepare(&sql1) {
                Ok(mut st) => {
                    let _ = st.step();
                    let _ = st.finalize();
                    true
                }
                Err(_) => false,
            };
            if !ok {
                let sql2 = format!(
                    "REPLACE INTO temp.sqlite_parameters(key,value)VALUES({key},{});",
                    sql_q(&az[3])
                );
                match db.prepare(&sql2) {
                    Ok(mut st) => {
                        let _ = st.step();
                        let _ = st.finalize();
                    }
                    Err(e) => {
                        let m = e.message.clone();
                        p.outs(&format!("Error: {m}\n"));
                        rc = 1;
                    }
                }
            }
        } else if n_arg == 3 && az[1] == "unset" {
            let sql = format!(
                "DELETE FROM temp.sqlite_parameters WHERE key={}",
                sql_q(&az[2])
            );
            let _ = p.db().execute_batch(&sql);
        } else {
            show_help(p, Some("parameter"));
        }
    } else if c == b'p' && n >= 3 && pfx(a0, "print") {
        let mut s = String::new();
        for (i, a) in az.iter().enumerate().take(n_arg).skip(1) {
            if i > 1 {
                s.push(' ');
            }
            s.push_str(a);
        }
        s.push('\n');
        p.outs(&s);
    } else if c == b'p' && n >= 3 && pfx(a0, "progress") {
        return unsupported(p, "progress");
    } else if c == b'p' && pfx(a0, "prompt") {
        let mut cnt = 0usize;
        let mut no_opt = false;
        for i in 1..n_arg {
            let zf = az[i].as_str();
            if zf.starts_with('-') && !no_opt {
                let mut z = if zf.starts_with("--") { &zf[1..] } else { zf };
                if z == "-hard-reset" {
                    z = &z[5..];
                }
                if z == "-reset" {
                    p.prompts = [None, None];
                } else if z == "-show" {
                    let a = p.prompts[0]
                        .clone()
                        .unwrap_or_else(|| "sqlite> ".to_owned());
                    let b = p.prompts[1]
                        .clone()
                        .unwrap_or_else(|| "   ...> ".to_owned());
                    p.stdouts(&format!("Main prompt:  '{a}'\nContinuation: '{b}'\n"));
                } else if z == "-color" || z == "-no-color" {
                } else if z == "-" {
                    no_opt = true;
                } else {
                    p.dot_cmd_error(i, Some("unknown option"), None);
                    return 1;
                }
            } else if cnt > 1 {
                p.dot_cmd_error(i, Some("extra argument"), None);
                return 1;
            } else if !p.dot.ab_quot[i] && !zf.bytes().any(|ch| !ch.is_ascii_lowercase()) {
                p.dot_cmd_error(i, Some("use quotes around the prompt string"), None);
                rc = 1;
            } else {
                p.prompts[cnt] = Some(zf.to_owned());
                cnt += 1;
            }
        }
    } else if c == b'q' && pfx(a0, "quit") {
        rc = 2;
    } else if c == b'r' && n >= 3 && pfx(a0, "read") {
        if p.fail_if_safe_mode("cannot run .read in safe mode") {
            return 1;
        }
        if n_arg != 2 {
            eprintln!("Usage: .read FILE");
            return 1;
        }
        let saved_in = p.input.take();
        let saved_lineno = p.lineno;
        if let Some(cmd) = az[1].strip_prefix('|') {
            match Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .stdout(Stdio::piped())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = p.stdout.flush();
                    if let Some(out) = child.stdout.take() {
                        p.input = Some(Box::new(BufReader::new(out)));
                        rc = i32::from(process_input(p, "<pipe>"));
                    }
                    p.input = None;
                    let _ = child.wait();
                }
                Err(_) => {
                    eprintln!("Error: cannot open \"{}\"", az[1]);
                    rc = 1;
                }
            }
        } else {
            match crate::open_chr_source(&az[1]) {
                None => {
                    eprintln!("Error: cannot open \"{}\"", az[1]);
                    rc = 1;
                }
                Some(r) => {
                    p.input = Some(r);
                    rc = i32::from(process_input(p, &az[1]));
                }
            }
        }
        p.input = saved_in;
        p.lineno = saved_lineno;
    } else if c == b'r' && n >= 3 && pfx(a0, "restore") {
        if p.fail_if_safe_mode("cannot run .restore in safe mode") {
            return 1;
        }
        let (z_src, z_db) = if n_arg == 2 {
            (az[1].as_str(), "main")
        } else if n_arg == 3 {
            (az[2].as_str(), az[1].as_str())
        } else {
            eprintln!("Usage: .restore ?DB? FILE");
            return 1;
        };
        let src = match Connection::open_with_flags(
            z_src,
            OpenFlags(OpenFlags::READWRITE.0 | OpenFlags::CREATE.0 | OpenFlags::URI.0),
        ) {
            Ok(s) => s,
            Err(_) => {
                eprintln!("Error: cannot open \"{z_src}\"");
                return 1;
            }
        };
        if !p.open_db(false) {
            return 1;
        }
        match run_backup(&src, "main", p.db(), z_db, true) {
            Ok(()) => rc = 0,
            Err((code, msg)) => {
                if code == 5 || code == 6 {
                    eprintln!("Error: source database is busy");
                } else {
                    eprintln!("Error: {msg}");
                }
                rc = 1;
            }
        }
        let _ = src.try_close();
    } else if c == b's' && (pfx(a0, "scanstats") || pfx(a0, "scanstatus")) {
        if n_arg == 2 {
            p.mode.scanstats_on = match az[1].as_str() {
                "vm" => 3,
                "est" => 2,
                other => boolean_value(other) as u8,
            };
            if !p.open_db(false) {
                return 1;
            }
            eprintln!("Warning: .scanstats not available in this build.");
        } else {
            eprintln!("Usage: .scanstats on|off|est");
            rc = 1;
        }
    } else if c == b's' && pfx(a0, "schema") {
        rc = dot_cmd_schema(p);
    } else if (c == b's' && n == 11 && pfx(a0, "selecttrace"))
        || (c == b't' && n == 9 && pfx(a0, "treetrace"))
    {
    } else if c == b's' && n >= 4 && pfx(a0, "selftest") {
        return unsupported(p, "selftest");
    } else if c == b's' && pfx(a0, "separator") {
        if !(2..=3).contains(&n_arg) {
            eprintln!("Usage: .separator COL ?ROW?");
            rc = 1;
        }
        if n_arg >= 2 {
            p.mode.spec.z_column_sep = Some(az[1].clone());
        }
        if n_arg >= 3 {
            p.mode.spec.z_row_sep = Some(az[2].clone());
        }
    } else if c == b's' && n >= 4 && pfx(a0, "sha3sum") {
        return unsupported(p, "sha3sum");
    } else if c == b's' && (pfx(a0, "shell") || pfx(a0, "system")) {
        if p.fail_if_safe_mode(&format!("cannot run .{a0} in safe mode")) {
            return 1;
        }
        if n_arg < 2 {
            eprintln!("Usage: .system COMMAND");
            return 1;
        }
        let mut cmd = String::new();
        for (i, a) in az.iter().enumerate().take(n_arg).skip(1) {
            if i > 1 {
                cmd.push(' ');
            }
            if a.contains(' ') {
                cmd.push_str(&format!("\"{a}\""));
            } else {
                cmd.push_str(a);
            }
        }
        // (system() shares the stdio of the shell: nothing buffered here may come later.)
        let x = match Command::new("/bin/sh").arg("-c").arg(&cmd).status() {
            Ok(st) => {
                use std::os::unix::process::ExitStatusExt;
                st.into_raw()
            }
            Err(_) => -1,
        };
        if x != 0 {
            eprintln!("System command returns {x}");
        }
    } else if c == b's' && pfx(a0, "show") {
        if n_arg != 1 {
            eprintln!("Usage: .show");
            return 1;
        }
        const BOOL: [&str; 4] = ["off", "on", "trigger", "full"];
        let m = &p.mode;
        let mut s = String::new();
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "echo",
            BOOL[usize::from(m.m_flags & MFLG_ECHO != 0)]
        ));
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "eqp",
            BOOL[usize::from(m.auto_eqp & 3)]
        ));
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "explain",
            if m.auto_explain != 0 { "auto" } else { "off" }
        ));
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "headers",
            if m.spec.b_titles == QRF_ALWAYS {
                "always"
            } else {
                BOOL[usize::from(m.spec.b_titles == QRF_YES)]
            }
        ));
        let name = A_MODE_INFO[usize::from(m.e_mode)].name;
        if matches!(
            m.spec.e_style,
            QRF_STYLE_COLUMN | QRF_STYLE_BOX | QRF_STYLE_TABLE | QRF_STYLE_MARKDOWN
        ) {
            s.push_str(&format!(
                "{:>12.12}: {name} --wrap {} --wordwrap {} --{}quote\n",
                "mode",
                m.spec.n_wrap,
                if m.spec.b_word_wrap == QRF_YES {
                    "on"
                } else {
                    "off"
                },
                if m.spec.e_text == QRF_TEXT_SQL {
                    ""
                } else {
                    "no"
                }
            ));
        } else {
            s.push_str(&format!("{:>12.12}: {name}\n", "mode"));
        }
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "nullvalue",
            c_string(m.spec.z_null.as_deref().unwrap_or(""))
        ));
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "output",
            if p.outfile.is_empty() {
                "stdout"
            } else {
                &p.outfile
            }
        ));
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "colseparator",
            c_string(m.spec.z_column_sep.as_deref().unwrap_or(""))
        ));
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "rowseparator",
            c_string(m.spec.z_row_sep.as_deref().unwrap_or(""))
        ));
        s.push_str(&format!("{:>12.12}:", "stats"));
        if p.m_stats == 0 {
            s.push_str(" off\n");
        } else {
            let mut ms = p.m_stats;
            if ms & CLISTAT_ALL == CLISTAT_ALL {
                s.push_str(" all");
                ms &= !CLISTAT_ALL;
            }
            if ms & CLISTAT_ON == CLISTAT_ON {
                s.push_str(" on");
                ms &= !CLISTAT_ON;
            }
            if ms & CLISTAT_XSTMT != 0 {
                s.push_str(" xstmt");
            } else if ms & CLISTAT_STMT != 0 {
                s.push_str(" stmt");
            } else if ms & CLISTAT_VMSTEP != 0 {
                s.push_str(" vmstep");
            }
            if ms & CLISTAT_XMEM != 0 {
                s.push_str(" xmem");
            } else if ms & CLISTAT_MEM != 0 {
                s.push_str(" mem");
            }
            if ms & CLISTAT_IO != 0 {
                s.push_str(" io");
            }
            if ms & CLISTAT_ZIPVFS != 0 {
                s.push_str(" zipvfs");
            }
            if ms & CLISTAT_RESET != 0 {
                s.push_str(" reset");
            }
            s.push('\n');
        }
        s.push_str(&format!("{:>12.12}: ", "width"));
        for w in &m.spec.a_width {
            s.push_str(&format!("{w} "));
        }
        s.push('\n');
        s.push_str(&format!(
            "{:>12.12}: {}\n",
            "filename",
            p.db_filename().unwrap_or("")
        ));
        p.outs(&s);
    } else if c == b's' && pfx(a0, "stats") {
        let saved = p.m_stats;
        if n_arg == 1 {
            p.m_stats = CLISTAT_ALL;
            display_stats(p);
            p.m_stats = saved;
            return 0;
        }
        let mut m_new = 0u8;
        let mut b_once = false;
        for ii in 1..n_arg {
            let mut z = az[ii].as_str();
            if z.starts_with('-') {
                z = &z[1..];
                if z.starts_with('-') && z.len() > 1 {
                    z = &z[1..];
                }
            }
            match z {
                "stmt" => m_new |= CLISTAT_STMT | CLISTAT_VMSTEP,
                "xstmt" => m_new |= CLISTAT_STMT | CLISTAT_XSTMT | CLISTAT_VMSTEP,
                "vmstep" => m_new |= CLISTAT_VMSTEP,
                "mem" => m_new |= CLISTAT_MEM,
                "xmem" => m_new |= CLISTAT_XMEM | CLISTAT_MEM,
                "io" => m_new |= CLISTAT_IO,
                "all" => m_new |= CLISTAT_ALL,
                "reset" => m_new |= CLISTAT_RESET,
                "once" => b_once = true,
                "on" | "yes" | "ON" | "YES" => m_new |= CLISTAT_ON,
                "off" | "no" | "OFF" | "NO" => m_new = 0,
                _ if z.as_bytes().first().is_some_and(u8::is_ascii_digit) => {
                    if boolean_value(z) != 0 {
                        m_new |= CLISTAT_ON;
                    } else {
                        m_new = 0;
                    }
                }
                _ => {
                    p.dot_cmd_error(ii, Some("unknown argument"), None);
                    return 1;
                }
            }
        }
        p.m_stats = m_new;
        if b_once {
            display_stats(p);
            p.m_stats = saved;
        }
    } else if c == b't' && n > 1 && pfx(a0, "tables") {
        let z_pattern = if n_arg > 1 {
            Some(az[1].as_str())
        } else {
            None
        };
        if !p.open_db(false) {
            return 1;
        }
        let dbs = match database_list(p.db()) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: {}", e.message);
                return 1;
            }
        };
        let mut sql = String::new();
        for (name, _) in &dbs {
            if !sql.is_empty() {
                sql.push_str(" UNION ALL ");
            }
            if name.eq_ignore_ascii_case("main") {
                sql.push_str("SELECT name FROM ");
            } else {
                sql.push_str(&format!("SELECT {}||'.'||name FROM ", sql_q(name)));
            }
            sql.push_str(&format!("{}.sqlite_schema", sql_w(name)));
            sql.push_str(
                " WHERE type IN ('table','view')   AND name NOT LIKE 'sqlite__%' ESCAPE '_'",
            );
            if let Some(pat) = z_pattern {
                sql.push_str(&format!(" AND name LIKE {}", sql_q(pat)));
            }
        }
        sql.push_str(" ORDER BY 1");
        p.mode_push();
        p.mode_change(MODE_SPLIT);
        exec_quiet(p, &sql);
        p.mode_pop();
    } else if c == b't' && a0 == "testcase" {
        return unsupported(p, "testcase");
    } else if c == b't' && n >= 8 && pfx(a0, "testctrl") {
        return unsupported(p, "testctrl");
    } else if c == b't' && n > 4 && pfx(a0, "timeout") {
        if !p.open_db(false) {
            return 1;
        }
        p.busy_timeout = if n_arg >= 2 { integer_value(&az[1]) } else { 0 };
        let _ = p.db().busy_timeout(p.busy_timeout.max(0) as u64);
    } else if c == b't' && n >= 5 && pfx(a0, "timer") {
        if n_arg == 2 {
            p.enable_timer = if az[1] == "once" {
                1
            } else {
                2 * boolean_value(&az[1]) as u8
            };
        } else {
            eprintln!("Usage: .timer on|off|once");
            rc = 1;
        }
    } else if c == b't' && pfx(a0, "trace") {
        if !p.open_db(false) {
            return 1;
        }
        let mut want_file = false;
        for z in az.iter().take(n_arg).skip(1) {
            if z.starts_with('-') {
                if !["expanded", "plain", "profile", "row", "stmt", "close"]
                    .iter()
                    .any(|o| option_match(z, o))
                {
                    eprintln!("Unknown option \"{z}\" on \".trace\"");
                    return 1;
                }
            } else if z != "off" {
                want_file = true;
            }
        }
        // Without an output file tracing is switched off (the only state this shell has).
        if want_file {
            return unsupported(p, "trace");
        }
    } else if c == b'v' && pfx(a0, "version") {
        p.outs(&format!("SQLite {SQLITE_VERSION} {}\n", crate::source_id()));
        // (The C shell names its compiler here, e.g. "gcc-15.2.0 (64-bit)".)
        p.outs("rustc (64-bit)\n");
    } else if c == b'v' && (pfx(a0, "vfsinfo") || pfx(a0, "vfslist") || pfx(a0, "vfsname")) {
        return unsupported(p, "vfsinfo/.vfslist/.vfsname");
    } else if c == b'w' && pfx(a0, "wheretrace") {
    } else if c == b'w' && pfx(a0, "width") {
        let mut a = Vec::new();
        for j in 1..n_arg {
            let w = integer_value(&az[j]).clamp(-QRF_MAX_WIDTH, QRF_MAX_WIDTH);
            a.push(w as i16);
        }
        p.mode.spec.a_width = a;
    } else {
        eprintln!(
            "Error: unknown command or invalid arguments:  \"{a0}\". Enter \".help\" for help"
        );
        rc = 1;
    }
    rc
}

/// `sqlite3_db_config()` for the `.dbconfig` option at index `ii`: sets it when `set` is
/// given and returns the current value. Options the engine has no switch for are kept as
/// shell-side copies (they do not change the engine's behaviour).
fn dbconfig_access(p: &mut ShellState, ii: usize, set: Option<i32>) -> i32 {
    let b = set.map(|v| v != 0);
    match A_DBCONFIG[ii] {
        "defensive" => i32::from(p.db().db_config_defensive(b)),
        "dqs_dml" => i32::from(p.db().db_config_dqs_dml(b)),
        "enable_trigger" => i32::from(p.db().db_config_enable_trigger(b)),
        "enable_view" => i32::from(p.db().db_config_enable_view(b)),
        "trusted_schema" => {
            if let Some(v) = b {
                let _ = p.db().set_trusted_schema(v);
            }
            i32::from(p.db().trusted_schema().unwrap_or(false))
        }
        "enable_fkey" => {
            if let Some(v) = b {
                let _ = p.db().execute_batch(if v {
                    "PRAGMA foreign_keys=ON"
                } else {
                    "PRAGMA foreign_keys=OFF"
                });
            }
            db_int(p.db(), "PRAGMA foreign_keys") as i32
        }
        "writable_schema" => i32::from(p.db().db_config_writable_schema(b)),
        "reverse_scanorder" => {
            if let Some(v) = b {
                let _ = p.db().execute_batch(if v {
                    "PRAGMA reverse_unordered_selects=ON"
                } else {
                    "PRAGMA reverse_unordered_selects=OFF"
                });
            }
            db_int(p.db(), "PRAGMA reverse_unordered_selects") as i32
        }
        "legacy_alter_table" => {
            if let Some(v) = b {
                let _ = p.db().execute_batch(if v {
                    "PRAGMA legacy_alter_table=ON"
                } else {
                    "PRAGMA legacy_alter_table=OFF"
                });
            }
            db_int(p.db(), "PRAGMA legacy_alter_table") as i32
        }
        _ => {
            if let Some(v) = set {
                p.dbcfg_shadow[ii] = if A_DBCONFIG[ii] == "fp_digits" {
                    v
                } else {
                    i32::from(v != 0)
                };
            }
            p.dbcfg_shadow[ii]
        }
    }
}

/// `sqlite3_limit()` for the `.limit` entry at index `i`.
fn limit_access(p: &mut ShellState, i: usize, set: Option<i32>) -> i32 {
    // sqlite3_limit(): a negative value only queries; a value above the hard limit is capped.
    let new_val = set.filter(|v| *v >= 0).map(|v| {
        if v > LIMIT_MAX[i] {
            LIMIT_MAX[i]
        } else if v < 30 && i == 0 {
            30 // SQLITE_MIN_LENGTH
        } else {
            v
        }
    });
    let old = p.limits[i];
    if let Some(v) = new_val {
        p.limits[i] = v;
        let db = p.db();
        match A_LIMIT[i] {
            "attached" => {
                db.set_attached_limit(v as isize);
            }
            "column" => {
                db.set_column_limit(v as isize);
            }
            "expr_depth" => {
                db.set_expr_depth_limit(v as isize);
            }
            "trigger_depth" => {
                db.set_trigger_depth_limit(v as isize);
            }
            "trigger_steps" => {
                db.set_trigger_steps_limit(v as isize);
            }
            _ => {}
        }
    }
    if set.is_some() { old } else { p.limits[i] }
}
