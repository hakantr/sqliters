//! `.import`: a port of `dotCmdImport()`, `csv_read_one_field()`, `ascii_read_one_field()`
//! and `zAutoColumn()` from `shell.c.in` (3.54.1).
#![allow(clippy::too_many_lines)]

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};

use sqliters::Value;

use crate::dotcmd::table_exists;
use crate::{MODE_ASCII, ShellState, c_string, integer_value};

const EOF: i32 = -1;

struct ImportCtx {
    z_file: String,
    input: Box<dyn Read>,
    peeked: Option<i32>,
    z: Vec<u8>,
    n_line: i32,
    n_row: i32,
    n_err: i32,
    b_not_first: bool,
    c_term: i32,
    c_col_sep: i32,
    c_row_sep: i32,
    c_q_escape: i32,
    c_uq_escape: i32,
}

impl ImportCtx {
    fn getc(&mut self) -> i32 {
        if let Some(c) = self.peeked.take() {
            return c;
        }
        let mut b = [0u8; 1];
        match self.input.read(&mut b) {
            Ok(1) => i32::from(b[0]),
            _ => EOF,
        }
    }

    /// `csv_read_one_field()`: `false` at end of input.
    fn csv_read_one_field(&mut self) -> bool {
        let c_sep = self.c_col_sep;
        let r_sep = self.c_row_sep;
        self.z.clear();
        let mut c = self.getc();
        if c == EOF {
            self.c_term = EOF;
            return false;
        }
        if c == i32::from(b'"') {
            let start_line = self.n_line;
            let c_quote = c;
            let c_esc = self.c_q_escape;
            let mut pc = 0i32;
            let mut ppc = 0i32;
            loop {
                c = self.getc();
                if c == r_sep {
                    self.n_line += 1;
                }
                if c == c_esc && c_esc != 0 {
                    c = self.getc();
                    self.z.push(c as u8);
                    ppc = 0;
                    pc = 0;
                    continue;
                }
                if c == c_quote && pc == c_quote {
                    pc = 0;
                    continue;
                }
                if (c == c_sep && pc == c_quote)
                    || (c == r_sep && pc == c_quote)
                    || (c == r_sep && pc == i32::from(b'\r') && ppc == c_quote)
                    || (c == EOF && pc == c_quote)
                {
                    // do{ p->n--; }while( p->z[p->n]!=cQuote );
                    while let Some(last) = self.z.pop() {
                        if i32::from(last) == c_quote {
                            break;
                        }
                    }
                    self.c_term = c;
                    break;
                }
                if pc == c_quote && c != i32::from(b'\r') {
                    eprintln!(
                        "{}:{}: unescaped {} character",
                        self.z_file, self.n_line, c_quote as u8 as char
                    );
                }
                if c == EOF {
                    eprintln!(
                        "{}:{}: unterminated {}-quoted field",
                        self.z_file, start_line, c_quote as u8 as char
                    );
                    self.c_term = c;
                    break;
                }
                self.z.push(c as u8);
                ppc = pc;
                pc = c;
            }
        } else {
            let c_esc = self.c_uq_escape;
            if (c & 0xff) == 0xef && !self.b_not_first {
                self.z.push(c as u8);
                c = self.getc();
                if (c & 0xff) == 0xbb {
                    self.z.push(c as u8);
                    c = self.getc();
                    if (c & 0xff) == 0xbf {
                        self.b_not_first = true;
                        self.z.clear();
                        return self.csv_read_one_field();
                    }
                }
            }
            while c != EOF && c != c_sep && c != r_sep {
                if c == c_esc && c_esc != 0 {
                    c = self.getc();
                }
                self.z.push(c as u8);
                c = self.getc();
            }
            if c == r_sep {
                self.n_line += 1;
                if self.z.last() == Some(&b'\r') {
                    self.z.pop();
                }
            }
            self.c_term = c;
        }
        self.b_not_first = true;
        true
    }

    /// `ascii_read_one_field()`.
    fn ascii_read_one_field(&mut self) -> bool {
        let c_sep = self.c_col_sep;
        let r_sep = self.c_row_sep;
        self.z.clear();
        let mut c = self.getc();
        if c == EOF {
            self.c_term = EOF;
            return false;
        }
        while c != EOF && c != c_sep && c != r_sep {
            self.z.push(c as u8);
            c = self.getc();
        }
        if c == r_sep {
            self.n_line += 1;
        }
        self.c_term = c;
        true
    }

    fn read(&mut self, ascii: bool) -> bool {
        if ascii {
            self.ascii_read_one_field()
        } else {
            self.csv_read_one_field()
        }
    }

    /// The field as the C string the C code binds.
    fn field(&self) -> String {
        let z = crate::qrf::cstr(&self.z);
        String::from_utf8_lossy(z).into_owned()
    }
}

/// `zAutoColumn()`: the column list of the table to create, and the renames that were
/// necessary (the "minimal one pass" renaming of the C shell, computed directly).
fn auto_columns(names_in: &[String]) -> (Option<String>, Option<String>) {
    if names_in.is_empty() {
        return (None, None);
    }
    // INSERT ... VALUES(iif(length(?1)>0,?1,'?'), ...)
    let names: Vec<String> = names_in
        .iter()
        .map(|n| {
            if n.is_empty() {
                "?".to_owned()
            } else {
                n.clone()
            }
        })
        .collect();
    let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let n = names.len();
    let repeated: Vec<bool> = (0..n)
        .map(|i| (0..n).any(|j| j != i && lower[j] == lower[i]))
        .collect();
    let has_dupes = repeated.iter().any(|&r| r);
    let mut suff: Vec<String> = vec![String::new(); n];
    if has_dupes {
        // SELECT CAST(ceil(log(count(*)+0.5)) AS INT)
        let n_digits = ((n as f64 + 0.5).log10().ceil()) as usize;
        let candidate = |i: usize, nlz: usize| -> String {
            if repeated[i] {
                format!(
                    "{}_{}{:0w$}",
                    lower[i],
                    "0".repeat(nlz),
                    i + 1,
                    w = n_digits
                )
            } else {
                lower[i].clone()
            }
        };
        let mut nlz = 0usize;
        loop {
            let collide =
                (0..n).any(|i| (0..n).any(|j| j != i && candidate(i, nlz) == candidate(j, nlz)));
            if !collide || nlz > 100 {
                break;
            }
            nlz += 1;
        }
        for i in 0..n {
            if repeated[i] {
                suff[i] = format!("_{}{}", "0".repeat(nlz), i + 1);
            }
        }
    }
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let mut spec = String::from("(\n");
    for i in 0..n {
        if i > 0 {
            spec.push(',');
            // ','||iif((cpos-1)%4>0, ' ', x'0a'||' ')   (cpos = i+1)
            if i % 4 > 0 {
                spec.push(' ');
            } else {
                spec.push_str("\n ");
            }
        }
        spec.push_str(&q(&format!("{}{}", names[i], suff[i])));
    }
    spec.push(')');
    let renamed = if has_dupes {
        let v: Vec<String> = (0..n)
            .filter(|&i| !suff[i].is_empty())
            .map(|i| {
                format!(
                    "{} to {}",
                    q(&names[i]),
                    q(&format!("{}{}", names[i], suff[i]))
                )
            })
            .collect();
        Some(v.join(",\n"))
    } else {
        None
    };
    (Some(spec), renamed)
}

/// `dotCmdImport()`.
pub fn dot_cmd_import(p: &mut ShellState) -> i32 {
    let n_arg = p.dot.n_arg;
    let az = p.dot.az_arg.clone();
    if p.fail_if_safe_mode("cannot run .import in safe mode") {
        return 1;
    }
    let mut z_table: Option<String> = None;
    let mut z_schema: Option<String> = None;
    let mut z_file: Option<String> = None;
    let mut e_verbose = 0;
    let mut n_skip: i64 = 0;
    let mut ascii = p.mode.e_mode == MODE_ASCII;
    let mut c_col_sep = 0i32;
    let mut c_row_sep = 0i32;
    let mut c_uq_escape = 0i32;
    let mut c_q_escape = 0i32;
    let first = |s: &str| i32::from(s.as_bytes().first().copied().unwrap_or(0));
    let mut i = 1usize;
    while i < n_arg {
        let zf = az[i].as_str();
        let z = if zf.starts_with("--") { &zf[1..] } else { zf };
        if !z.starts_with('-') {
            if z_file.is_none() {
                z_file = Some(z.to_owned());
            } else if z_table.is_none() {
                z_table = Some(z.to_owned());
            } else {
                p.dot_cmd_error(i, Some("unknown argument"), None);
                return 1;
            }
        } else if z == "-v" {
            e_verbose += 1;
        } else if matches!(
            z,
            "-schema" | "-skip" | "-esc" | "-qesc" | "-colsep" | "-rowsep"
        ) {
            if i == n_arg - 1 {
                p.dot_cmd_error(i, Some("missing argument"), None);
                return 1;
            }
            i += 1;
            match z {
                "-schema" => z_schema = Some(az[i].clone()),
                "-skip" => n_skip = integer_value(&az[i]),
                "-esc" => c_uq_escape = first(&az[i]),
                "-qesc" => c_q_escape = first(&az[i]),
                "-colsep" => c_col_sep = first(&az[i]),
                _ => c_row_sep = first(&az[i]),
            }
        } else if z == "-ascii" {
            if c_col_sep == 0 {
                c_col_sep = 0x1f;
            }
            if c_row_sep == 0 {
                c_row_sep = 0x1e;
            }
            ascii = true;
        } else if z == "-csv" {
            if c_col_sep == 0 {
                c_col_sep = i32::from(b',');
            }
            if c_row_sep == 0 {
                c_row_sep = i32::from(b'\n');
            }
            ascii = false;
        } else {
            p.dot_cmd_error(i, Some("unknown option"), None);
            return 1;
        }
        i += 1;
    }
    let Some(z_table) = z_table else {
        let loc = p.error_location();
        eprintln!(
            "{loc} Missing {} argument\n",
            if z_file.is_none() { "FILE" } else { "TABLE" }
        );
        return 1;
    };
    let z_file = z_file.unwrap_or_default();
    if !p.open_db(false) {
        return 1;
    }
    if c_col_sep == 0 {
        c_col_sep = match p.mode.spec.z_column_sep.as_deref() {
            Some(s) if !s.is_empty() => i32::from(s.as_bytes()[0]),
            _ => i32::from(b','),
        };
    }
    if c_col_sep & 0x80 != 0 {
        eprintln!("Error: .import column separator must be ASCII");
        return 1;
    }
    if c_row_sep == 0 {
        c_row_sep = match p.mode.spec.z_row_sep.as_deref() {
            Some(s) if !s.is_empty() => i32::from(s.as_bytes()[0]),
            _ => i32::from(b'\n'),
        };
    }
    if c_row_sep == i32::from(b'\r') && !ascii {
        c_row_sep = i32::from(b'\n');
    }
    if c_row_sep & 0x80 != 0 {
        eprintln!("Error: .import row separator must be ASCII");
        return 1;
    }
    let mut ctx_file = z_file.clone();
    let mut n_line = 1i32;
    let mut i_line_offset: i64 = 0;
    let mut child: Option<std::process::Child> = None;
    let input: Box<dyn Read>;
    if let Some(cmd) = z_file.strip_prefix('|') {
        match Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(mut ch) => {
                let out = ch.stdout.take();
                child = Some(ch);
                ctx_file = "<pipe>".to_owned();
                match out {
                    Some(o) => input = Box::new(BufReader::new(o)),
                    None => {
                        let loc = p.error_location();
                        eprintln!("{loc} cannot open \"{z_file}\"");
                        return 1;
                    }
                }
            }
            Err(_) => {
                let loc = p.error_location();
                eprintln!("{loc} cannot open \"{z_file}\"");
                return 1;
            }
        }
    } else if z_file.starts_with("<<") && z_file.len() > 2 {
        // Content comes from the following lines of the script up to the end mark.
        let end_mark = z_file[2..].as_bytes().to_vec();
        let i_start = p.lineno;
        ctx_file = p.z_in_file.clone().unwrap_or_default();
        n_line = (p.lineno + 1) as i32;
        i_line_offset = p.lineno;
        let mut content: Vec<u8> = Vec::new();
        let mut found = false;
        loop {
            let mut line: Vec<u8> = Vec::new();
            let got = match &mut p.input {
                Some(r) => r.read_until(b'\n', &mut line).unwrap_or(0),
                None => std::io::stdin()
                    .lock()
                    .read_until(b'\n', &mut line)
                    .unwrap_or(0),
            };
            if got == 0 {
                break;
            }
            if line.starts_with(&end_mark) {
                found = true;
                if line.ends_with(b"\n") {
                    p.lineno += 1;
                }
                break;
            }
            if line.ends_with(b"\n") {
                p.lineno += 1;
            }
            content.extend_from_slice(&line);
        }
        if !found {
            let saved = p.lineno;
            p.lineno = i_start;
            let loc = p.error_location();
            eprintln!("{loc} Content terminator \"{}\" not found.", &z_file[2..]);
            p.lineno = saved;
            return 1;
        }
        // (The C code keeps the content as a C string.)
        let content = crate::qrf::cstr(&content).to_vec();
        input = Box::new(std::io::Cursor::new(content));
    } else {
        match std::fs::File::open(&z_file) {
            Ok(f) if f.metadata().is_ok_and(|m| !m.is_dir()) => input = Box::new(BufReader::new(f)),
            _ => {
                let loc = p.error_location();
                eprintln!("{loc} cannot open \"{z_file}\"");
                return 1;
            }
        }
    }
    let mut s_ctx = ImportCtx {
        z_file: ctx_file,
        input,
        peeked: None,
        z: Vec::new(),
        n_line,
        n_row: 0,
        n_err: 0,
        b_not_first: false,
        c_term: 0,
        c_col_sep,
        c_row_sep,
        c_q_escape,
        c_uq_escape,
    };
    if e_verbose >= 1 {
        let cs = (c_col_sep as u8 as char).to_string();
        let rs = (c_row_sep as u8 as char).to_string();
        p.outs(&format!(
            "Column separator {}, row separator {}\n",
            c_string(&cs),
            c_string(&rs)
        ));
    }
    while n_skip > 0 {
        n_skip -= 1;
        while s_ctx.read(ascii) && s_ctx.c_term == s_ctx.c_col_sep {}
    }
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let finish_child = |child: &mut Option<std::process::Child>| {
        if let Some(mut ch) = child.take() {
            let _ = ch.wait();
        }
    };
    if !table_exists(p.db(), z_schema.as_deref(), &z_table) {
        let view_sql = format!(
            "SELECT count(*) FROM {}.sqlite_schema WHERE name='{}' COLLATE nocase AND type='view'",
            q(z_schema.as_deref().unwrap_or("main")),
            z_table.replace('\'', "''")
        );
        let is_view = matches!(p.db().query_row(&view_sql, &[]), Ok(Some(r)) if matches!(r.get(0), Some(Value::Integer(x)) if *x>0));
        if !is_view {
            let mut names: Vec<String> = Vec::new();
            while s_ctx.read(ascii) {
                names.push(s_ctx.field());
                if s_ctx.c_term != s_ctx.c_col_sep {
                    break;
                }
            }
            let (col_defs, renames) = auto_columns(&names);
            if let Some(r) = renames {
                eprintln!(
                    "Columns renamed during .import {} due to duplicates:\n{r}",
                    s_ctx.z_file
                );
            }
            let Some(col_defs) = col_defs else {
                eprintln!("{}: empty file", s_ctx.z_file);
                finish_child(&mut child);
                return 1;
            };
            let z_create = format!(
                "CREATE TABLE {}.{}{col_defs}\n",
                q(z_schema.as_deref().unwrap_or("main")),
                q(&z_table)
            );
            if e_verbose >= 1 {
                p.outs(&format!("{z_create}\n"));
            }
            if let Err(e) = p.db().execute_batch(&z_create) {
                eprintln!("{z_create} failed:\n{}", e.message);
                finish_child(&mut child);
                return 1;
            }
        }
    }
    let sq = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let cnt_sql = match &z_schema {
        Some(s) => format!(
            "SELECT count(*) FROM pragma_table_info({},{});",
            sq(&z_table),
            sq(s)
        ),
        None => format!(
            "SELECT count(*) FROM pragma_table_info({},NULL);",
            sq(&z_table)
        ),
    };
    let n_col = match p.db().query_row(&cnt_sql, &[]) {
        Ok(Some(r)) => match r.get(0) {
            Some(Value::Integer(n)) => *n as usize,
            _ => 0,
        },
        Ok(None) => 0,
        Err(e) => {
            eprintln!("Error: {}", e.message);
            finish_child(&mut child);
            return 1;
        }
    };
    if n_col == 0 {
        finish_child(&mut child);
        return 0;
    }
    let mut z_sql = match &z_schema {
        Some(s) => format!("INSERT INTO {}.{} VALUES(?", q(s), q(&z_table)),
        None => format!("INSERT INTO {} VALUES(?", q(&z_table)),
    };
    for _ in 1..n_col {
        z_sql.push_str(",?");
    }
    z_sql.push(')');
    if e_verbose >= 2 {
        p.outs(&format!("Insert using: {z_sql}\n"));
    }
    let mut stmt = match p.db().prepare(&z_sql) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {}", e.message);
            finish_child(&mut child);
            return 1;
        }
    };
    let need_commit = p.db().is_autocommit();
    if need_commit {
        let _ = p.db().execute_batch("BEGIN");
    }
    loop {
        let start_line = s_ctx.n_line;
        let mut i = 0usize;
        while i < n_col {
            let got = s_ctx.read(ascii);
            if !got && i == 0 {
                break;
            }
            if p.mode.e_mode == MODE_ASCII
                && (!got || s_ctx.z.is_empty() || s_ctx.z[0] == 0)
                && i == 0
            {
                break;
            }
            let z: Option<String> = if got {
                Some(s_ctx.field())
            } else if !ascii && i == n_col - 1 && i > 0 {
                Some(String::new())
            } else {
                None
            };
            let _ = match &z {
                Some(t) => stmt.bind_text(i + 1, t).map(|_| ()),
                None => stmt.bind(i + 1, Value::Null).map(|_| ()),
            };
            if i < n_col - 1 && s_ctx.c_term != s_ctx.c_col_sep {
                if i == 0 && matches!(z.as_deref(), Some("\n" | "\r\n")) {
                    break;
                }
                eprintln!(
                    "{}:{}: expected {} columns but found {} - filling the rest with NULL",
                    s_ctx.z_file,
                    start_line,
                    n_col,
                    i + 1
                );
                i += 2;
                while i <= n_col {
                    let _ = stmt.bind(i, Value::Null);
                    i += 1;
                }
            }
            i += 1;
        }
        if s_ctx.c_term == s_ctx.c_col_sep {
            loop {
                s_ctx.read(ascii);
                i += 1;
                if s_ctx.c_term != s_ctx.c_col_sep {
                    break;
                }
            }
            eprintln!(
                "{}:{}: expected {} columns but found {} - extras ignored",
                s_ctx.z_file, start_line, n_col, i
            );
        }
        if i >= n_col {
            let r1 = stmt.step().map(|_| ());
            let r2 = stmt.reset();
            if let Err(e) = r1.and(r2) {
                eprintln!(
                    "{}:{}: INSERT failed: {}",
                    s_ctx.z_file, start_line, e.message
                );
                s_ctx.n_err += 1;
                if p.bail_on_error {
                    break;
                }
            } else {
                s_ctx.n_row += 1;
            }
        }
        if s_ctx.c_term == EOF {
            break;
        }
    }
    let _ = stmt.finalize();
    finish_child(&mut child);
    if need_commit {
        let _ = p.db().execute_batch("COMMIT");
    }
    if e_verbose > 0 {
        p.outs(&format!(
            "Added {} rows with {} errors using {} lines of input\n",
            s_ctx.n_row,
            s_ctx.n_err,
            i64::from(s_ctx.n_line) - 1 - i_line_offset
        ));
    }
    i32::from(s_ctx.n_err != 0)
}
