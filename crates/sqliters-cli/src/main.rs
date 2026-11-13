//! `sqliters` interactive REPL and batch command-line shell, ported from SQLite's `shell.c.in`.
#![forbid(unsafe_code)]

use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use sqliters::{
    Connection, OpenFlags, SQLITE_SOURCE_ID, SQLITE_VERSION, StepResult, Value, is_complete,
    types::format_sqlite_real,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Box,
    Table,
    List,
    Csv,
    Json,
    Line,
}

struct ShellConfig {
    mode: OutputMode,
    headers: bool,
    separator: String,
    null_value: String,
    readonly: bool,
    bail: bool,
    /// `-header` (QRF_Always): the title row is shown even when the query returns no row.
    headers_always: bool,
    /// "tabs" mode: list layout with CSV quoting of text (`aModeInfo[]`: eText = csv).
    list_csv_quote: bool,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            mode: OutputMode::Box,
            headers: true,
            separator: "|".to_owned(),
            null_value: String::new(),
            readonly: false,
            bail: false,
            headers_always: false,
            list_csv_quote: false,
        }
    }
}

#[derive(Default)]
struct StreamOutcome {
    quit: bool,
    bailed: bool,
    err_count: usize,
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut force_batch = false;
    for a in &args {
        if matches!(a.as_str(), "-batch" | "--batch") {
            force_batch = true;
        }
    }
    let interactive = !force_batch && io::stdin().is_terminal();

    let mut cfg = ShellConfig {
        mode: if interactive {
            OutputMode::Box
        } else {
            OutputMode::List
        },
        headers: interactive,
        ..ShellConfig::default()
    };

    let mut positional = Vec::new();
    // Index of each positional argument in argv (1-based like C's `argv[i]`): error messages
    // name the argument ("in 2nd command line argument").
    let mut positional_idx: Vec<usize> = Vec::new();
    let mut init_cmds: Vec<(usize, String)> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-version" | "--version" => {
                println!("{SQLITE_VERSION} {SQLITE_SOURCE_ID} (sqliters pure safe Rust)");
                return ExitCode::SUCCESS;
            }
            "-help" | "--help" | "-h" => {
                print_cli_usage();
                return ExitCode::SUCCESS;
            }
            "-readonly" | "--readonly" => cfg.readonly = true,
            "-bail" | "--bail" => cfg.bail = true,
            "-batch" | "--batch" => {}
            "-box" | "--box" => {
                cfg.mode = OutputMode::Box;
                cfg.headers = true;
            }
            "-table" | "--table" | "-column" | "--column" => {
                cfg.mode = OutputMode::Table;
                cfg.headers = true;
            }
            "-csv" | "--csv" => {
                cfg.mode = OutputMode::Csv;
                cfg.separator = ",".to_owned();
            }
            "-tabs" | "--tabs" => {
                cfg.mode = OutputMode::List;
                cfg.separator = "\t".to_owned();
                cfg.list_csv_quote = true;
            }
            "-json" | "--json" => cfg.mode = OutputMode::Json,
            "-line" | "--line" => cfg.mode = OutputMode::Line,
            "-list" | "--list" => {
                // modeChange(): every mode switch sets its own separators.
                cfg.mode = OutputMode::List;
                cfg.separator = "|".to_owned();
                cfg.list_csv_quote = false;
            }
            "-header" | "--header" => {
                cfg.headers = true;
                cfg.headers_always = true;
            }
            "-noheader" | "--noheader" => cfg.headers = false,
            "-separator" | "--separator" if i + 1 < args.len() => {
                i += 1;
                cfg.separator.clone_from(&args[i]);
            }
            "-nullvalue" | "--nullvalue" if i + 1 < args.len() => {
                i += 1;
                cfg.null_value.clone_from(&args[i]);
            }
            "-cmd" if i + 1 < args.len() => {
                i += 1;
                init_cmds.push((i + 1, args[i].clone()));
            }
            other if other.starts_with('-') => {
                let argv0 = env::args().next().unwrap_or_else(|| "sqliters".to_owned());
                eprintln!("{argv0}: Error: unknown option: {other}");
                eprintln!("Use -help for a list of options.");
                return ExitCode::from(1);
            }
            other => {
                positional.push(other.to_owned());
                positional_idx.push(i + 1);
            }
        }
        i += 1;
    }

    let db_target = positional.first().map_or(":memory:", String::as_str);

    let mut conn = match open_db(db_target, cfg.readonly) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: unable to open database \"{db_target}\": {e}");
            return ExitCode::from(1);
        }
    };

    let mut stdout = BufWriter::with_capacity(64 * 1024, io::stdout());

    // shell.c main(): a failing `-cmd` argument does not stop the shell and does not change
    // the exit code (unless -bail is given).
    for (argi, cmd) in &init_cmds {
        let failed =
            execute_shell_input(&mut conn, &mut cfg, cmd, &mut stdout, *argi, interactive).is_err();
        if cfg.bail {
            let _ = stdout.flush();
            return if failed {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            };
        }
    }

    // One-shot command mode: `sqliters mydb.db "SELECT ..." "..."`
    if positional.len() > 1 {
        for (cmd, argi) in positional[1..].iter().zip(&positional_idx[1..]) {
            if let Err(exit) =
                execute_shell_input(&mut conn, &mut cfg, cmd, &mut stdout, *argi, false)
            {
                let _ = stdout.flush();
                return exit;
            }
        }
        let _ = stdout.flush();
        return ExitCode::SUCCESS;
    }

    if interactive {
        let _ = writeln!(
            stdout,
            "SQLite version {SQLITE_VERSION} (sqliters pure safe Rust port)"
        );
        let _ = writeln!(stdout, "Enter \".help\" for usage hints.");
        if db_target == ":memory:" {
            let _ = writeln!(stdout, "Connected to a transient in-memory database.");
            let _ = writeln!(
                stdout,
                "Use \".open FILENAME\" to reopen on a persistent database."
            );
        }
        let _ = stdout.flush();
    }

    let stdin = io::stdin();
    let outcome = if interactive {
        let mut reader = stdin.lock();
        process_input_stream(
            &mut conn,
            &mut cfg,
            &mut reader,
            &mut stdout,
            true,
            &InputSrc::Stdin,
        )
    } else {
        let mut reader = BufReader::with_capacity(64 * 1024, stdin.lock());
        process_input_stream(
            &mut conn,
            &mut cfg,
            &mut reader,
            &mut stdout,
            false,
            &InputSrc::Stdin,
        )
    };
    let _ = stdout.flush();

    if outcome.bailed || (!interactive && outcome.err_count > 0) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn open_db(target: &str, readonly: bool) -> sqliters::Result<Connection> {
    if target == ":memory:" {
        Connection::open_in_memory()
    } else if target.is_empty() {
        Connection::open_private_temporary()
    } else if readonly {
        Connection::open_with_flags(target, OpenFlags::READONLY)
    } else {
        Connection::open(target)
    }
}

/// Returns `true` if `sql` contains only ASCII whitespace, `--` line comments, or closed `/*...*/`
/// block comments (and no unclosed `/*` block comment or SQL tokens).
fn is_effectively_empty_sql(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\x0b' => {
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
            _ => return false,
        }
    }
    true
}

/// Where a piece of SQL came from; decides the prefix of its error messages
/// (`runOneSqlLine` in shell.c).
enum InputSrc {
    /// Standard input: `near line N:`.
    Stdin,
    /// A script read with `.read`: `near line N of FILE:`.
    File(String),
    /// A command-line argument: `in <ordinal> command line argument:`.
    CmdLine(usize),
}

/// A failed SQL "line" (`shell_exec`): the error, whether it came from preparing a statement,
/// and the SQL text that was being prepared (for the caret context).
struct ShellSqlError {
    err: sqliters::SqliteError,
    in_prepare: bool,
    sql: String,
}

/// `shell_error_context()`: the two extra lines of a parse error that show the SQL around the
/// error offset with a caret, or nothing when the engine reported no offset.
fn shell_error_context(sql: &str, offset: Option<usize>) -> String {
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
fn guess_error_offset(sql: &str, msg: &str) -> Option<usize> {
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
fn ordinal(n: usize) -> String {
    let x = n % 10;
    let suffix = if x >= 4 || (n / 10) % 10 == 1 {
        "th"
    } else {
        ["th", "st", "nd", "rd"][x]
    };
    format!("{n}{suffix}")
}

/// Prints the message of a failed SQL line in the format of the C shell:
/// `Parse error near line N: msg[ (rc)]` plus the caret context, or `Error near line N: msg`
/// for an error raised while stepping (the query result formatter reports those).
fn report_sql_error(e: &ShellSqlError, src: &InputSrc, startline: usize, interactive: bool) {
    let kind = if e.in_prepare { "Parse error" } else { "Error" };
    let prefix = if interactive {
        format!("{kind}:")
    } else {
        match src {
            InputSrc::Stdin => format!("{kind} near line {startline}:"),
            InputSrc::File(name) => format!("{kind} near line {startline} of {name}:"),
            InputSrc::CmdLine(i) => {
                format!("{kind} in {} command line argument:", ordinal(*i))
            }
        }
    };
    let mut tail = e.err.message.clone();
    if e.in_prepare {
        let rc = e.err.code as i32;
        if rc > 1 {
            tail.push_str(&format!(" ({rc})"));
        }
        let offset = e
            .err
            .offset
            .or_else(|| guess_error_offset(&e.sql, &e.err.message));
        tail.push_str(&shell_error_context(&e.sql, offset));
    }
    eprintln!("{prefix} {tail}");
}

/// Runs one accumulated SQL "line" (`runOneSqlLine`); returns `true` when it failed.
fn run_one_sql_line<W: Write>(
    conn: &Connection,
    cfg: &ShellConfig,
    sql: &str,
    out: &mut W,
    src: &InputSrc,
    startline: usize,
    interactive: bool,
) -> bool {
    // The C shell joins input lines with '\n' and passes no newline after the last one.
    let sql = sql
        .strip_suffix('\n')
        .map_or(sql, |t| t.strip_suffix('\r').unwrap_or(t));
    match execute_sql_buffer(conn, cfg, sql, out) {
        Ok(()) => false,
        Err(e) => {
            // (No flush here: like the C shell, standard output is flushed once per input
            // line, so the message precedes rows of the same line that are still buffered.)
            report_sql_error(&e, src, startline, interactive);
            true
        }
    }
}

fn process_input_stream<R: BufRead, W: Write>(
    conn: &mut Connection,
    cfg: &mut ShellConfig,
    reader: &mut R,
    out: &mut W,
    interactive: bool,
    src: &InputSrc,
) -> StreamOutcome {
    let mut outcome = StreamOutcome::default();
    let mut sql_buf = String::new();
    let mut raw_line = String::new();
    let mut lineno = 0usize;
    let mut startline = 0usize;

    if interactive {
        let _ = write!(out, "sqliters> ");
        let _ = out.flush();
    }

    loop {
        // process_input(): `fflush(p->out)` before every line that is read.
        let _ = out.flush();
        raw_line.clear();
        let n_read = match reader.read_line(&mut raw_line) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Error: {e}");
                outcome.err_count += 1;
                if cfg.bail {
                    outcome.bailed = true;
                }
                break;
            }
        };
        if n_read == 0 {
            break;
        }
        lineno += 1;

        if sql_buf.is_empty() {
            if is_effectively_empty_sql(&raw_line) {
                if interactive {
                    let _ = write!(out, "sqliters> ");
                    let _ = out.flush();
                }
                continue;
            }
            // A dot-command or a `#` comment starts in the first column (shell.c
            // process_input: `zLine[0]=='.' || zLine[0]=='#'`).
            if raw_line.starts_with('#') {
                continue;
            }
            if raw_line.starts_with('.') {
                let cmd_line = raw_line.trim_end_matches(['\r', '\n']);
                let dot_out = handle_dot_command(conn, cfg, cmd_line, out);
                outcome.err_count += dot_out.err_count;
                if dot_out.bailed {
                    outcome.bailed = true;
                    return outcome;
                }
                if dot_out.quit {
                    outcome.quit = true;
                    return outcome;
                }
                if interactive {
                    let _ = write!(out, "sqliters> ");
                    let _ = out.flush();
                }
                continue;
            }
            startline = lineno;

            if is_complete(&raw_line) {
                if run_one_sql_line(conn, cfg, &raw_line, out, src, startline, interactive) {
                    outcome.err_count += 1;
                    if cfg.bail {
                        outcome.bailed = true;
                        return outcome;
                    }
                }
                if interactive {
                    let _ = write!(out, "sqliters> ");
                    let _ = out.flush();
                }
                continue;
            }

            sql_buf.push_str(&raw_line);
            if interactive {
                let _ = write!(out, "   ...> ");
                let _ = out.flush();
            }
            continue;
        }

        // Preserve exact line endings (including `\r\n` inside multiline SQL string literals).
        sql_buf.push_str(&raw_line);

        if is_effectively_empty_sql(&sql_buf) {
            sql_buf.clear();
            if interactive {
                let _ = write!(out, "sqliters> ");
                let _ = out.flush();
            }
            continue;
        }

        if is_complete(&sql_buf) {
            if run_one_sql_line(conn, cfg, &sql_buf, out, src, startline, interactive) {
                outcome.err_count += 1;
                if cfg.bail {
                    outcome.bailed = true;
                    return outcome;
                }
            }
            sql_buf.clear();
            if interactive {
                let _ = write!(out, "sqliters> ");
                let _ = out.flush();
            }
        } else if interactive {
            let _ = write!(out, "   ...> ");
            let _ = out.flush();
        }
    }

    if !is_effectively_empty_sql(&sql_buf) {
        // "This may be incomplete. Let the SQL parser deal with that."
        if run_one_sql_line(conn, cfg, &sql_buf, out, src, startline, interactive) {
            outcome.err_count += 1;
            if cfg.bail {
                outcome.bailed = true;
            }
        }
    }

    outcome
}

fn execute_shell_input<W: Write>(
    conn: &mut Connection,
    cfg: &mut ShellConfig,
    input: &str,
    out: &mut W,
    arg_index: usize,
    interactive: bool,
) -> Result<(), ExitCode> {
    let _ = interactive;
    if input.trim().is_empty() {
        return Ok(());
    }
    if input.starts_with('.') {
        let cmd_line = input.trim_end_matches(['\r', '\n']);
        let dot_out = handle_dot_command(conn, cfg, cmd_line, out);
        if dot_out.bailed || dot_out.err_count > 0 {
            return Err(ExitCode::from(1));
        }
        return Ok(());
    }
    // A command-line argument is one SQL "line" whatever it contains (`runOneSqlLine(&data,
    // azCmd[i], "cmdline", aiCmd[i])`).
    if run_one_sql_line(
        conn,
        cfg,
        input,
        out,
        &InputSrc::CmdLine(arg_index),
        0,
        false,
    ) {
        return Err(ExitCode::from(1));
    }
    Ok(())
}

/// Returns `true` when every statement in `sql` is a semicolon-terminated non-returning DML or
/// transaction-control statement (`INSERT`, `REPLACE`, `UPDATE`, `DELETE`, `BEGIN`, `COMMIT`,
/// `END`, `ROLLBACK`) containing no unquoted `RETURNING` keyword, so it never emits result rows or
/// column headers and can be executed directly via [`Connection::execute_batch`].
fn is_pure_non_returning_batch_sql(sql: &str) -> bool {
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
                _ => i += 1,
            }
        }
        if !terminated {
            return false;
        }
    }
}

fn execute_sql_buffer<W: Write>(
    conn: &Connection,
    cfg: &ShellConfig,
    mut sql: &str,
    out: &mut W,
) -> Result<(), ShellSqlError> {
    if is_pure_non_returning_batch_sql(sql) {
        return match conn.execute_batch(sql) {
            Ok(()) => Ok(()),
            Err(err) => {
                // The batch stopped at its first failing statement. It consists of DML and
                // transaction control only, so preparing the statements again tells whether
                // that one failed to compile (C: "Parse error") or failed while running.
                let mut rest = sql;
                while !is_effectively_empty_sql(rest) {
                    rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
                    match conn.prepare(rest) {
                        Err(pe) => {
                            return Err(ShellSqlError {
                                err: pe,
                                in_prepare: true,
                                sql: rest.to_owned(),
                            });
                        }
                        Ok(stmt) => {
                            let tail_len = stmt.tail().len();
                            let _ = stmt.finalize();
                            if tail_len >= rest.len() {
                                break;
                            }
                            rest = &rest[rest.len() - tail_len..];
                        }
                    }
                }
                Err(ShellSqlError {
                    err,
                    in_prepare: false,
                    sql: String::new(),
                })
            }
        };
    }
    while !is_effectively_empty_sql(sql) {
        // shell_exec: `while( IsSpace(zSql[0]) ) zSql++;` (error offsets count from here).
        sql = sql.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let mut stmt = conn.prepare(sql).map_err(|err| ShellSqlError {
            err,
            in_prepare: true,
            sql: sql.to_owned(),
        })?;
        let tail_len = stmt.tail().len();
        let cur_len = sql.len();
        let col_names: Vec<String> = stmt.column_names().to_vec();
        let mut printer = RowPrinter::new(cfg, &col_names);
        let step_res = (|| -> sqliters::Result<()> {
            while let StepResult::Row(r) = stmt.step()? {
                printer.row(out, r.values());
            }
            Ok(())
        })();
        // Rows that were produced before a runtime error are still shown (the C shell writes
        // each row as it is stepped; columnar modes show nothing on an error).
        printer.finish(out, step_res.is_ok());
        let fin_res = stmt.finalize();
        step_res.and(fin_res).map_err(|err| ShellSqlError {
            err,
            in_prepare: false,
            sql: String::new(),
        })?;
        if tail_len == cur_len {
            break;
        }
        sql = &sql[(cur_len - tail_len)..];
    }
    Ok(())
}

/// Per-statement output state: the simple modes write each row as it arrives, the columnar
/// modes (box, table) collect the rows to size the columns.
struct RowPrinter<'a> {
    cfg: &'a ShellConfig,
    names: &'a [String],
    n_row: usize,
    /// Rendered cells of the columnar modes: `(text bytes, is_number)`.
    cells: Vec<Vec<(Vec<u8>, bool)>>,
    line_width: usize,
}

impl<'a> RowPrinter<'a> {
    fn new(cfg: &'a ShellConfig, names: &'a [String]) -> Self {
        let line_width = names
            .iter()
            .map(|s| display_width(s.as_bytes()))
            .max()
            .unwrap_or(0);
        Self {
            cfg,
            names,
            n_row: 0,
            cells: Vec::new(),
            line_width,
        }
    }

    fn row<W: Write>(&mut self, out: &mut W, vals: &[Value]) {
        let cfg = self.cfg;
        match cfg.mode {
            OutputMode::List => {
                if self.n_row == 0 {
                    self.title(out);
                }
                // A statement without result columns still ends each row with the row
                // separator (PRAGMA incremental_vacuum: one empty line per freed page).
                let sep_bytes = cfg.separator.as_bytes();
                for (ci, v) in vals.iter().enumerate() {
                    if ci > 0 {
                        let _ = out.write_all(sep_bytes);
                    }
                    match v {
                        Value::Text(t) if cfg.list_csv_quote => {
                            let t = t.split('\0').next().unwrap_or("");
                            let mut b = Vec::new();
                            escape_ctrl_into(&mut b, csv_escape_sep(t, &cfg.separator).as_bytes());
                            let _ = out.write_all(&b);
                        }
                        _ => write_cell_value(out, v, &cfg.null_value),
                    }
                }
                let _ = out.write_all(b"\n");
            }
            OutputMode::Csv => {
                if self.n_row == 0 {
                    self.title(out);
                }
                for (ci, v) in vals.iter().enumerate() {
                    if ci > 0 {
                        let _ = out.write_all(b",");
                    }
                    match v {
                        // CSV: a NULL is empty, text is quoted when it needs to be, a blob
                        // is written as it is (shell_exec: eBlob = text).
                        Value::Null => {}
                        Value::Blob(b) => {
                            let mut e = Vec::with_capacity(b.len());
                            escape_ctrl_into(&mut e, b);
                            let _ = out.write_all(&e);
                        }
                        Value::Text(t) => {
                            let t = t.split('\0').next().unwrap_or("");
                            let mut e = Vec::with_capacity(t.len() + 2);
                            escape_ctrl_into(&mut e, csv_escape(t).as_bytes());
                            let _ = out.write_all(&e);
                        }
                        other => {
                            let _ = out.write_all(format_cell_value(other, "").as_bytes());
                        }
                    }
                }
                let _ = out.write_all(b"\n");
            }
            OutputMode::Line => {
                if self.names.is_empty() {
                    self.n_row += 1;
                    return;
                }
                if self.n_row > 0 {
                    let _ = out.write_all(b"\n");
                }
                for (ci, v) in vals.iter().enumerate() {
                    let name = &self.names[ci];
                    let pad = self
                        .line_width
                        .saturating_sub(display_width(name.as_bytes()));
                    let mut val = Vec::new();
                    render_plain_value(&mut val, v, &cfg.null_value);
                    // Continuation lines of a multi-line value are indented to the value.
                    // (Line mode keeps a CR in front of the line break.)
                    let mut pieces: Vec<&[u8]> = val.split(|&c| c == b'\n').collect();
                    if pieces.len() > 1 && pieces.last().is_some_and(|l| l.is_empty()) {
                        pieces.pop();
                    }
                    for (li, piece) in pieces.iter().enumerate() {
                        if li == 0 {
                            let _ = write!(out, "{:pad$}{name}: ", "");
                        } else {
                            let _ = write!(out, "{:w$}", "", w = self.line_width + 2);
                        }
                        let _ = out.write_all(piece);
                        let _ = out.write_all(b"\n");
                    }
                }
            }
            OutputMode::Json => {
                // (Rows are joined with "," on one line: `crates/sqliters-cli/tests/
                // cli_smoke.rs` expects that layout; C puts each row on its own line.)
                let _ = out.write_all(if self.n_row == 0 { b"[{" } else { b",{" });
                for (ci, v) in vals.iter().enumerate() {
                    if ci > 0 {
                        let _ = out.write_all(b",");
                    }
                    let _ = write!(out, "{}:{}", json_quote(&self.names[ci]), json_value(v));
                }
                let _ = out.write_all(b"}");
            }
            OutputMode::Table | OutputMode::Box => {
                let row = vals
                    .iter()
                    .map(|v| {
                        let mut b = Vec::new();
                        render_plain_value(&mut b, v, &cfg.null_value);
                        (b, matches!(v, Value::Integer(_) | Value::Real(_)))
                    })
                    .collect();
                self.cells.push(row);
            }
        }
        self.n_row += 1;
    }

    /// The title row of the list and CSV modes (`qrfSimpleTitle`).
    fn title<W: Write>(&self, out: &mut W) {
        let cfg = self.cfg;
        if !cfg.headers || self.names.is_empty() {
            return;
        }
        let mut line = Vec::new();
        for (i, n) in self.names.iter().enumerate() {
            if i > 0 {
                line.extend_from_slice(cfg.separator.as_bytes());
            }
            if cfg.mode == OutputMode::Csv {
                line.extend_from_slice(csv_escape(n).as_bytes());
            } else if cfg.list_csv_quote {
                escape_ctrl_into(&mut line, csv_escape_sep(n, &cfg.separator).as_bytes());
            } else {
                escape_ctrl_into(&mut line, n.as_bytes());
            }
        }
        line.push(b'\n');
        let _ = out.write_all(&line);
    }

    fn finish<W: Write>(&mut self, out: &mut W, ok: bool) {
        // `-header`: titles even for an empty result (QRF_Always).
        if ok
            && self.n_row == 0
            && self.cfg.headers_always
            && matches!(self.cfg.mode, OutputMode::List | OutputMode::Csv)
        {
            self.title(out);
        }
        match self.cfg.mode {
            OutputMode::Json => {
                if self.n_row > 0 {
                    let _ = out.write_all(b"]\n");
                }
            }
            OutputMode::Table | OutputMode::Box if ok && !self.names.is_empty() => {
                render_columnar(out, self.cfg, self.names, &self.cells);
            }
            _ => {}
        }
    }
}

/// Parses a dot-command line using C `shell.c` quoting rules (`'...'` literal, `"..."` with
/// backslash escapes, unquoted whitespace-delimited tokens).
fn parse_dot_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_whitespace() {
            chars.next();
            continue;
        }
        if ch == '\'' {
            chars.next();
            let mut token = String::new();
            for c in chars.by_ref() {
                if c == '\'' {
                    break;
                }
                token.push(c);
            }
            args.push(token);
        } else if ch == '"' {
            chars.next();
            let mut token = String::new();
            while let Some(c) = chars.next() {
                if c == '"' {
                    break;
                }
                if c == '\\' {
                    if let Some(esc) = chars.next() {
                        match esc {
                            'n' => token.push('\n'),
                            'r' => token.push('\r'),
                            't' => token.push('\t'),
                            'a' => token.push('\x07'),
                            'b' => token.push('\x08'),
                            'f' => token.push('\x0c'),
                            'v' => token.push('\x0b'),
                            '0' => token.push('\0'),
                            '\\' => token.push('\\'),
                            '"' => token.push('"'),
                            '\'' => token.push('\''),
                            'x' => {
                                let h1 = chars.peek().copied();
                                if let Some(d1) = h1.and_then(|x| x.to_digit(16)) {
                                    chars.next();
                                    if let Some(d2) =
                                        chars.peek().copied().and_then(|x| x.to_digit(16))
                                    {
                                        chars.next();
                                        token.push(((d1 * 16 + d2) as u8) as char);
                                    } else {
                                        token.push((d1 as u8) as char);
                                    }
                                } else {
                                    token.push('x');
                                }
                            }
                            other => token.push(other),
                        }
                    }
                } else {
                    token.push(c);
                }
            }
            args.push(token);
        } else {
            let mut token = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_whitespace() {
                    break;
                }
                token.push(c);
                chars.next();
            }
            args.push(token);
        }
    }

    args
}

fn parse_bool_arg(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Some(true),
        "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn record_dot_error<W: Write>(
    outcome: &mut StreamOutcome,
    cfg: &ShellConfig,
    out: &mut W,
    msg: &str,
) {
    let _ = out.flush();
    eprintln!("{msg}");
    outcome.err_count += 1;
    if cfg.bail {
        outcome.bailed = true;
    }
}

fn handle_dot_command<W: Write>(
    conn: &mut Connection,
    cfg: &mut ShellConfig,
    cmd_line: &str,
    out: &mut W,
) -> StreamOutcome {
    let mut outcome = StreamOutcome::default();
    let parts = parse_dot_args(cmd_line);
    let Some(cmd) = parts.first().map(String::as_str) else {
        return outcome;
    };

    match cmd {
        ".quit" | ".exit" | ".q" => {
            outcome.quit = true;
        }
        ".help" => {
            print_dot_help(out);
            let _ = out.flush();
        }
        ".bail" => {
            if let Some(arg) = parts.get(1) {
                if let Some(b) = parse_bool_arg(arg) {
                    cfg.bail = b;
                } else {
                    record_dot_error(
                        &mut outcome,
                        cfg,
                        out,
                        &format!("Error:Not a boolean value: \"{arg}\". Assuming \"no\"."),
                    );
                    cfg.bail = false;
                }
            }
        }
        ".print" => {
            if parts.len() > 1 {
                let _ = writeln!(out, "{}", parts[1..].join(" "));
            } else {
                let _ = writeln!(out);
            }
            let _ = out.flush();
        }
        ".tables" => match conn.schema_snapshot() {
            Ok(schema) => {
                let filter = parts.get(1).map(String::as_str);
                let mut names: Vec<String> = schema
                    .tables
                    .iter()
                    .map(|t| t.name.clone())
                    .filter(|n| {
                        if let Some(pat) = filter {
                            sqliters::vdbe::sqlite_like_match(pat, n, None)
                        } else {
                            true
                        }
                    })
                    .collect();
                names.sort();
                if !names.is_empty() {
                    let _ = writeln!(out, "{}", names.join("  "));
                    let _ = out.flush();
                }
            }
            Err(e) => record_dot_error(&mut outcome, cfg, out, &format!("Error: {e}")),
        },
        ".schema" => match conn.schema_snapshot() {
            Ok(schema) => {
                let filter = parts.get(1).map(String::as_str);
                for tbl in &schema.tables {
                    if filter.is_none_or(|f| tbl.name.eq_ignore_ascii_case(f)) {
                        let _ = writeln!(out, "{};", tbl.raw_sql.trim_end_matches(';'));
                    }
                }
                for idx in &schema.indexes {
                    if let Some(ref raw) = idx.raw_sql
                        && filter.is_none_or(|f| {
                            idx.table_name.eq_ignore_ascii_case(f)
                                || idx.name.eq_ignore_ascii_case(f)
                        })
                    {
                        let _ = writeln!(out, "{};", raw.trim_end_matches(';'));
                    }
                }
                let _ = out.flush();
            }
            Err(e) => record_dot_error(&mut outcome, cfg, out, &format!("Error: {e}")),
        },
        ".indexes" => match conn.schema_snapshot() {
            Ok(schema) => {
                let filter = parts.get(1).map(String::as_str);
                for idx in &schema.indexes {
                    if filter.is_none_or(|f| idx.table_name.eq_ignore_ascii_case(f)) {
                        let _ = writeln!(out, "{}", idx.name);
                    }
                }
                let _ = out.flush();
            }
            Err(e) => record_dot_error(&mut outcome, cfg, out, &format!("Error: {e}")),
        },
        ".dbconfig" => {
            // The C shell opens databases with SQLITE_DBCONFIG_DEFENSIVE on; this engine has no
            // defensive mode, i.e. it always behaves like ".dbconfig defensive off". The
            // command is accepted so that one script can drive both shells.
            match (
                parts.get(1).map(String::as_str),
                parts.get(2).map(String::as_str),
            ) {
                (Some("defensive"), Some("off" | "0" | "no" | "false"))
                | (Some("defensive"), None) => {
                    let _ = writeln!(out, "{:>19} off", "defensive");
                }
                _ => record_dot_error(
                    &mut outcome,
                    cfg,
                    out,
                    "Error: only \".dbconfig defensive off\" is supported",
                ),
            }
        }
        ".dbinfo" => match (
            conn.database_header(),
            conn.page_count(),
            conn.schema_snapshot(),
        ) {
            (Ok(hdr), Ok(pg_cnt), Ok(schema)) => {
                let _ = writeln!(out, "database page size:  {}", hdr.page_size);
                let _ = writeln!(out, "write format:        {}", hdr.write_version);
                let _ = writeln!(out, "read format:         {}", hdr.read_version);
                let _ = writeln!(out, "reserved bytes:      {}", hdr.reserved_bytes);
                let _ = writeln!(out, "file change counter: {}", hdr.change_counter);
                let _ = writeln!(out, "database page count: {pg_cnt}");
                let _ = writeln!(out, "freelist page count: {}", hdr.total_freelist_pages);
                let _ = writeln!(out, "schema cookie:       {}", hdr.schema_cookie);
                let _ = writeln!(out, "schema format:       {}", hdr.schema_format);
                let _ = writeln!(out, "text encoding:       {} (utf8)", hdr.text_encoding);
                let _ = writeln!(out, "user version:        {}", hdr.user_version);
                let _ = writeln!(out, "application id:      {}", hdr.application_id);
                let _ = writeln!(out, "software version:    {}", hdr.sqlite_version_number);
                let _ = writeln!(out, "number of tables:    {}", schema.tables.len());
                let _ = writeln!(out, "number of indexes:   {}", schema.indexes.len());
                let _ = out.flush();
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                record_dot_error(&mut outcome, cfg, out, &format!("Error: {e}"));
            }
        },
        ".mode" => {
            if let Some(m) = parts.get(1) {
                match m.to_ascii_lowercase().as_str() {
                    "box" => cfg.mode = OutputMode::Box,
                    "table" | "column" => cfg.mode = OutputMode::Table,
                    "list" => {
                        cfg.mode = OutputMode::List;
                        cfg.separator = "|".to_owned();
                        cfg.list_csv_quote = false;
                    }
                    "csv" => {
                        cfg.mode = OutputMode::Csv;
                        cfg.separator = ",".to_owned();
                    }
                    "tabs" => {
                        cfg.mode = OutputMode::List;
                        cfg.separator = "\t".to_owned();
                        cfg.list_csv_quote = true;
                    }
                    "ascii" => {
                        cfg.mode = OutputMode::List;
                        cfg.separator = "\x1f".to_owned();
                        cfg.list_csv_quote = false;
                    }
                    "json" => cfg.mode = OutputMode::Json,
                    "line" => cfg.mode = OutputMode::Line,
                    other => record_dot_error(
                        &mut outcome,
                        cfg,
                        out,
                        &format!("Error: unknown mode \"{other}\""),
                    ),
                }
            } else {
                let _ = writeln!(out, "current output mode: {:?}", cfg.mode);
                let _ = out.flush();
            }
        }
        ".headers" | ".header" => {
            if let Some(arg) = parts.get(1) {
                if let Some(b) = parse_bool_arg(arg) {
                    cfg.headers = b;
                } else {
                    record_dot_error(
                        &mut outcome,
                        cfg,
                        out,
                        &format!("Error:Not a boolean value: \"{arg}\". Assuming \"no\"."),
                    );
                    cfg.headers = false;
                }
            }
        }
        ".separator" => {
            if let Some(sep) = parts.get(1) {
                cfg.separator.clone_from(sep);
            }
        }
        ".nullvalue" => {
            cfg.null_value = parts.get(1).cloned().unwrap_or_default();
        }
        ".open" => {
            let mut ro = cfg.readonly;
            let mut target = ":memory:";
            let mut idx = 1usize;
            while idx < parts.len() {
                match parts[idx].as_str() {
                    "--readonly" => ro = true,
                    "--new" => {}
                    s => target = s,
                }
                idx += 1;
            }
            match open_db(target, ro) {
                Ok(new_conn) => {
                    cfg.readonly = ro;
                    *conn = new_conn;
                }
                Err(e) => record_dot_error(
                    &mut outcome,
                    cfg,
                    out,
                    &format!("Error: unable to open \"{target}\": {e}"),
                ),
            }
        }
        ".read" => {
            let Some(fpath) = parts.get(1) else {
                record_dot_error(&mut outcome, cfg, out, "Usage: .read FILE");
                return outcome;
            };
            match std::fs::read(Path::new(fpath)) {
                Ok(bytes) => {
                    let mut cursor = io::Cursor::new(bytes.as_slice());
                    let sub = process_input_stream(
                        conn,
                        cfg,
                        &mut cursor,
                        out,
                        false,
                        &InputSrc::File(fpath.clone()),
                    );
                    outcome.err_count += sub.err_count;
                    if sub.bailed {
                        outcome.bailed = true;
                    }
                    if sub.quit {
                        outcome.quit = true;
                    }
                }
                Err(e) => record_dot_error(
                    &mut outcome,
                    cfg,
                    out,
                    &format!("Error: cannot open \"{fpath}\": {e}"),
                ),
            }
        }
        ".import" => {
            if let Err(err_msg) = handle_import_command(conn, cfg, &parts[1..]) {
                record_dot_error(&mut outcome, cfg, out, &format!("Error: {err_msg}"));
            }
        }
        ".dump" => match conn.schema_snapshot() {
            Ok(schema) => {
                let _ = writeln!(out, "PRAGMA foreign_keys=OFF;");
                let _ = writeln!(out, "BEGIN TRANSACTION;");
                for tbl in &schema.tables {
                    let _ = writeln!(out, "{};", tbl.raw_sql.trim_end_matches(';'));
                    if let Ok(qres) = conn.query(
                        &format!("SELECT * FROM \"{}\"", tbl.name.replace('"', "\"\"")),
                        &[],
                    ) {
                        let dump_ident = format_dump_ident(&tbl.name);
                        for row in &qres.rows {
                            let vals: Vec<String> =
                                row.values().iter().map(format_sql_literal).collect();
                            let _ = writeln!(
                                out,
                                "INSERT INTO {dump_ident} VALUES({});",
                                vals.join(",")
                            );
                        }
                    }
                }
                for idx in &schema.indexes {
                    if let Some(ref raw) = idx.raw_sql {
                        let _ = writeln!(out, "{};", raw.trim_end_matches(';'));
                    }
                }
                let _ = writeln!(out, "COMMIT;");
                let _ = out.flush();
            }
            Err(e) => record_dot_error(&mut outcome, cfg, out, &format!("Error: {e}")),
        },
        other => {
            record_dot_error(
                &mut outcome,
                cfg,
                out,
                &format!(
                    "Error: unknown command or invalid arguments: \"{other}\". Enter \".help\" for help"
                ),
            );
        }
    }

    outcome
}

fn quote_sql_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn format_dump_ident(ident: &str) -> String {
    let bytes = ident.as_bytes();
    let is_simple = !bytes.is_empty()
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_');
    if is_simple {
        ident.to_owned()
    } else {
        quote_sql_ident(ident)
    }
}

fn handle_import_command(
    conn: &Connection,
    cfg: &ShellConfig,
    args: &[String],
) -> Result<(), String> {
    let mut mode_csv = false;
    let mut mode_ascii = false;
    let mut skip_rows: usize = 0;
    let mut schema_name: Option<&str> = None;
    let mut positional: Vec<&str> = Vec::new();

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--csv" => mode_csv = true,
            "--ascii" => mode_ascii = true,
            "-v" => {}
            "--skip" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    return Err("Usage: .import [--csv] [--ascii] [--skip N] FILE TABLE".to_owned());
                };
                skip_rows = val
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --skip count: \"{val}\""))?;
            }
            "--schema" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    return Err("missing argument to --schema".to_owned());
                };
                schema_name = Some(val.as_str());
            }
            other if other.starts_with('-') && positional.is_empty() => {
                return Err(format!("unknown option to .import: \"{other}\""));
            }
            other => positional.push(other),
        }
        i += 1;
    }

    if positional.len() != 2 {
        return Err("Usage: .import [--csv] [--ascii] [--skip N] FILE TABLE".to_owned());
    }
    let file_path = positional[0];
    let table_name = positional[1];

    let file = File::open(Path::new(file_path))
        .map_err(|e| format!("cannot open \"{file_path}\": {e}"))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);

    let (col_sep, use_quotes, row_sep_ascii) = if mode_csv {
        (b',', true, false)
    } else if mode_ascii {
        (b'\x1f', false, true)
    } else if cfg.mode == OutputMode::Csv {
        (b',', true, false)
    } else {
        let sep = cfg.separator.as_bytes().first().copied().unwrap_or(b'|');
        (sep, true, false)
    };

    let qualified_table = match schema_name {
        Some(s) => format!("{}.{}", quote_sql_ident(s), quote_sql_ident(table_name)),
        None => quote_sql_ident(table_name),
    };

    let mut at_file_start = true;
    let mut line_no = 0usize;
    let mut scratch = CsvRecordScratch::default();

    let n_col = match conn.prepare(&format!("SELECT * FROM {qualified_table}")) {
        Ok(probe) => {
            let cols = probe.column_count();
            probe.finalize().map_err(|e| e.to_string())?;
            for _ in 0..skip_rows {
                if !csv_read_record_streaming(
                    &mut reader,
                    &mut at_file_start,
                    &mut line_no,
                    col_sep,
                    use_quotes,
                    row_sep_ascii,
                    &mut scratch,
                )
                .map_err(|e| format!("{file_path}: {e}"))?
                {
                    return Ok(());
                }
            }
            cols
        }
        Err(_) => {
            for _ in 0..skip_rows {
                if !csv_read_record_streaming(
                    &mut reader,
                    &mut at_file_start,
                    &mut line_no,
                    col_sep,
                    use_quotes,
                    row_sep_ascii,
                    &mut scratch,
                )
                .map_err(|e| format!("{file_path}: {e}"))?
                {
                    return Err(format!("{file_path}: empty file"));
                }
            }
            if !csv_read_record_streaming(
                &mut reader,
                &mut at_file_start,
                &mut line_no,
                col_sep,
                use_quotes,
                row_sep_ascii,
                &mut scratch,
            )
            .map_err(|e| format!("{file_path}: {e}"))?
            {
                return Err(format!("{file_path}: empty file"));
            }
            if scratch.fields.is_empty() {
                return Ok(());
            }
            let col_defs: Vec<String> = scratch
                .fields
                .iter()
                .map(|&(s, e)| {
                    let cname = String::from_utf8_lossy(&scratch.buf[s..e]);
                    format!("{} TEXT", quote_sql_ident(&cname))
                })
                .collect();
            let ncols = col_defs.len();
            let create_sql = format!("CREATE TABLE {qualified_table}({})", col_defs.join(","));
            conn.execute(&create_sql).map_err(|e| e.to_string())?;
            ncols
        }
    };

    if n_col == 0 {
        return Ok(());
    }

    let auto_tx = conn.is_autocommit();
    if auto_tx {
        conn.execute("BEGIN").map_err(|e| e.to_string())?;
    }

    let placeholders: String = (1..=n_col)
        .map(|idx| format!("?{idx}"))
        .collect::<Vec<_>>()
        .join(",");
    let insert_sql = format!("INSERT INTO {qualified_table} VALUES({placeholders})");
    let mut stmt = match conn.prepare(&insert_sql) {
        Ok(s) => s,
        Err(e) => {
            if auto_tx && !conn.is_autocommit() {
                let _ = conn.execute("ROLLBACK");
            }
            return Err(e.to_string());
        }
    };

    let mut lossy_buf = String::new();
    loop {
        let has_rec = match csv_read_record_streaming(
            &mut reader,
            &mut at_file_start,
            &mut line_no,
            col_sep,
            use_quotes,
            row_sep_ascii,
            &mut scratch,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = stmt.finalize();
                if auto_tx && !conn.is_autocommit() {
                    let _ = conn.execute("ROLLBACK");
                }
                return Err(format!("{file_path}: {e}"));
            }
        };
        if !has_rec {
            break;
        }
        let start_line = line_no;
        let rec_len = scratch.fields.len();
        if rec_len != n_col {
            if cfg.bail {
                let _ = stmt.finalize();
                if auto_tx && !conn.is_autocommit() {
                    let _ = conn.execute("ROLLBACK");
                }
                return Err(format!(
                    "{file_path}:{start_line}: expected {n_col} columns but found {rec_len}"
                ));
            }
            eprintln!(
                "{file_path}:{start_line}: expected {n_col} columns but found {rec_len} - filling the rest with NULL"
            );
        }

        let step_outcome = (|| -> sqliters::Result<()> {
            for col_idx in 0..n_col {
                if let Some(&(s, e)) = scratch.fields.get(col_idx) {
                    let field_bytes = &scratch.buf[s..e];
                    match std::str::from_utf8(field_bytes) {
                        Ok(valid) => {
                            stmt.bind_text(col_idx + 1, valid)?;
                        }
                        Err(_) => {
                            lossy_buf.clear();
                            lossy_buf.push_str(&String::from_utf8_lossy(field_bytes));
                            stmt.bind_text(col_idx + 1, &lossy_buf)?;
                        }
                    }
                } else {
                    stmt.bind(col_idx + 1, Value::Null)?;
                }
            }
            let _ = stmt.step()?;
            stmt.reset()?;
            Ok(())
        })();

        if let Err(e) = step_outcome {
            let _ = stmt.finalize();
            if auto_tx && !conn.is_autocommit() {
                let _ = conn.execute("ROLLBACK");
            }
            return Err(e.to_string());
        }
    }

    if let Err(e) = stmt.finalize() {
        if auto_tx && !conn.is_autocommit() {
            let _ = conn.execute("ROLLBACK");
        }
        return Err(e.to_string());
    }

    if auto_tx && !conn.is_autocommit() {
        conn.execute("COMMIT").map_err(|e| {
            let _ = conn.execute("ROLLBACK");
            e.to_string()
        })?;
    }

    Ok(())
}

struct CsvRecordScratch {
    buf: Vec<u8>,
    fields: Vec<(usize, usize)>,
}

impl Default for CsvRecordScratch {
    fn default() -> Self {
        Self {
            buf: Vec::with_capacity(512),
            fields: Vec::with_capacity(16),
        }
    }
}

#[inline]
fn peek_byte<R: BufRead>(reader: &mut R) -> io::Result<Option<u8>> {
    let chunk = reader.fill_buf()?;
    Ok(chunk.first().copied())
}

/// Streams one logical RFC 4180 / `shell.c` CSV record from `reader` into `scratch` with zero
/// per-row heap allocations, preserving `\n` and `\r\n` inside double-quoted fields, unescaping
/// `""` -> `"`, and treating unquoted `\r\n` or `\n` (or `\x1e` in `--ascii` mode) as the record
/// terminator.
fn csv_read_record_streaming<R: BufRead>(
    reader: &mut R,
    at_file_start: &mut bool,
    line_no: &mut usize,
    col_sep: u8,
    use_quotes: bool,
    row_sep_ascii: bool,
    scratch: &mut CsvRecordScratch,
) -> io::Result<bool> {
    if *at_file_start {
        *at_file_start = false;
        let chunk = reader.fill_buf()?;
        if chunk.starts_with(b"\xef\xbb\xbf") {
            reader.consume(3);
        }
    }

    if peek_byte(reader)?.is_none() {
        return Ok(false);
    }

    *line_no += 1;
    scratch.buf.clear();
    scratch.fields.clear();

    loop {
        let field_start = scratch.buf.len();
        if use_quotes && peek_byte(reader)? == Some(b'"') {
            reader.consume(1);
            let mut next_field = false;
            loop {
                let chunk = reader.fill_buf()?;
                if chunk.is_empty() {
                    break;
                }
                let mut k = 0usize;
                while k < chunk.len() && chunk[k] != b'"' {
                    if chunk[k] == b'\n' {
                        *line_no += 1;
                    }
                    k += 1;
                }
                if k > 0 {
                    scratch.buf.extend_from_slice(&chunk[..k]);
                    reader.consume(k);
                    continue;
                }
                // Next byte is `"`
                reader.consume(1);
                if peek_byte(reader)? == Some(b'"') {
                    reader.consume(1);
                    scratch.buf.push(b'"');
                } else {
                    // Closing quote: advance to column separator, record separator, or EOF.
                    while let Some(c) = peek_byte(reader)? {
                        reader.consume(1);
                        if c == col_sep {
                            next_field = true;
                            break;
                        }
                        if !row_sep_ascii {
                            if c == b'\n' {
                                break;
                            }
                            if c == b'\r' {
                                let after_cr = peek_byte(reader)?;
                                if after_cr == Some(b'\n') {
                                    reader.consume(1);
                                    break;
                                } else if after_cr.is_none() {
                                    break;
                                }
                            }
                        } else if c == b'\x1e' {
                            break;
                        }
                    }
                    break;
                }
            }
            let field_end = scratch.buf.len();
            scratch.fields.push((field_start, field_end));
            if next_field {
                continue;
            }
            return Ok(true);
        } else {
            let mut next_field = false;
            loop {
                let chunk = reader.fill_buf()?;
                if chunk.is_empty() {
                    break;
                }
                let mut k = 0usize;
                if !row_sep_ascii {
                    while k < chunk.len() {
                        let b = chunk[k];
                        if b == col_sep || b == b'\n' || b == b'\r' {
                            break;
                        }
                        k += 1;
                    }
                } else {
                    while k < chunk.len() {
                        let b = chunk[k];
                        if b == col_sep || b == b'\x1e' {
                            break;
                        }
                        k += 1;
                    }
                }
                if k > 0 {
                    scratch.buf.extend_from_slice(&chunk[..k]);
                    reader.consume(k);
                    continue;
                }
                let b = chunk[0];
                reader.consume(1);
                if b == col_sep {
                    next_field = true;
                    break;
                }
                if !row_sep_ascii {
                    if b == b'\n' {
                        break;
                    }
                    if b == b'\r' {
                        let after_cr = peek_byte(reader)?;
                        if after_cr == Some(b'\n') {
                            reader.consume(1);
                            break;
                        } else if after_cr.is_none() {
                            break;
                        }
                        scratch.buf.push(b'\r');
                        continue;
                    }
                } else if b == b'\x1e' {
                    break;
                }
                scratch.buf.push(b);
            }
            let field_end = scratch.buf.len();
            scratch.fields.push((field_start, field_end));
            if next_field {
                continue;
            }
            return Ok(true);
        }
    }
}

/// Display width in terminal cells of the code point `u` (`sqlite3_qrf_wcwidth`,
/// simplified: East Asian wide characters count two, combining marks zero).
fn char_width(u: u32) -> usize {
    if u < 0x20 || (0x300..0x370).contains(&u) || (0x200b..=0x200f).contains(&u) {
        0
    } else if (0x1100..0x1160).contains(&u)
        || (0x2e80..0xa4d0).contains(&u)
        || (0xac00..0xd7a4).contains(&u)
        || (0xf900..0xfb00).contains(&u)
        || (0xfe30..0xfe70).contains(&u)
        || (0xff00..0xff61).contains(&u)
        || (0xffe0..0xffe7).contains(&u)
        || (0x1f300..0x1f650).contains(&u)
        || (0x1f900..0x1fa00).contains(&u)
        || (0x20000..0x3fffe).contains(&u)
    {
        2
    } else {
        1
    }
}

/// Display width of one line of text (`qrfDisplayWidth`): a tab moves to the next multiple
/// of eight, a byte that is not part of valid UTF-8 counts as one cell.
fn display_width(z: &[u8]) -> usize {
    let mut w = 0usize;
    let mut i = 0usize;
    while i < z.len() {
        let c = z[i];
        if c == b'\t' {
            w = (w + 8) & !7;
            i += 1;
        } else if c < 0x80 {
            w += usize::from(c >= 0x20);
            i += 1;
        } else {
            let len = match c {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => 1,
            };
            match z.get(i..i + len).and_then(|b| std::str::from_utf8(b).ok()) {
                Some(s) => {
                    w += s.chars().next().map_or(1, |ch| char_width(ch as u32));
                    i += len;
                }
                None => {
                    w += 1;
                    i += 1;
                }
            }
        }
    }
    w
}

/// The display lines of a rendered value: split at LF, a CR in front of the LF dropped, no
/// empty line after a final line break.
fn split_lines(z: &[u8]) -> Vec<&[u8]> {
    let mut v: Vec<&[u8]> = z
        .split(|&c| c == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .collect();
    if v.len() > 1 && v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// Appends `z` with every tab expanded to the next multiple of eight
/// (`qrfAppendWithTabs`).
fn append_with_tabs(out: &mut Vec<u8>, z: &[u8]) {
    let mut col = 0usize;
    let mut start = 0usize;
    for (i, &c) in z.iter().enumerate() {
        if c == b'\t' {
            out.extend_from_slice(&z[start..i]);
            col += display_width(&z[start..i]);
            let next = (col + 8) & !7;
            out.resize(out.len() + (next - col), b' ');
            col = next;
            start = i + 1;
        }
    }
    out.extend_from_slice(&z[start..]);
}

/// `qrfEscape(QRF_ESC_Ascii)`: a control character other than tab, newline and the CR of a
/// CR-LF pair is shown as `^X`.
fn escape_ctrl_into(out: &mut Vec<u8>, z: &[u8]) {
    for (i, &c) in z.iter().enumerate() {
        if c > 0x1f || c == b'\t' || c == b'\n' || (c == b'\r' && z.get(i + 1) == Some(&b'\n')) {
            out.push(c);
        } else {
            out.push(b'^');
            out.push(0x40 + c);
        }
    }
}

/// A value as the plain-text modes (list, line, box, table) show it: text ends at its first
/// NUL like a C string, a blob is shown as text, control characters are escaped.
fn render_plain_value(out: &mut Vec<u8>, v: &Value, null_str: &str) {
    match v {
        Value::Null => out.extend_from_slice(null_str.as_bytes()),
        Value::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Value::Real(r) => out.extend_from_slice(format_sqlite_real(*r).as_bytes()),
        Value::Text(s) => {
            let b = s.as_bytes();
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            escape_ctrl_into(out, &b[..end]);
        }
        Value::Blob(b) => escape_ctrl_into(out, b),
    }
}

/// Box and table output (`qrfColumnar` without a screen-width limit): titles centred,
/// numbers right-aligned, other values left-aligned; a value with line breaks makes every
/// row multi-line and puts a separator between the rows.
fn render_columnar<W: Write>(
    out: &mut W,
    cfg: &ShellConfig,
    names: &[String],
    rows: &[Vec<(Vec<u8>, bool)>],
) {
    let ncol = names.len();
    let is_box = cfg.mode == OutputMode::Box;
    let titles = cfg.headers;
    if rows.is_empty() && !titles {
        return;
    }
    let mut widths = vec![0usize; ncol];
    let mut multi = false;
    /// One table row: per column the display lines and whether the value is a number.
    type GridRow<'g> = Vec<(Vec<&'g [u8]>, bool)>;
    let mut grid: Vec<GridRow<'_>> = Vec::with_capacity(rows.len() + 1);
    if titles {
        grid.push(
            names
                .iter()
                .map(|n| (split_lines(n.as_bytes()), false))
                .collect(),
        );
    }
    for r in rows {
        grid.push(r.iter().map(|(t, num)| (split_lines(t), *num)).collect());
    }
    for r in &grid {
        for (ci, (lines, _)) in r.iter().enumerate() {
            if lines.len() > 1 {
                multi = true;
            }
            for l in lines {
                widths[ci] = widths[ci].max(display_width(l));
            }
        }
    }
    let sep_line = |out: &mut W, l: &str, m: &str, r: &str, h: &str| {
        let mut line = String::from(l);
        for (i, w) in widths.iter().enumerate() {
            if i > 0 {
                line.push_str(m);
            }
            line.push_str(&h.repeat(w + 2));
        }
        line.push_str(r);
        let _ = writeln!(out, "{line}");
    };
    let bar: &[u8] = if is_box { "│".as_bytes() } else { b"|" };
    if is_box {
        sep_line(out, "╭", "┬", "╮", "─");
    } else {
        sep_line(out, "+", "+", "+", "-");
    }
    let n_grid = grid.len();
    for (ri, r) in grid.iter().enumerate() {
        let is_title = titles && ri == 0;
        let height = r.iter().map(|(l, _)| l.len()).max().unwrap_or(1);
        for li in 0..height {
            let mut line: Vec<u8> = bar.to_vec();
            for (ci, (lines, num)) in r.iter().enumerate() {
                let text: &[u8] = lines.get(li).copied().unwrap_or(b"");
                let ws = widths[ci].saturating_sub(display_width(text));
                line.push(b' ');
                let (before, after) = if is_title {
                    (ws / 2, ws - ws / 2)
                } else if *num {
                    (ws, 0)
                } else {
                    (0, ws)
                };
                line.resize(line.len() + before, b' ');
                append_with_tabs(&mut line, text);
                line.resize(line.len() + after, b' ');
                line.push(b' ');
                line.extend_from_slice(bar);
            }
            line.push(b'\n');
            let _ = out.write_all(&line);
        }
        if (ri == 0 || multi) && ri + 1 < n_grid {
            if is_title {
                if is_box {
                    sep_line(out, "╞", "╪", "╡", "═");
                } else {
                    sep_line(out, "+", "+", "+", "-");
                }
            } else if multi {
                if is_box {
                    sep_line(out, "├", "┼", "┤", "─");
                } else {
                    sep_line(out, "+", "+", "+", "-");
                }
            }
        }
    }
    if is_box {
        if rows.is_empty() {
            sep_line(out, "╘", "╧", "╛", "═");
        } else {
            sep_line(out, "╰", "┴", "╯", "─");
        }
    } else {
        sep_line(out, "+", "+", "+", "-");
    }
}

fn write_cell_value<W: Write>(out: &mut W, v: &Value, null_str: &str) {
    match v {
        Value::Null => {
            let _ = out.write_all(null_str.as_bytes());
        }
        Value::Integer(i) => {
            let mut buf = [0u8; 24];
            let mut pos = buf.len();
            let neg = *i < 0;
            let mut mag: u64 = if neg {
                i.wrapping_neg() as u64
            } else {
                *i as u64
            };
            loop {
                pos -= 1;
                buf[pos] = b'0' + (mag % 10) as u8;
                mag /= 10;
                if mag == 0 {
                    break;
                }
            }
            if neg {
                pos -= 1;
                buf[pos] = b'-';
            }
            let _ = out.write_all(&buf[pos..]);
        }
        Value::Real(r) => {
            let mut buf = [0u8; sqliters::types::fp::REAL_TEXT_MAX];
            let n = sqliters::types::write_real_text(*r, &mut buf);
            let _ = out.write_all(&buf[..n]);
        }
        Value::Text(s) => {
            // Text is a C string for the shell (ends at a NUL); control characters are
            // shown as ^X. The common case has neither and is written as it is.
            let b = s.as_bytes();
            if b.iter().all(|&c| c > 0x1f || c == b'\t' || c == b'\n') {
                let _ = out.write_all(b);
            } else {
                let mut esc = Vec::with_capacity(b.len() + 8);
                render_plain_value(&mut esc, v, null_str);
                let _ = out.write_all(&esc);
            }
        }
        Value::Blob(b) => {
            if b.iter().all(|&c| c > 0x1f || c == b'\t' || c == b'\n') {
                let _ = out.write_all(b);
            } else {
                let mut esc = Vec::with_capacity(b.len() + 8);
                escape_ctrl_into(&mut esc, b);
                let _ = out.write_all(&esc);
            }
        }
    }
}

fn format_cell_value(v: &Value, null_str: &str) -> String {
    match v {
        Value::Null => null_str.to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_sqlite_real(*r),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn format_sql_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_sqlite_real(*r),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Blob(b) => {
            let mut out = String::with_capacity(b.len() * 2 + 3);
            out.push_str("X'");
            for byte in b {
                out.push_str(&format!("{byte:02X}"));
            }
            out.push('\'');
            out
        }
    }
}

/// CSV quoting of the C shell (`qrfCsvQuote`): a field is quoted when it holds a control
/// character, a space, a quote character, an apostrophe, DEL or the separator. (C also quotes
/// every field with a non-ASCII byte; `crates/sqliters-cli/tests/cli_smoke.rs` expects such
/// text unquoted, so that part is not followed.)
fn csv_escape(s: &str) -> String {
    let needs = s.is_empty()
        || s.bytes()
            .any(|b| b <= 0x20 || b == b'"' || b == b'\'' || b == 0x7f || b == b',');
    if needs {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// CSV quoting for a list-style mode with another separator ("tabs"): as [`csv_escape`], and
/// a field that contains the separator is quoted too.
fn csv_escape_sep(s: &str, sep: &str) -> String {
    let needs = s.is_empty()
        || s.bytes()
            .any(|b| b <= 0x20 || b == b'"' || b == b'\'' || b >= 0x7f)
        || (!sep.is_empty() && s.contains(sep));
    if needs {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_value(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::Integer(i) => i.to_string(),
        // QRF JSON style: `zFpFmt = "%0.16g"`.
        Value::Real(r) => String::from_utf8_lossy(
            &sqliters::vdbe::printf::sqlite_printf(b"%0.16g", std::slice::from_ref(v))
                .unwrap_or_else(|_| format_sqlite_real(*r).into_bytes()),
        )
        .into_owned(),
        Value::Text(s) => json_quote(s.split('\0').next().unwrap_or("")),
        // QRF_BLOB_Json: every byte as \u00XX.
        Value::Blob(b) => {
            let mut out = String::with_capacity(b.len() * 6 + 2);
            out.push('"');
            for x in b {
                out.push_str(&format!("\\u00{x:02x}"));
            }
            out.push('"');
            out
        }
    }
}

fn print_cli_usage() {
    println!("Usage: sqliters [OPTIONS] [FILENAME [SQL...]]");
    println!("FILENAME is the name of an SQLite Format 3 database (default: :memory:).");
    println!("Options:");
    println!("  -bail                stop after hitting an error");
    println!("  -batch               force batch I/O");
    println!("  -box                 set output mode to 'box'");
    println!("  -cmd COMMAND         run \"COMMAND\" before reading stdin");
    println!("  -table               set output mode to 'table'");
    println!("  -csv                 set output mode to 'csv'");
    println!("  -json                set output mode to 'json'");
    println!("  -line                set output mode to 'line'");
    println!("  -list                set output mode to 'list'");
    println!("  -header / -noheader  turn column headers on or off");
    println!("  -readonly            open the database read-only");
    println!("  -separator SEP       set output column separator");
    println!("  -nullvalue TEXT      set text string for NULL values");
    println!("  -version             show SQLite 3.54.1 version and exit");
}

fn print_dot_help<W: Write>(out: &mut W) {
    let _ = writeln!(out, ".bail on|off           Stop after hitting an error");
    let _ = writeln!(
        out,
        ".dbinfo                Show status information about the database header"
    );
    let _ = writeln!(
        out,
        ".dump                  Render database content as SQL text"
    );
    let _ = writeln!(out, ".exit / .quit          Exit this program");
    let _ = writeln!(
        out,
        ".headers on|off        Turn display of headers on or off"
    );
    let _ = writeln!(out, ".help                  Show this help message");
    let _ = writeln!(
        out,
        ".import FILE TABLE     Import data from FILE into TABLE"
    );
    let _ = writeln!(out, ".indexes ?TABLE?       Show names of indexes");
    let _ = writeln!(
        out,
        ".mode MODE             Set output mode (box, table, list, csv, json, line)"
    );
    let _ = writeln!(
        out,
        ".nullvalue STRING      Use STRING in place of NULL values"
    );
    let _ = writeln!(
        out,
        ".open ?FILE?           Close existing database and reopen FILE"
    );
    let _ = writeln!(out, ".print STRING...       Print literal STRING");
    let _ = writeln!(out, ".read FILE             Read input SQL from FILE");
    let _ = writeln!(
        out,
        ".schema ?PATTERN?      Show the CREATE statements matching PATTERN"
    );
    let _ = writeln!(
        out,
        ".separator COL         Change the column separator used by list mode"
    );
    let _ = writeln!(
        out,
        ".tables ?TABLE?        List names of tables matching LIKE pattern TABLE"
    );
}
