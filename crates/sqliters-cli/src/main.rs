//! `sqliters` interactive REPL and batch command-line shell, ported from SQLite's `shell.c.in`.
#![forbid(unsafe_code)]

use std::env;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use sqliters::{
    Connection, OpenFlags, QueryResult, SQLITE_SOURCE_ID, SQLITE_VERSION, Value, is_complete,
    parse_one, types::format_sqlite_real,
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
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            mode: OutputMode::Box,
            headers: true,
            separator: "|".to_owned(),
            null_value: String::new(),
            readonly: false,
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let interactive = io::stdin().is_terminal();

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
            "-box" | "--box" => {
                cfg.mode = OutputMode::Box;
                cfg.headers = true;
            }
            "-table" | "--table" => {
                cfg.mode = OutputMode::Table;
                cfg.headers = true;
            }
            "-csv" | "--csv" => {
                cfg.mode = OutputMode::Csv;
                cfg.separator = ",".to_owned();
            }
            "-json" | "--json" => cfg.mode = OutputMode::Json,
            "-line" | "--line" => cfg.mode = OutputMode::Line,
            "-list" | "--list" => cfg.mode = OutputMode::List,
            "-header" | "--header" => cfg.headers = true,
            "-noheader" | "--noheader" => cfg.headers = false,
            "-separator" if i + 1 < args.len() => {
                i += 1;
                cfg.separator.clone_from(&args[i]);
            }
            "-nullvalue" if i + 1 < args.len() => {
                i += 1;
                cfg.null_value.clone_from(&args[i]);
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

    // One-shot command mode: `sqliters mydb.db "SELECT ..." "..."`
    if positional.len() > 1 {
        for cmd in &positional[1..] {
            if let Err(exit) = execute_shell_input(&mut conn, &mut cfg, cmd) {
                return exit;
            }
        }
        return ExitCode::SUCCESS;
    }

    if interactive {
        println!("SQLite version {SQLITE_VERSION} (sqliters pure safe Rust port)");
        println!("Enter \".help\" for usage hints.");
        if db_target == ":memory:" {
            println!("Connected to a transient in-memory database.");
            println!("Use \".open FILENAME\" to reopen on a persistent database.");
        }
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut sql_buf = String::new();

    if interactive {
        let _ = write!(stdout, "sqliters> ");
        let _ = stdout.flush();
    }

    for line_res in stdin.lock().lines() {
        let Ok(line) = line_res else {
            break;
        };

        if sql_buf.trim().is_empty() && line.trim_start().starts_with('.') {
            if handle_dot_command(&mut conn, &mut cfg, line.trim()) {
                return ExitCode::SUCCESS;
            }
            if interactive {
                let _ = write!(stdout, "sqliters> ");
                let _ = stdout.flush();
            }
            continue;
        }

        if !sql_buf.is_empty() {
            sql_buf.push('\n');
        }
        sql_buf.push_str(&line);

        if sql_buf.trim().is_empty() {
            sql_buf.clear();
            if interactive {
                let _ = write!(stdout, "sqliters> ");
                let _ = stdout.flush();
            }
            continue;
        }

        if is_complete(&sql_buf) {
            if let Err(e) = execute_sql_buffer(&conn, &cfg, &sql_buf) {
                eprintln!("Error: {e}");
            }
            sql_buf.clear();
            if interactive {
                let _ = write!(stdout, "sqliters> ");
                let _ = stdout.flush();
            }
        } else if interactive {
            let _ = write!(stdout, "   ...> ");
            let _ = stdout.flush();
        }
    }

    if !sql_buf.trim().is_empty() {
        // Flush trailing statement even if final semicolon was omitted in batch mode
        sql_buf.push(';');
        if let Err(e) = execute_sql_buffer(&conn, &cfg, &sql_buf) {
            eprintln!("Parse error: {e}");
            return ExitCode::from(1);
        }
    }

    ExitCode::SUCCESS
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

fn execute_shell_input(
    conn: &mut Connection,
    cfg: &mut ShellConfig,
    input: &str,
) -> Result<(), ExitCode> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if trimmed.starts_with('.') {
        let _ = handle_dot_command(conn, cfg, trimmed);
        return Ok(());
    }
    let mut sql = trimmed.to_owned();
    if !is_complete(&sql) {
        sql.push(';');
    }
    if let Err(e) = execute_sql_buffer(conn, cfg, &sql) {
        eprintln!("Error: {e}");
        return Err(ExitCode::from(1));
    }
    Ok(())
}

fn execute_sql_buffer(conn: &Connection, cfg: &ShellConfig, mut sql: &str) -> sqliters::Result<()> {
    while !sql.trim().is_empty() {
        let (_, tail) = parse_one(sql)?;
        let consumed_len = sql.len() - tail.len();
        let single_chunk = &sql[..consumed_len];
        if !single_chunk.trim().is_empty() {
            let qres = conn.query(single_chunk, &[])?;
            if !qres.rows.is_empty()
                || (cfg.headers && !qres.column_names.is_empty() && cfg.mode == OutputMode::Json)
            {
                render_query_result(&qres, cfg);
            }
        }
        if tail.len() == sql.len() {
            break;
        }
        sql = tail;
    }
    Ok(())
}

/// Returns `true` if the shell should exit (`.quit` / `.exit`).
fn handle_dot_command(conn: &mut Connection, cfg: &mut ShellConfig, cmd_line: &str) -> bool {
    let parts: Vec<&str> = cmd_line.split_whitespace().collect();
    let Some(&cmd) = parts.first() else {
        return false;
    };

    match cmd {
        ".quit" | ".exit" | ".q" => return true,
        ".help" => print_dot_help(),
        ".tables" => match conn.schema_snapshot() {
            Ok(schema) => {
                let filter = parts.get(1).copied();
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
                    println!("{}", names.join("  "));
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        },
        ".schema" => match conn.schema_snapshot() {
            Ok(schema) => {
                let filter = parts.get(1).copied();
                for tbl in &schema.tables {
                    if filter.is_none_or(|f| tbl.name.eq_ignore_ascii_case(f)) {
                        println!("{};", tbl.raw_sql.trim_end_matches(';'));
                    }
                }
                for idx in &schema.indexes {
                    if let Some(ref raw) = idx.raw_sql
                        && filter.is_none_or(|f| {
                            idx.table_name.eq_ignore_ascii_case(f)
                                || idx.name.eq_ignore_ascii_case(f)
                        })
                    {
                        println!("{};", raw.trim_end_matches(';'));
                    }
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        },
        ".indexes" => match conn.schema_snapshot() {
            Ok(schema) => {
                let filter = parts.get(1).copied();
                for idx in &schema.indexes {
                    if filter.is_none_or(|f| idx.table_name.eq_ignore_ascii_case(f)) {
                        println!("{}", idx.name);
                    }
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        },
        ".dbinfo" => match (
            conn.database_header(),
            conn.page_count(),
            conn.schema_snapshot(),
        ) {
            (Ok(hdr), Ok(pg_cnt), Ok(schema)) => {
                println!("database page size:  {}", hdr.page_size);
                println!("write format:        {}", hdr.write_version);
                println!("read format:         {}", hdr.read_version);
                println!("reserved bytes:      {}", hdr.reserved_bytes);
                println!("file change counter: {}", hdr.change_counter);
                println!("database page count: {pg_cnt}");
                println!("freelist page count: {}", hdr.total_freelist_pages);
                println!("schema cookie:       {}", hdr.schema_cookie);
                println!("schema format:       {}", hdr.schema_format);
                println!("text encoding:       {} (utf8)", hdr.text_encoding);
                println!("user version:        {}", hdr.user_version);
                println!("application id:      {}", hdr.application_id);
                println!("software version:    {}", hdr.sqlite_version_number);
                println!("number of tables:    {}", schema.tables.len());
                println!("number of indexes:   {}", schema.indexes.len());
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
                eprintln!("Error: {e}");
            }
        },
        ".mode" => {
            if let Some(&m) = parts.get(1) {
                match m.to_ascii_lowercase().as_str() {
                    "box" => cfg.mode = OutputMode::Box,
                    "table" | "column" => cfg.mode = OutputMode::Table,
                    "list" => cfg.mode = OutputMode::List,
                    "csv" => cfg.mode = OutputMode::Csv,
                    "json" => cfg.mode = OutputMode::Json,
                    "line" => cfg.mode = OutputMode::Line,
                    other => eprintln!("Error: unknown mode \"{other}\""),
                }
            } else {
                println!("current output mode: {:?}", cfg.mode);
            }
        }
        ".headers" | ".header" => {
            if let Some(&arg) = parts.get(1) {
                cfg.headers = matches!(
                    arg.to_ascii_lowercase().as_str(),
                    "on" | "1" | "true" | "yes"
                );
            }
        }
        ".separator" => {
            if let Some(&sep) = parts.get(1) {
                cfg.separator = sep.to_owned();
            }
        }
        ".nullvalue" => {
            cfg.null_value = parts.get(1).copied().unwrap_or("").to_owned();
        }
        ".open" => {
            let target = parts.get(1).copied().unwrap_or(":memory:");
            match open_db(target, cfg.readonly) {
                Ok(new_conn) => *conn = new_conn,
                Err(e) => eprintln!("Error: unable to open \"{target}\": {e}"),
            }
        }
        ".read" => {
            let Some(&fpath) = parts.get(1) else {
                eprintln!("Usage: .read FILE");
                return false;
            };
            match fs::read_to_string(Path::new(fpath)) {
                Ok(script) => {
                    if let Err(e) = execute_sql_buffer(conn, cfg, &script) {
                        eprintln!("Error in {fpath}: {e}");
                    }
                }
                Err(e) => eprintln!("Error: cannot read \"{fpath}\": {e}"),
            }
        }
        ".dump" => match conn.schema_snapshot() {
            Ok(schema) => {
                println!("PRAGMA foreign_keys=OFF;");
                println!("BEGIN TRANSACTION;");
                for tbl in &schema.tables {
                    println!("{};", tbl.raw_sql.trim_end_matches(';'));
                    if let Ok(qres) = conn.query(&format!("SELECT * FROM \"{}\"", tbl.name), &[]) {
                        for row in &qres.rows {
                            let vals: Vec<String> =
                                row.values().iter().map(format_sql_literal).collect();
                            println!("INSERT INTO {} VALUES({});", tbl.name, vals.join(","));
                        }
                    }
                }
                for idx in &schema.indexes {
                    if let Some(ref raw) = idx.raw_sql {
                        println!("{};", raw.trim_end_matches(';'));
                    }
                }
                println!("COMMIT;");
            }
            Err(e) => eprintln!("Error: {e}"),
        },
        other => {
            eprintln!(
                "Error: unknown command or invalid arguments: \"{other}\". Enter \".help\" for help"
            );
        }
    }
    false
}

fn render_query_result(res: &QueryResult, cfg: &ShellConfig) {
    let ncols = res.column_names.len();
    if ncols == 0 {
        return;
    }

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

    match cfg.mode {
        OutputMode::List => {
            if cfg.headers {
                println!("{}", res.column_names.join(&cfg.separator));
            }
            for r in &formatted_rows {
                println!("{}", r.join(&cfg.separator));
            }
        }
        OutputMode::Csv => {
            if cfg.headers {
                let hdr: Vec<String> = res.column_names.iter().map(|s| csv_escape(s)).collect();
                println!("{}", hdr.join(","));
            }
            for r in &formatted_rows {
                let esc: Vec<String> = r.iter().map(|s| csv_escape(s)).collect();
                println!("{}", esc.join(","));
            }
        }
        OutputMode::Line => {
            let max_w = res
                .column_names
                .iter()
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0);
            for (ri, r) in formatted_rows.iter().enumerate() {
                if ri > 0 {
                    println!();
                }
                for (ci, val) in r.iter().enumerate() {
                    println!("{:>width$} = {val}", res.column_names[ci], width = max_w);
                }
            }
        }
        OutputMode::Json => {
            print!("[");
            for (ri, row) in res.rows.iter().enumerate() {
                if ri > 0 {
                    print!(",");
                }
                print!("{{");
                for (ci, v) in row.values().iter().enumerate() {
                    if ci > 0 {
                        print!(",");
                    }
                    print!("{}:{}", json_quote(&res.column_names[ci]), json_value(v));
                }
                print!("}}");
            }
            println!("]");
        }
        OutputMode::Table | OutputMode::Box => {
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
                print_box_border("┌", "┬", "┐", "─", &widths);
                if cfg.headers {
                    print_box_row(&res.column_names, &widths, "│");
                    print_box_border("├", "┼", "┤", "─", &widths);
                }
                for r in &formatted_rows {
                    print_box_row(r, &widths, "│");
                }
                print_box_border("╰", "┴", "╯", "─", &widths);
            } else {
                print_box_border("+", "+", "+", "-", &widths);
                if cfg.headers {
                    print_box_row(&res.column_names, &widths, "|");
                    print_box_border("+", "+", "+", "-", &widths);
                }
                for r in &formatted_rows {
                    print_box_row(r, &widths, "|");
                }
                print_box_border("+", "+", "+", "-", &widths);
            }
        }
    }
}

fn print_box_border(left: &str, mid: &str, right: &str, horiz: &str, widths: &[usize]) {
    print!("{left}");
    for (i, &w) in widths.iter().enumerate() {
        if i > 0 {
            print!("{mid}");
        }
        print!("{}", horiz.repeat(w + 2));
    }
    println!("{right}");
}

fn print_box_row(cells: &[String], widths: &[usize], sep: &str) {
    print!("{sep}");
    for (i, cell) in cells.iter().enumerate() {
        let pad = widths[i].saturating_sub(cell.chars().count());
        print!(" {cell}{} {sep}", " ".repeat(pad));
    }
    println!();
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
    println!("  -box                 set output mode to 'box'");
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

fn print_dot_help() {
    println!(".dbinfo                Show status information about the database header");
    println!(".dump                  Render database content as SQL text");
    println!(".exit / .quit          Exit this program");
    println!(".headers on|off        Turn display of headers on or off");
    println!(".help                  Show this help message");
    println!(".indexes ?TABLE?       Show names of indexes");
    println!(".mode MODE             Set output mode (box, table, list, csv, json, line)");
    println!(".nullvalue STRING      Use STRING in place of NULL values");
    println!(".open ?FILE?           Close existing database and reopen FILE");
    println!(".read FILE             Read input SQL from FILE");
    println!(".schema ?PATTERN?      Show the CREATE statements matching PATTERN");
    println!(".separator COL         Change the column separator used by list mode");
    println!(".tables ?TABLE?        List names of tables matching LIKE pattern TABLE");
}
