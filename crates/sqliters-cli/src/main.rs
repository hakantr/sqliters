//! `sqliters` interactive REPL and batch command-line shell, ported from SQLite's `shell.c.in`.
#![forbid(unsafe_code)]

use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use sqliters::{
    Connection, OpenFlags, QueryResult, SQLITE_SOURCE_ID, SQLITE_VERSION, StepResult, Value,
    is_complete, types::format_sqlite_real,
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
    let mut init_cmds: Vec<String> = Vec::new();
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
            }
            "-json" | "--json" => cfg.mode = OutputMode::Json,
            "-line" | "--line" => cfg.mode = OutputMode::Line,
            "-list" | "--list" => cfg.mode = OutputMode::List,
            "-header" | "--header" => cfg.headers = true,
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
                init_cmds.push(args[i].clone());
            }
            other if other.starts_with('-') => {
                eprintln!("Error: unknown option: {other}");
                return ExitCode::from(1);
            }
            other => positional.push(other.to_owned()),
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

    for cmd in &init_cmds {
        if let Err(exit) = execute_shell_input(&mut conn, &mut cfg, cmd, &mut stdout) {
            let _ = stdout.flush();
            return exit;
        }
    }

    // One-shot command mode: `sqliters mydb.db "SELECT ..." "..."`
    if positional.len() > 1 {
        for cmd in &positional[1..] {
            if let Err(exit) = execute_shell_input(&mut conn, &mut cfg, cmd, &mut stdout) {
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
        process_input_stream(&mut conn, &mut cfg, &mut reader, &mut stdout, true)
    } else {
        let mut reader = BufReader::with_capacity(64 * 1024, stdin.lock());
        process_input_stream(&mut conn, &mut cfg, &mut reader, &mut stdout, false)
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

fn process_input_stream<R: BufRead, W: Write>(
    conn: &mut Connection,
    cfg: &mut ShellConfig,
    reader: &mut R,
    out: &mut W,
    interactive: bool,
) -> StreamOutcome {
    let mut outcome = StreamOutcome::default();
    let mut sql_buf = String::new();
    let mut raw_line = String::new();

    if interactive {
        let _ = write!(out, "sqliters> ");
        let _ = out.flush();
    }

    loop {
        raw_line.clear();
        let n_read = match reader.read_line(&mut raw_line) {
            Ok(n) => n,
            Err(e) => {
                let _ = out.flush();
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

        if sql_buf.is_empty() {
            let trimmed_start = raw_line.trim_start_matches([' ', '\t']);
            if trimmed_start.starts_with('.') {
                let cmd_line = trimmed_start.trim_end_matches(['\r', '\n']);
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

            if is_effectively_empty_sql(&raw_line) {
                if interactive {
                    let _ = write!(out, "sqliters> ");
                    let _ = out.flush();
                }
                continue;
            }

            if is_complete(&raw_line) {
                if let Err(e) = execute_sql_buffer(conn, cfg, &raw_line, out) {
                    let _ = out.flush();
                    eprintln!("Error: {e}");
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
            if let Err(e) = execute_sql_buffer(conn, cfg, &sql_buf, out) {
                let _ = out.flush();
                eprintln!("Error: {e}");
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
        sql_buf.push(';');
        if let Err(e) = execute_sql_buffer(conn, cfg, &sql_buf, out) {
            let _ = out.flush();
            eprintln!("Parse error: {e}");
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
) -> Result<(), ExitCode> {
    let trimmed = input.trim_start_matches([' ', '\t']);
    if trimmed.trim().is_empty() {
        return Ok(());
    }
    if trimmed.starts_with('.') {
        let cmd_line = trimmed.trim_end_matches(['\r', '\n']);
        let dot_out = handle_dot_command(conn, cfg, cmd_line, out);
        if dot_out.bailed || dot_out.err_count > 0 {
            return Err(ExitCode::from(1));
        }
        return Ok(());
    }
    if !input.contains('\n') && !input.contains('\r') && is_complete(input) {
        if let Err(e) = execute_sql_buffer(conn, cfg, input, out) {
            let _ = out.flush();
            eprintln!("Error: {e}");
            return Err(ExitCode::from(1));
        }
        return Ok(());
    }
    let mut cursor = io::Cursor::new(input.as_bytes());
    let outcome = process_input_stream(conn, cfg, &mut cursor, out, false);
    if outcome.bailed || outcome.err_count > 0 {
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
) -> sqliters::Result<()> {
    if is_pure_non_returning_batch_sql(sql) {
        return conn.execute_batch(sql);
    }
    while !is_effectively_empty_sql(sql) {
        let mut stmt = conn.prepare(sql)?;
        let tail_len = stmt.tail().len();
        let cur_len = sql.len();
        if matches!(
            cfg.mode,
            OutputMode::List | OutputMode::Csv | OutputMode::Line
        ) {
            let col_names: Vec<String> = stmt.column_names().to_vec();
            let has_cols = !col_names.is_empty();
            let max_w = if cfg.mode == OutputMode::Line && has_cols {
                col_names
                    .iter()
                    .map(|s| s.chars().count())
                    .max()
                    .unwrap_or(0)
            } else {
                0
            };
            let mut row_idx = 0usize;
            let step_res = (|| -> sqliters::Result<()> {
                while let StepResult::Row(r) = stmt.step()? {
                    if !has_cols {
                        continue;
                    }
                    match cfg.mode {
                        OutputMode::List => {
                            if row_idx == 0 && cfg.headers {
                                let _ = writeln!(out, "{}", col_names.join(&cfg.separator));
                            }
                            let sep_bytes = cfg.separator.as_bytes();
                            for (ci, v) in r.values().iter().enumerate() {
                                if ci > 0 {
                                    let _ = out.write_all(sep_bytes);
                                }
                                write_cell_value(out, v, &cfg.null_value);
                            }
                            let _ = out.write_all(b"\n");
                        }
                        OutputMode::Csv => {
                            if row_idx == 0 && cfg.headers {
                                let hdr: Vec<String> =
                                    col_names.iter().map(|s| csv_escape(s)).collect();
                                let _ = writeln!(out, "{}", hdr.join(","));
                            }
                            for (ci, v) in r.values().iter().enumerate() {
                                if ci > 0 {
                                    let _ = out.write_all(b",");
                                }
                                let esc = csv_escape(&format_cell_value(v, &cfg.null_value));
                                let _ = out.write_all(esc.as_bytes());
                            }
                            let _ = out.write_all(b"\n");
                        }
                        OutputMode::Line => {
                            if row_idx > 0 {
                                let _ = out.write_all(b"\n");
                            }
                            for (ci, v) in r.values().iter().enumerate() {
                                let val = format_cell_value(v, &cfg.null_value);
                                let _ = writeln!(
                                    out,
                                    "{:>width$} = {val}",
                                    col_names[ci],
                                    width = max_w
                                );
                            }
                        }
                        _ => unreachable!(),
                    }
                    row_idx += 1;
                }
                Ok(())
            })();
            let fin_res = stmt.finalize();
            step_res?;
            fin_res?;
            if row_idx > 0 {
                let _ = out.flush();
            }
        } else {
            let step_res = (|| -> sqliters::Result<QueryResult> {
                let mut rows = Vec::new();
                while let StepResult::Row(r) = stmt.step()? {
                    rows.push(r);
                }
                Ok(QueryResult {
                    column_names: stmt.column_names().to_vec(),
                    rows,
                })
            })();
            let fin_res = stmt.finalize();
            let qres = step_res?;
            fin_res?;
            if !qres.rows.is_empty()
                || (cfg.headers && !qres.column_names.is_empty() && cfg.mode == OutputMode::Json)
            {
                render_query_result(&qres, cfg, out);
                let _ = out.flush();
            }
        }
        if tail_len == cur_len {
            break;
        }
        sql = &sql[(cur_len - tail_len)..];
    }
    Ok(())
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
                    "list" => cfg.mode = OutputMode::List,
                    "csv" => {
                        cfg.mode = OutputMode::Csv;
                        cfg.separator = ",".to_owned();
                    }
                    "tabs" => {
                        cfg.mode = OutputMode::List;
                        cfg.separator = "\t".to_owned();
                    }
                    "ascii" => {
                        cfg.mode = OutputMode::List;
                        cfg.separator = "\x1f".to_owned();
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
                    if cfg.bail
                        && let Ok(sql_text) = std::str::from_utf8(&bytes)
                        && is_pure_non_returning_batch_sql(sql_text)
                    {
                        if let Err(e) = conn.execute_batch(sql_text) {
                            record_dot_error(&mut outcome, cfg, out, &format!("Error: {e}"));
                        }
                    } else {
                        let mut cursor = io::Cursor::new(bytes.as_slice());
                        let sub = process_input_stream(conn, cfg, &mut cursor, out, false);
                        outcome.err_count += sub.err_count;
                        if sub.bailed {
                            outcome.bailed = true;
                        }
                        if sub.quit {
                            outcome.quit = true;
                        }
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

fn render_query_result<W: Write>(res: &QueryResult, cfg: &ShellConfig, out: &mut W) {
    let ncols = res.column_names.len();
    if ncols == 0 {
        return;
    }

    match cfg.mode {
        OutputMode::List => {
            if cfg.headers {
                let _ = writeln!(out, "{}", res.column_names.join(&cfg.separator));
            }
            for r in &res.rows {
                for (ci, v) in r.values().iter().enumerate() {
                    if ci > 0 {
                        let _ = write!(out, "{}", cfg.separator);
                    }
                    write_cell_value(out, v, &cfg.null_value);
                }
                let _ = writeln!(out);
            }
        }
        OutputMode::Csv => {
            if cfg.headers {
                let hdr: Vec<String> = res.column_names.iter().map(|s| csv_escape(s)).collect();
                let _ = writeln!(out, "{}", hdr.join(","));
            }
            for r in &res.rows {
                let esc: Vec<String> = r
                    .values()
                    .iter()
                    .map(|v| csv_escape(&format_cell_value(v, &cfg.null_value)))
                    .collect();
                let _ = writeln!(out, "{}", esc.join(","));
            }
        }
        OutputMode::Line => {
            let max_w = res
                .column_names
                .iter()
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0);
            for (ri, r) in res.rows.iter().enumerate() {
                if ri > 0 {
                    let _ = writeln!(out);
                }
                for (ci, v) in r.values().iter().enumerate() {
                    let val = format_cell_value(v, &cfg.null_value);
                    let _ = writeln!(
                        out,
                        "{:>width$} = {val}",
                        res.column_names[ci],
                        width = max_w
                    );
                }
            }
        }
        OutputMode::Json => {
            let _ = write!(out, "[");
            for (ri, row) in res.rows.iter().enumerate() {
                if ri > 0 {
                    let _ = write!(out, ",");
                }
                let _ = write!(out, "{{");
                for (ci, v) in row.values().iter().enumerate() {
                    if ci > 0 {
                        let _ = write!(out, ",");
                    }
                    let _ = write!(
                        out,
                        "{}:{}",
                        json_quote(&res.column_names[ci]),
                        json_value(v)
                    );
                }
                let _ = write!(out, "}}");
            }
            let _ = writeln!(out, "]");
        }
        OutputMode::Table | OutputMode::Box => {
            let formatted_rows: Vec<Vec<String>> = res
                .rows
                .iter()
                .map(|r| {
                    r.values()
                        .iter()
                        .map(|v| format_cell_value(v, &cfg.null_value))
                        .collect()
                })
                .collect();

            let mut widths: Vec<usize> = res
                .column_names
                .iter()
                .map(|h| if cfg.headers { h.chars().count() } else { 0 })
                .collect();
            for r in &formatted_rows {
                for (ci, cell) in r.iter().enumerate() {
                    widths[ci] = widths[ci].max(cell.chars().count());
                }
            }

            if cfg.mode == OutputMode::Box {
                print_box_border(out, "┌", "┬", "┐", "─", &widths);
                if cfg.headers {
                    print_box_row(out, &res.column_names, &widths, "│");
                    print_box_border(out, "├", "┼", "┤", "─", &widths);
                }
                for r in &formatted_rows {
                    print_box_row(out, r, &widths, "│");
                }
                print_box_border(out, "╰", "┴", "╯", "─", &widths);
            } else {
                print_box_border(out, "+", "+", "+", "-", &widths);
                if cfg.headers {
                    print_box_row(out, &res.column_names, &widths, "|");
                    print_box_border(out, "+", "+", "+", "-", &widths);
                }
                for r in &formatted_rows {
                    print_box_row(out, r, &widths, "|");
                }
                print_box_border(out, "+", "+", "+", "-", &widths);
            }
        }
    }
}

fn print_box_border<W: Write>(
    out: &mut W,
    left: &str,
    mid: &str,
    right: &str,
    horiz: &str,
    widths: &[usize],
) {
    let _ = write!(out, "{left}");
    for (i, &w) in widths.iter().enumerate() {
        if i > 0 {
            let _ = write!(out, "{mid}");
        }
        let _ = write!(out, "{}", horiz.repeat(w + 2));
    }
    let _ = writeln!(out, "{right}");
}

fn print_box_row<W: Write>(out: &mut W, cells: &[String], widths: &[usize], sep: &str) {
    let _ = write!(out, "{sep}");
    for (i, cell) in cells.iter().enumerate() {
        let pad = widths[i].saturating_sub(cell.chars().count());
        let _ = write!(out, " {cell}{} {sep}", " ".repeat(pad));
    }
    let _ = writeln!(out);
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
            let s = format_sqlite_real(*r);
            let _ = out.write_all(s.as_bytes());
        }
        Value::Text(s) => {
            let _ = out.write_all(s.as_bytes());
        }
        Value::Blob(b) => {
            let _ = out.write_all(String::from_utf8_lossy(b).as_bytes());
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

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
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
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
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
        Value::Real(r) => format_sqlite_real(*r),
        Value::Text(s) => json_quote(s),
        Value::Blob(b) => json_quote(&format!(
            "x'{}'",
            b.iter().map(|x| format!("{x:02X}")).collect::<String>()
        )),
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
