//! `sqliters` command-line shell: a port of SQLite's `shell.c.in` (3.54.1) for the parts this
//! shell implements. Structure and names follow the C source: `ShellState`, `Mode`,
//! `modeChange`, `process_input`, `runOneSqlLine`, `shell_exec`, `do_meta_command`.
#![forbid(unsafe_code)]
// The code follows the C functions it ports statement by statement (index loops over argv,
// explicit prefix tests): the lints below would only move it away from the original.
#![allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::needless_range_loop,
    clippy::manual_strip,
    clippy::sliced_string_as_bytes,
    clippy::while_let_loop
)]

mod dotcmd;
mod help;
mod import;
mod qrf;
mod util;

use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::process::{Child, ExitCode};

use qrf::{
    QRF_ALWAYS, QRF_AUTO, QRF_BLOB_AUTO, QRF_BLOB_JSON, QRF_BLOB_SQL, QRF_BLOB_TEXT, QRF_NO,
    QRF_STYLE_AUTO, QRF_STYLE_EQP, QRF_STYLE_EXPLAIN, QRF_STYLE_HTML, QRF_TEXT_JSON,
    QRF_TEXT_RELAXED, QRF_TEXT_SQL, QRF_YES, QrfSpec,
};
use sqliters::{Connection, OpenFlags, SQLITE_SOURCE_ID, SQLITE_VERSION, SqliteError, Value};

pub const MFLG_ECHO: u8 = 0x01;
pub const MFLG_CRLF: u8 = 0x02;
pub const MFLG_HDR: u8 = 0x04;

pub const CLISTAT_VMSTEP: u8 = 0x01;
pub const CLISTAT_STMT: u8 = 0x02;
pub const CLISTAT_MEM: u8 = 0x04;
pub const CLISTAT_XSTMT: u8 = 0x08;
pub const CLISTAT_XMEM: u8 = 0x10;
pub const CLISTAT_IO: u8 = 0x20;
pub const CLISTAT_ZIPVFS: u8 = 0x40;
pub const CLISTAT_ON: u8 = 0x17;
pub const CLISTAT_ALL: u8 = 0x7f;
pub const CLISTAT_RESET: u8 = 0x80;

pub const SHFLG_BACKSLASH: u32 = 0x0000_0004;
pub const SHFLG_PRESERVE_ROWID: u32 = 0x0000_0008;
pub const SHFLG_COUNT_CHANGES: u32 = 0x0000_0020;
pub const SHFLG_DUMP_DATA_ONLY: u32 = 0x0000_0100;
pub const SHFLG_DUMP_NO_SYS: u32 = 0x0000_0200;
pub const SHFLG_TESTING_MODE: u32 = 0x0000_0400;

pub const MODE_ASCII: u8 = 0;
pub const MODE_COLUMN: u8 = 3;
pub const MODE_CSV: u8 = 5;
pub const MODE_INSERT: u8 = 7;
pub const MODE_LIST: u8 = 12;
pub const MODE_QBOX: u8 = 16;
pub const MODE_SPLIT: u8 = 18;
pub const MODE_WWW: u8 = 22;
pub const MODE_BATCH: u8 = 50;
pub const MODE_TTY: u8 = 51;
pub const MODE_USER: u8 = 75;
pub const MODE_N_USER: usize = 25;

pub const DFLT_CHAR_LIMIT: i32 = 300;
pub const DFLT_LINE_LIMIT: i16 = 5;
pub const DFLT_TITLE_LIMIT: i16 = 20;
pub const DFLT_MULTI_INSERT: u32 = 3000;

const MAX_INPUT_NESTING: i32 = 25;

/// `ModeInfo`: a built-in display mode.
pub struct ModeInfo {
    pub name: &'static str,
    pub e_csep: u8,
    pub e_rsep: u8,
    pub e_null: u8,
    pub e_text: u8,
    pub e_hdr: u8,
    pub e_blob: u8,
    pub b_hdr: u8,
    pub e_style: u8,
    pub e_cx: u8,
    pub m_flg: u8,
}

/// `aModeStr[]`.
pub const A_MODE_STR: [Option<&str>; 14] = [
    None,
    Some("\n"),
    Some("|"),
    Some(" "),
    Some(","),
    Some("\r\n"),
    Some("\x1e"),
    Some("\x1f"),
    Some("\t"),
    Some(""),
    Some("NULL"),
    Some("null"),
    Some("\"\""),
    Some(": "),
];

macro_rules! mi {
    ($n:expr, $c:expr, $r:expr, $nu:expr, $t:expr, $h:expr, $b:expr, $bh:expr, $s:expr, $cx:expr, $f:expr) => {
        ModeInfo {
            name: $n,
            e_csep: $c,
            e_rsep: $r,
            e_null: $nu,
            e_text: $t,
            e_hdr: $h,
            e_blob: $b,
            b_hdr: $bh,
            e_style: $s,
            e_cx: $cx,
            m_flg: $f,
        }
    };
}

/// `aModeInfo[]`.
pub const A_MODE_INFO: [ModeInfo; 23] = [
    mi!("ascii", 7, 6, 9, 1, 1, 0, 1, 12, 0, 0),
    mi!("box", 0, 0, 9, 1, 1, 0, 3, 1, 2, 0),
    mi!("c", 4, 1, 10, 5, 5, 4, 1, 12, 0, 0),
    mi!("column", 0, 0, 9, 1, 1, 0, 3, 2, 2, 0),
    mi!("count", 0, 0, 0, 0, 0, 0, 0, 3, 0, 0),
    mi!("csv", 4, 5, 9, 3, 3, 0, 1, 12, 0, 0),
    mi!("html", 0, 0, 9, 4, 4, 0, 3, 7, 0, 0),
    mi!("insert", 0, 0, 10, 2, 2, 0, 1, 8, 0, 0),
    mi!("jatom", 4, 1, 11, 6, 6, 0, 1, 12, 0, 0),
    mi!("jobject", 0, 1, 11, 6, 6, 0, 0, 10, 0, 0),
    mi!("json", 0, 0, 11, 6, 6, 0, 0, 9, 0, 0),
    mi!("line", 13, 1, 9, 1, 1, 0, 0, 11, 1, 0),
    mi!("list", 2, 1, 9, 1, 1, 0, 1, 12, 0, 0),
    mi!("markdown", 0, 0, 9, 1, 1, 0, 3, 13, 2, 0),
    mi!("off", 0, 0, 0, 0, 0, 0, 0, 14, 0, 0),
    mi!("psql", 0, 0, 9, 1, 1, 0, 3, 19, 2, 5),
    mi!("qbox", 0, 0, 10, 2, 1, 0, 3, 1, 2, 0),
    mi!("quote", 4, 1, 10, 2, 2, 0, 1, 12, 0, 0),
    mi!("split", 0, 0, 9, 1, 1, 0, 1, 2, 2, 2),
    mi!("table", 0, 0, 9, 1, 1, 0, 3, 19, 2, 0),
    mi!("tabs", 8, 1, 9, 3, 3, 0, 1, 12, 0, 0),
    mi!("tcl", 3, 1, 12, 5, 5, 4, 1, 12, 0, 0),
    mi!("www", 0, 0, 9, 4, 4, 0, 3, 7, 0, 0),
];

/// `Mode`: everything that determines how query results are rendered.
#[derive(Clone, Default)]
pub struct Mode {
    pub auto_explain: u8,
    pub auto_eqp: u8,
    pub scanstats_on: u8,
    pub b_auto_screen_width: bool,
    pub m_flags: u8,
    pub e_mode: u8,
    pub spec: QrfSpec,
}

/// Where `p->out` goes when it is not standard output.
pub enum Redirect {
    File(io::BufWriter<File>),
    Pipe(Child),
    /// `.output memory`: the text is kept until `.output --show`.
    Memory(Vec<u8>),
    Null,
}

/// A writer with the buffering of a C `FILE*` on a non-terminal: a fixed buffer that is filled
/// completely before it is written out (so the chunk boundaries are those of stdio).
pub struct StdioBuf<W: Write> {
    inner: W,
    buf: Vec<u8>,
    cap: usize,
}

impl<W: Write> StdioBuf<W> {
    pub fn new(inner: W, cap: usize) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(cap),
            cap,
        }
    }
}

impl<W: Write> Write for StdioBuf<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut d = data;
        if self.buf.len() + d.len() < self.cap {
            self.buf.extend_from_slice(d);
            return Ok(data.len());
        }
        // Fill the buffer, write it, then write whole blocks directly.
        if !self.buf.is_empty() {
            let room = self.cap - self.buf.len();
            self.buf.extend_from_slice(&d[..room]);
            d = &d[room..];
            let _ = self.inner.write_all(&self.buf);
            self.buf.clear();
        }
        let whole = d.len() - d.len() % self.cap;
        if whole > 0 {
            let _ = self.inner.write_all(&d[..whole]);
            d = &d[whole..];
        }
        self.buf.extend_from_slice(d);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let _ = self.inner.write_all(&self.buf);
            self.buf.clear();
        }
        self.inner.flush()
    }
}

/// `struct AuxDb`.
#[derive(Default)]
pub struct AuxDb {
    pub db: Option<Connection>,
    pub z_db_filename: Option<String>,
}

/// `struct DotCmdLine`.
#[derive(Default)]
pub struct DotCmdLine {
    pub z_orig: String,
    pub az_arg: Vec<String>,
    pub ai_ofst: Vec<usize>,
    pub ab_quot: Vec<bool>,
    pub n_arg: usize,
}

/// `ShellState`.
pub struct ShellState {
    pub db: Option<Connection>,
    pub open_readonly: bool,
    pub open_create: bool,
    pub open_flags_set: bool,
    pub open_nofollow: bool,
    pub open_deserialize: bool,
    pub b_safe_mode: bool,
    pub b_safe_mode_persist: bool,
    pub e_restore_state: u8,
    pub m_stats: u8,
    pub n_pop_output: u8,
    pub n_pop_mode: u8,
    pub enable_timer: u8,
    pub input_nesting: i32,
    pub prev_timer: f64,
    pub lineno: i64,
    pub z_in_file: Option<String>,
    pub input: Option<Box<dyn BufRead>>,
    pub stdout: StdioBuf<io::Stdout>,
    pub redirect: Option<Redirect>,
    pub pipe_out: Option<StdioBuf<std::process::ChildStdin>>,
    pub n_err: i32,
    pub writable_schema: bool,
    pub shell_flgs: u32,
    pub z_err_prefix: Option<String>,
    pub mode: Mode,
    pub mode_stack: Vec<Mode>,
    pub saved_modes: Vec<(String, Mode)>,
    pub aux: [AuxDb; 5],
    pub i_aux: usize,
    pub dot: DotCmdLine,
    pub outfile: String,
    pub bail_on_error: bool,
    pub stdin_is_interactive: bool,
    pub stdout_is_console: bool,
    pub stdout_tty_width: i32,
    pub argv0: String,
    pub prompts: [Option<String>; 2],
    pub busy_timeout: i64,
    /// Shell-side copies of `sqlite3_db_config` booleans the engine has no switch for (shown
    /// by `.dbconfig`; index = position in the `.dbconfig` list).
    pub dbcfg_shadow: [i32; 22],
    /// Shell-side copies of `sqlite3_limit` values the engine has no setting for.
    pub limits: [i32; 15],
    pub exit_code: Option<i32>,
}

impl ShellState {
    /// `p->out`.
    pub fn out(&mut self) -> &mut dyn Write {
        match &mut self.redirect {
            None => &mut self.stdout,
            Some(Redirect::File(f)) => f,
            Some(Redirect::Pipe(_)) => match &mut self.pipe_out {
                Some(w) => w,
                None => &mut self.stdout,
            },
            Some(Redirect::Memory(v)) => v,
            Some(Redirect::Null) => &mut self.stdout,
        }
    }

    pub fn out_bytes(&mut self, b: &[u8]) {
        if matches!(self.redirect, Some(Redirect::Null)) {
            return;
        }
        let _ = self.out().write_all(b);
    }

    pub fn outs(&mut self, s: &str) {
        self.out_bytes(s.as_bytes());
    }

    /// `cli_printf(stdout, ...)`.
    pub fn stdouts(&mut self, s: &str) {
        let _ = self.stdout.write_all(s.as_bytes());
    }

    pub fn has_flag(&self, f: u32) -> bool {
        self.shell_flgs & f != 0
    }

    pub fn db(&self) -> &Connection {
        self.db.as_ref().expect("database is open")
    }

    /// `modeSetLimit()`: 0 on success.
    pub fn mode_set_limit(&mut self, z_arg: Option<&str>) -> bool {
        let k = match z_arg {
            None | Some("off") => 1,
            Some("on") => 0,
            Some(_) => -1,
        };
        if k == 0 {
            self.mode.spec.n_line_limit = DFLT_LINE_LIMIT;
            self.mode.spec.n_char_limit = DFLT_CHAR_LIMIT;
            self.mode.spec.n_title_limit = DFLT_TITLE_LIMIT;
        } else if k == 1 {
            self.mode.spec.n_line_limit = 0;
            self.mode.spec.n_char_limit = 0;
            self.mode.spec.n_title_limit = 0;
        } else {
            // sscanf(zArg, "%d,%d,%d", &L, &C, &T)
            let mut nums: Vec<i64> = Vec::new();
            let mut rest = z_arg.unwrap_or("");
            loop {
                let t = rest.trim_start();
                let b = t.as_bytes();
                let mut i = usize::from(!b.is_empty() && (b[0] == b'-' || b[0] == b'+'));
                let ds = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == ds {
                    break;
                }
                nums.push(t[..i].parse::<i64>().unwrap_or(i64::MAX));
                rest = &t[i..];
                if nums.len() == 3 || !rest.starts_with(',') {
                    break;
                }
                rest = &rest[1..];
            }
            if nums.len() < 2 || nums.iter().any(|&x| x < 0) {
                return true;
            }
            self.mode.spec.n_line_limit = nums[0] as i16;
            self.mode.spec.n_char_limit = nums[1] as i32;
            if nums.len() == 3 {
                self.mode.spec.n_title_limit = nums[2] as i16;
            }
        }
        false
    }

    /// `modeFree()`.
    pub fn mode_free(&mut self) {
        let auto_explain = self.mode.auto_explain;
        self.mode = Mode::default();
        self.mode.auto_explain = auto_explain;
    }

    /// `modeChange()`.
    pub fn mode_change(&mut self, e_mode: u8) {
        if usize::from(e_mode) < A_MODE_INFO.len() {
            let pi = &A_MODE_INFO[usize::from(e_mode)];
            let pm = &mut self.mode;
            pm.e_mode = e_mode;
            if pi.e_csep != 0 {
                pm.spec.z_column_sep = A_MODE_STR[usize::from(pi.e_csep)].map(str::to_owned);
            }
            if pi.e_rsep != 0 {
                pm.spec.z_row_sep = A_MODE_STR[usize::from(pi.e_rsep)].map(str::to_owned);
            }
            if pi.e_null != 0 {
                pm.spec.z_null = A_MODE_STR[usize::from(pi.e_null)].map(str::to_owned);
            }
            pm.spec.e_text = pi.e_text;
            pm.spec.e_blob = pi.e_blob;
            if pm.m_flags & MFLG_HDR == 0 {
                pm.spec.b_titles = pi.b_hdr;
            }
            pm.spec.e_title = pi.e_hdr;
            pm.spec.b_border = if pi.m_flg & 0x01 != 0 {
                QRF_NO
            } else {
                QRF_AUTO
            };
            if pi.m_flg & 0x02 != 0 {
                pm.spec.b_split_column = QRF_YES;
                pm.b_auto_screen_width = true;
            } else {
                pm.spec.b_split_column = QRF_NO;
            }
            pm.spec.b_row_count = if pi.m_flg & 0x04 != 0 {
                QRF_YES
            } else {
                QRF_NO
            };
        } else if e_mode >= MODE_USER && usize::from(e_mode - MODE_USER) < self.saved_modes.len() {
            self.mode = self.saved_modes[usize::from(e_mode - MODE_USER)].1.clone();
        } else if e_mode == MODE_BATCH {
            let m_flags = self.mode.m_flags;
            self.mode_free();
            self.mode_change(MODE_LIST);
            self.mode_set_limit(Some("off"));
            self.mode.m_flags = m_flags;
        } else if e_mode == MODE_TTY {
            let m_flags = self.mode.m_flags;
            self.mode_free();
            self.mode_change(MODE_QBOX);
            self.mode.b_auto_screen_width = true;
            self.mode.spec.e_text = QRF_TEXT_RELAXED;
            self.mode.spec.n_char_limit = DFLT_CHAR_LIMIT;
            self.mode.spec.n_line_limit = DFLT_LINE_LIMIT;
            self.mode.spec.b_text_jsonb = QRF_YES;
            self.mode.spec.n_title_limit = DFLT_TITLE_LIMIT;
            self.mode.spec.n_multi_insert = DFLT_MULTI_INSERT;
            self.mode.m_flags = m_flags;
        }
    }

    /// `modeDefault()`.
    fn mode_default(&mut self) {
        self.mode.auto_explain = 1;
        if self.stdin_is_interactive || self.stdout_is_console {
            self.mode_change(MODE_TTY);
        } else {
            self.mode_change(MODE_BATCH);
        }
    }

    /// `modeFind()`.
    pub fn mode_find(&self, name: &str) -> i32 {
        if let Some(i) = A_MODE_INFO.iter().position(|m| m.name == name) {
            return i as i32;
        }
        if let Some(i) = self.saved_modes.iter().position(|m| m.0 == name) {
            return i as i32 + i32::from(MODE_USER);
        }
        match name {
            "batch" => i32::from(MODE_BATCH),
            "tty" => i32::from(MODE_TTY),
            _ => -1,
        }
    }

    pub fn mode_push(&mut self) {
        self.mode_stack.push(self.mode.clone());
    }

    pub fn mode_pop(&mut self) {
        if let Some(m) = self.mode_stack.pop() {
            self.mode = m;
        }
    }

    /// `shellErrorLocation()`.
    pub fn error_location(&self) -> String {
        if let Some(p) = &self.z_err_prefix {
            p.clone()
        } else {
            match self.z_in_file.as_deref() {
                None | Some("<stdin>") => format!("line {}:", self.lineno),
                Some(f) => format!("{f}:{}:", self.lineno),
            }
        }
    }

    /// `dotCmdError()`.
    pub fn dot_cmd_error(&mut self, i_arg: usize, z_brief: Option<&str>, z_detail: Option<&str>) {
        let loc = self.error_location();
        if let Some(brief) = z_brief
            && i_arg < self.dot.n_arg
        {
            let i = self.dot.ai_ofst[i_arg];
            let n_prompt = brief.len() + 5;
            eprintln!("{loc} {}", self.dot.z_orig);
            if i > n_prompt {
                eprintln!("{loc} {:w$}{brief} ---^", "", w = 1 + i - n_prompt);
            } else {
                eprintln!("{loc} {:i$}^--- {brief}", "");
            }
        }
        if let Some(d) = z_detail {
            eprintln!("{loc} {d}");
        }
    }

    /// `failIfSafeMode()`: prints the message and returns true when the shell must exit(1).
    pub fn fail_if_safe_mode(&mut self, msg: &str) -> bool {
        if self.b_safe_mode {
            let loc = self.error_location();
            eprintln!("{loc} {msg}");
            self.exit_code = Some(1);
            return true;
        }
        false
    }

    /// `shellScreenWidth()`.
    pub fn screen_width(&self) -> i32 {
        if self.stdout_tty_width > 0 {
            return self.stdout_tty_width;
        }
        if let Ok(c) = env::var("COLUMNS")
            && let Ok(n) = c.trim().parse::<i32>()
            && n > 0
        {
            return n;
        }
        80
    }

    /// `output_reset()`.
    pub fn output_reset(&mut self) {
        let _ = self.stdout.flush();
        match self.redirect.take() {
            Some(Redirect::Pipe(mut child)) => {
                if let Some(mut w) = self.pipe_out.take() {
                    let _ = w.flush();
                    drop(w);
                }
                let _ = child.wait();
            }
            Some(Redirect::File(mut f)) => {
                if self.mode.e_mode == MODE_WWW {
                    let _ = f.write_all(b"</PRE></BODY></HTML>\n");
                }
                let _ = f.flush();
            }
            Some(Redirect::Null | Redirect::Memory(_)) | None => {
                if self.mode.e_mode == MODE_WWW {
                    self.stdouts("</PRE></BODY></HTML>\n");
                }
            }
        }
        self.outfile.clear();
    }

    /// The database file name of the active connection (`p->pAuxDb->zDbFilename`).
    pub fn db_filename(&self) -> Option<&str> {
        self.aux[self.i_aux].z_db_filename.as_deref()
    }

    /// `open_db()`. With `keep_alive` an open failure falls back to an in-memory database;
    /// otherwise the shell exits (returns false with `exit_code` set).
    pub fn open_db(&mut self, keep_alive: bool) -> bool {
        if self.db.is_none() {
            let mut name = self.db_filename().map(str::to_owned);
            if let Some(n) = &name
                && n.starts_with("~/")
                && !std::path::Path::new(n).exists()
                && let Ok(home) = env::var("HOME")
                && !home.is_empty()
            {
                name = Some(format!("{home}{}", &n[1..]));
            }
            if !self.open_flags_set {
                self.open_create = true;
                self.open_readonly = false;
                self.open_flags_set = true;
            }
            let res = open_connection(
                name.as_deref(),
                self.open_readonly,
                self.open_create,
                self.open_nofollow,
                self.open_deserialize,
            );
            match res {
                Ok(c) => self.db = Some(c),
                Err(e) => {
                    // sqlite3_errmsg() of a failed open is the text of the result code.
                    let msg = match e.code {
                        sqliters::ErrorCode::NoMem
                        | sqliters::ErrorCode::NotADb
                        | sqliters::ErrorCode::Misuse => e.message.clone(),
                        _ => "unable to open database file".to_owned(),
                    };
                    eprintln!(
                        "Error: unable to open database \"{}\": {}",
                        name.as_deref().unwrap_or(""),
                        msg
                    );
                    if !keep_alive {
                        self.exit_code = Some(1);
                        return false;
                    }
                    match Connection::open_in_memory() {
                        Ok(c) => {
                            eprintln!(
                                "Notice: using substitute in-memory database instead of \"{}\"",
                                name.as_deref().unwrap_or("")
                            );
                            self.db = Some(c);
                        }
                        Err(_) => {
                            eprintln!("Also: unable to open substitute in-memory database.");
                            self.exit_code = Some(1);
                            return false;
                        }
                    }
                }
            }
            let testmode_on = self.has_flag(SHFLG_TESTING_MODE);
            if let Some(db) = &self.db {
                let _ = db.set_trusted_schema(testmode_on);
                let _ = db.db_config_defensive(Some(!testmode_on));
                if self.busy_timeout > 0 {
                    let _ = db.busy_timeout(self.busy_timeout as u64);
                }
            }
            self.dbcfg_shadow = dotcmd::DBCFG_DEFAULTS;
            self.limits = dotcmd::LIMIT_DEFAULTS;
        }
        true
    }
}

/// `sqlite3_open_v2()` for the shell: `None` / "" = private temporary database, ":memory:" =
/// in-memory database, anything else a file (or a `file:` URI).
fn open_connection(
    name: Option<&str>,
    readonly: bool,
    create: bool,
    nofollow: bool,
    deserialize: bool,
) -> sqliters::Result<Connection> {
    match name {
        None | Some("") => Connection::open_private_temporary(),
        Some(":memory:") => Connection::open_in_memory(),
        Some(path) => {
            if deserialize {
                let c = Connection::open_in_memory()?;
                // readFile() failure: the connection stays an empty database.
                if let Ok(data) = std::fs::read(path)
                    && let Err(e) = c.deserialize(&data, false)
                {
                    eprintln!("Error: sqlite3_deserialize() returns {}", e.code as i32);
                }
                return Ok(c);
            }
            let mut flags = if readonly {
                OpenFlags::READONLY
            } else if create {
                OpenFlags(OpenFlags::READWRITE.0 | OpenFlags::CREATE.0)
            } else {
                OpenFlags::READWRITE
            };
            flags = OpenFlags(flags.0 | OpenFlags::URI.0);
            if nofollow {
                flags = OpenFlags(flags.0 | OpenFlags::NOFOLLOW.0);
            }
            Connection::open_with_flags(path, flags)
        }
    }
}

/// `line_is_all_whitespace()`.
fn line_is_all_whitespace(z: &[u8]) -> bool {
    let at = |i: usize| z.get(i).copied().unwrap_or(0);
    let mut i = 0usize;
    while at(i) != 0 {
        let c = at(i);
        if is_space(c) {
            i += 1;
            continue;
        }
        if c == b'/' && at(i + 1) == b'*' {
            i += 2;
            while at(i) != 0 && (at(i) != b'*' || at(i + 1) != b'/') {
                i += 1;
            }
            if at(i) == 0 {
                return false;
            }
            i += 2;
            continue;
        }
        if c == b'-' && at(i + 1) == b'-' {
            i += 2;
            while at(i) != 0 && at(i) != b'\n' {
                i += 1;
            }
            if at(i) == 0 {
                return true;
            }
            i += 1;
            continue;
        }
        return false;
    }
    true
}

/// C `isspace()` in the "C" locale.
pub fn is_space(c: u8) -> bool {
    matches!(c, b' ' | 9..=13)
}

/// `sqlite3_incomplete()` (complete.c of 3.54.1): 0 when `z` is one or more complete
/// statements, `SQLITE_EMPTY` (16) when it holds nothing, otherwise a description of what is
/// missing: bits 0-7 = 1, bits 8-15 = what ends the statement (1 `;`, 2 `END;`, 3 `;END;`),
/// bits 16-23 = the unmatched structure character, bits 32.. = open parentheses.
pub fn sqlite3_incomplete(z: &[u8]) -> i64 {
    const TRANS: [[u8; 8]; 8] = [
        [1, 0, 2, 3, 4, 2, 2, 2],
        [1, 1, 2, 3, 4, 2, 2, 2],
        [1, 2, 2, 2, 2, 2, 2, 2],
        [1, 3, 3, 2, 4, 2, 2, 2],
        [1, 4, 2, 2, 2, 4, 5, 2],
        [6, 5, 5, 5, 5, 5, 5, 5],
        [6, 6, 5, 5, 5, 5, 5, 7],
        [1, 7, 5, 5, 5, 5, 5, 5],
    ];
    const STATEMAP: [u8; 8] = [0, 0, 1, 1, 1, 3, 2, 1];
    // (A C string ends at its first NUL.)
    let z = if z.contains(&0) {
        crate::qrf::cstr(z)
    } else {
        z
    };
    let n = z.len();
    let id_char = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;
    let mut state = 0u8;
    let mut pending = 0u8;
    let mut n_paren: i32 = 0;
    let mut i = 0usize;
    'outer: while i < n {
        // In the states NORMAL (2) and TRIGGER (5) every token but a semicolon leaves the
        // state as it is, so only the bytes that start something to skip or to count matter.
        if state == 2 || state == 5 {
            while i < n
                && !matches!(
                    z[i],
                    b';' | b'/' | b'-' | b'[' | b'`' | b'"' | b'\'' | b'(' | b')'
                )
            {
                i += 1;
            }
            if i >= n {
                break;
            }
        }
        let token: usize;
        match z[i] {
            b';' => token = 0,
            b' ' | b'\r' | b'\t' | b'\n' | 0x0c => token = 1,
            b'/' => {
                if i + 1 >= n || z[i + 1] != b'*' {
                    token = 2;
                } else {
                    i += 2;
                    while i < n && (z[i] != b'*' || i + 1 >= n || z[i + 1] != b'/') {
                        i += 1;
                    }
                    if i >= n {
                        pending = b'/';
                        break 'outer;
                    }
                    i += 1;
                    token = 1;
                }
            }
            b'-' => {
                if i + 1 >= n || z[i + 1] != b'-' {
                    token = 2;
                } else {
                    while i < n && z[i] != b'\n' {
                        i += 1;
                    }
                    if i >= n {
                        pending = b'-';
                        break 'outer;
                    }
                    token = 1;
                }
            }
            b'[' => {
                i += 1;
                while i < n && z[i] != b']' {
                    i += 1;
                }
                if i >= n {
                    pending = b']';
                    break 'outer;
                }
                token = 2;
            }
            c @ (b'`' | b'"' | b'\'') => {
                i += 1;
                while i < n && z[i] != c {
                    i += 1;
                }
                if i >= n {
                    pending = c;
                    break 'outer;
                }
                token = 2;
            }
            b'(' => {
                n_paren += 1;
                token = 2;
            }
            b')' => {
                n_paren -= 1;
                token = 2;
            }
            c => {
                if id_char(c) {
                    let mut n_id = 1usize;
                    while i + n_id < n && id_char(z[i + n_id]) {
                        n_id += 1;
                    }
                    token = if matches!(c, b'c' | b'C' | b't' | b'T' | b'e' | b'E') {
                        let w = &z[i..i + n_id];
                        match c {
                            b'c' | b'C' if w.eq_ignore_ascii_case(b"create") => 4,
                            b't' | b'T' if w.eq_ignore_ascii_case(b"trigger") => 6,
                            b't' | b'T'
                                if w.eq_ignore_ascii_case(b"temp")
                                    || w.eq_ignore_ascii_case(b"temporary") =>
                            {
                                5
                            }
                            b'e' | b'E' if w.eq_ignore_ascii_case(b"end") => 7,
                            b'e' | b'E' if w.eq_ignore_ascii_case(b"explain") => 3,
                            _ => 2,
                        }
                    } else {
                        2
                    };
                    i += n_id - 1;
                } else {
                    token = 2;
                }
            }
        }
        state = TRANS[usize::from(state)][token];
        i += 1;
    }
    if state == 0 && pending == 0 {
        return 16;
    }
    if state == 1 {
        n_paren = 0;
    }
    ((i64::from(n_paren as u32)) << 32)
        | (i64::from(pending) << 16)
        | (i64::from(STATEMAP[usize::from(state)]) << 8)
        | i64::from(state > 1 || pending != 0)
}

/// `line_is_complete()`.
fn line_is_complete(z_sql: &[u8]) -> bool {
    let x = sqlite3_incomplete(z_sql);
    if x == 0x00 || x == 0x10 {
        return true;
    }
    if x == 0x2d_0001 {
        let mut t = z_sql.to_vec();
        t.push(b'\n');
        let x = sqlite3_incomplete(&t);
        if x == 0x00 || x == 0x10 {
            return true;
        }
    }
    false
}

/// `fgets()`: reads up to `max-1` bytes, stopping after a newline. `None` at end of file.
fn fgets(r: &mut dyn BufRead, max: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    while out.len() + 1 < max {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(_) => break,
        };
        if buf.is_empty() {
            break;
        }
        let room = max - 1 - out.len();
        let take = buf.len().min(room);
        if let Some(p) = buf[..take].iter().position(|&c| c == b'\n') {
            out.extend_from_slice(&buf[..=p]);
            r.consume(p + 1);
            return Some(out);
        }
        out.extend_from_slice(&buf[..take]);
        r.consume(take);
    }
    if out.is_empty() { None } else { Some(out) }
}

/// `local_getline()`: one line of input without its line terminator (LF or CR LF). Like the C
/// function it works on C strings: a NUL in the input hides the rest of that read chunk.
pub fn local_getline(r: &mut dyn BufRead) -> Option<Vec<u8>> {
    // The common case: the whole line is in the reader's buffer and holds no NUL. Then the
    // chunked reads below produce exactly the line without its terminator.
    if let Ok(buf) = r.fill_buf()
        && let Some(pos) = buf
            .split(|&c| c == b'\n')
            .next()
            .map(<[u8]>::len)
            .filter(|&n| n < buf.len())
    {
        let line = &buf[..pos];
        if !line.contains(&0) {
            let end = if pos > 0 && line[pos - 1] == b'\r' {
                pos - 1
            } else {
                pos
            };
            let z = line[..end].to_vec();
            r.consume(pos + 1);
            return Some(z);
        }
    }
    let mut z: Vec<u8> = Vec::new();
    let mut n_line = 0usize;
    let mut n = 0usize;
    loop {
        if n + 100 > n_line {
            n_line = n_line * 2 + 100;
        }
        let Some(chunk) = fgets(r, n_line - n) else {
            if n == 0 {
                return None;
            }
            z.truncate(n);
            break;
        };
        z.truncate(n);
        let len = chunk.iter().position(|&c| c == 0).unwrap_or(chunk.len());
        z.extend_from_slice(&chunk[..len]);
        n += len;
        if n > 0 && z[n - 1] == b'\n' {
            n -= 1;
            if n > 0 && z[n - 1] == b'\r' {
                n -= 1;
            }
            z.truncate(n);
            break;
        }
    }
    Some(z)
}

/// `integerValue()`.
pub fn integer_value(z_arg: &str) -> i64 {
    const MULT: [(&str, u64); 9] = [
        ("KiB", 1024),
        ("MiB", 1024 * 1024),
        ("GiB", 1024 * 1024 * 1024),
        ("KB", 1000),
        ("MB", 1_000_000),
        ("GB", 1_000_000_000),
        ("K", 1000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
    ];
    let mut z = z_arg.as_bytes();
    let mut is_neg = false;
    if z.first() == Some(&b'-') {
        is_neg = true;
        z = &z[1..];
    } else if z.first() == Some(&b'+') {
        z = &z[1..];
    }
    let overflow = if is_neg { i64::MIN } else { i64::MAX };
    let mut v: u64 = 0;
    if z.len() >= 2 && z[0] == b'0' && z[1] == b'x' {
        z = &z[2..];
        while let Some(x) = z.first().and_then(|c| (*c as char).to_digit(16)) {
            if v > 0x0fff_ffff_ffff_ffff {
                return overflow;
            }
            v = (v << 4) + u64::from(x);
            z = &z[1..];
        }
    } else {
        while let Some(&c) = z.first() {
            if !c.is_ascii_digit() {
                break;
            }
            if v >= 922_337_203_685_477_580 && (v > 922_337_203_685_477_580 || c >= b'8') {
                return overflow;
            }
            v = v * 10 + u64::from(c - b'0');
            z = &z[1..];
        }
    }
    for (suffix, mult) in MULT {
        if z.eq_ignore_ascii_case(suffix.as_bytes()) {
            if 0x7fff_ffff_ffff_ffff / mult < v {
                return overflow;
            }
            v *= mult;
            break;
        }
    }
    if is_neg && v > 0x7fff_ffff_ffff_ffff {
        return overflow;
    }
    if is_neg {
        (v as i64).wrapping_neg()
    } else {
        v as i64
    }
}

/// `booleanValue()`.
pub fn boolean_value(z_arg: &str) -> i32 {
    let b = z_arg.as_bytes();
    let i = if b.len() >= 2 && b[0] == b'0' && b[1] == b'x' {
        2 + b[2..].iter().take_while(|c| c.is_ascii_hexdigit()).count()
    } else {
        b.iter().take_while(|c| c.is_ascii_digit()).count()
    };
    if i > 0 && i == b.len() {
        return (integer_value(z_arg) & 0xffff_ffff) as i32;
    }
    if z_arg.eq_ignore_ascii_case("on") || z_arg.eq_ignore_ascii_case("yes") {
        return 1;
    }
    if z_arg.eq_ignore_ascii_case("off") || z_arg.eq_ignore_ascii_case("no") {
        return 0;
    }
    eprintln!("ERROR: Not a boolean value: \"{z_arg}\". Assuming \"no\".");
    0
}

/// `resolve_backslashes()`.
pub fn resolve_backslashes(z: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(z.len());
    let mut i = 0usize;
    while i < z.len() {
        let mut c = z[i];
        if c == b'\\' && i + 1 < z.len() {
            i += 1;
            c = z[i];
            match c {
                b'a' => c = 7,
                b'b' => c = 8,
                b't' => c = b'\t',
                b'n' => c = b'\n',
                b'v' => c = 0x0b,
                b'f' => c = 0x0c,
                b'r' => c = b'\r',
                b'x' => {
                    let mut nhd = 0usize;
                    let mut hv = 0u8;
                    while nhd < 2 {
                        let Some(d) = z.get(i + 1 + nhd).and_then(|x| (*x as char).to_digit(16))
                        else {
                            break;
                        };
                        hv = (hv << 4) | d as u8;
                        nhd += 1;
                    }
                    i += nhd;
                    c = hv;
                }
                b'0'..=b'7' => {
                    let mut v = u32::from(c - b'0');
                    if z.get(i + 1).is_some_and(|x| (b'0'..=b'7').contains(x)) {
                        i += 1;
                        v = (v << 3) + u32::from(z[i] - b'0');
                        if z.get(i + 1).is_some_and(|x| (b'0'..=b'7').contains(x)) {
                            i += 1;
                            v = (v << 3) + u32::from(z[i] - b'0');
                        }
                    }
                    c = v as u8;
                }
                _ => {}
            }
        }
        if c == 0 {
            // (The result is a C string.)
            break;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// `output_c_string()` / `append_c_string()`.
pub fn c_string(z: &str) -> String {
    let mut out = String::from("\"");
    for ch in z.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03o}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A failed SQL "line" (`shell_exec`): the error, whether it came from preparing a statement,
/// and the SQL text that was being prepared (for the caret context).
pub struct ShellSqlError {
    pub err: SqliteError,
    pub in_prepare: bool,
    pub sql: String,
    /// The error has no message of its own (`save_err_msg(db, "stepping", ...)`): it is
    /// reported as "Runtime error".
    pub stepping: bool,
}

/// CPU times of this process in seconds (user, system), from /proc.
fn cpu_times() -> (f64, f64) {
    let Ok(s) = std::fs::read_to_string("/proc/self/stat") else {
        return (0.0, 0.0);
    };
    let Some(pos) = s.rfind(')') else {
        return (0.0, 0.0);
    };
    let f: Vec<&str> = s[pos + 1..].split_whitespace().collect();
    let tick = 100.0;
    let u = f.get(11).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0) / tick;
    let sy = f.get(12).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0) / tick;
    (u, sy)
}

/// `bind_prepared_stmt()`.
fn bind_prepared_stmt(p: &ShellState, stmt: &mut sqliters::Statement) {
    let n_var = stmt.parameter_count();
    if n_var == 0 {
        return;
    }
    let db = p.db();
    let mut names: Vec<Option<String>> = vec![None; n_var + 1];
    for (name, idx) in stmt.named_parameters() {
        if *idx <= n_var {
            names[*idx] = Some(name.clone());
        }
    }
    let have_table = db
        .prepare("SELECT key FROM temp.sqlite_parameters LIMIT 0")
        .map(|s| s.finalize())
        .is_ok();
    for i in 1..=n_var {
        let z_var = names[i].clone().unwrap_or_else(|| format!("?{i}"));
        let mut bound = false;
        if have_table
            && let Ok(Some(row)) = db.query_row(
                "SELECT value FROM temp.sqlite_parameters WHERE key=?1",
                &[Value::Text(z_var.clone())],
            )
            && let Some(v) = row.get(0)
        {
            let _ = stmt.bind(i, v.clone());
            bound = true;
        }
        if bound {
            continue;
        }
        let like = |pat: &str| sqliters::vdbe::sqlite_like_match(pat, &z_var, None);
        if like("_NAN") {
            let _ = stmt.bind(i, Value::Null);
        } else if like("_INF") {
            let _ = stmt.bind(i, Value::Real(f64::INFINITY));
        } else if let Some(rest) = z_var.strip_prefix("$int_") {
            let n: i64 = rest
                .bytes()
                .take_while(u8::is_ascii_digit)
                .fold(0i64, |a, c| {
                    a.wrapping_mul(10).wrapping_add(i64::from(c - b'0'))
                });
            let _ = stmt.bind(i, Value::Integer(i64::from(n as i32)));
        } else if let Some(rest) = z_var.strip_prefix("$text_") {
            let _ = stmt.bind(i, Value::Text(rest.to_owned()));
        } else if z_var == "$TIMER" {
            let _ = stmt.bind(i, Value::Real(p.prev_timer));
        } else {
            let _ = stmt.bind(i, Value::Null);
        }
    }
}

/// `shell_exec()`: runs every statement of `z_sql` and renders the results.
pub fn shell_exec(p: &mut ShellState, z_sql: &str) -> Result<(), ShellSqlError> {
    // (The "count" mode prints a row count for every statement, also those without rows.)
    let plain = p.mode.auto_eqp == 0
        && p.m_stats == 0
        && p.mode.scanstats_on == 0
        && A_MODE_INFO[usize::from(p.mode.e_mode)].e_style != qrf::QRF_STYLE_COUNT;
    // Statements that produce no rows and take no parameters go through the engine's batch
    // executor: the observable behaviour is the same, error classification included.
    if plain && util::is_pure_non_returning_batch_sql(z_sql) {
        let db = p.db();
        return match db.execute_batch(z_sql) {
            Ok(()) => Ok(()),
            Err(err) => {
                let mut rest = z_sql;
                loop {
                    rest = rest.trim_start_matches(|c: char| c.is_ascii() && is_space(c as u8));
                    if rest.is_empty() {
                        break;
                    }
                    match db.prepare(rest) {
                        Err(pe) => {
                            return Err(ShellSqlError {
                                err: pe,
                                in_prepare: true,
                                sql: rest.to_owned(),
                                stepping: false,
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
                    stepping: false,
                })
            }
        };
    }
    let mut spec = p.mode.spec.clone();
    if p.mode.e_mode == MODE_INSERT && p.has_flag(SHFLG_PRESERVE_ROWID) {
        spec.b_titles = QRF_YES;
    }
    let e_style = A_MODE_INFO[usize::from(p.mode.e_mode)].e_style;
    if p.mode.b_auto_screen_width {
        spec.n_screen_width = p.screen_width().min(i32::from(i16::MAX)) as i16;
    }
    if spec.e_blob == QRF_BLOB_AUTO {
        spec.e_blob = match spec.e_text {
            QRF_TEXT_RELAXED | QRF_TEXT_SQL => QRF_BLOB_SQL,
            QRF_TEXT_JSON => QRF_BLOB_JSON,
            _ => QRF_BLOB_TEXT,
        };
    }
    let mut z = z_sql;
    while !z.is_empty() {
        // SQLITE_LIMIT_SQL_LENGTH (`.limit sql_length`): the tokenizer of the C library gives
        // up once the tokens of a statement exceed it.
        let mx_sql = p.limits[1];
        let prep = if mx_sql < 1_000_000_000
            && sqliters::syntax::first_statement_end_offset(z) > mx_sql.max(0) as usize
        {
            Err(SqliteError::new(
                sqliters::ErrorCode::TooBig,
                "string or blob too big",
            ))
        } else {
            p.db().prepare(z)
        };
        let mut stmt = match prep {
            Ok(s) => s,
            Err(err) => {
                return Err(ShellSqlError {
                    err,
                    in_prepare: true,
                    sql: z.to_owned(),
                    stepping: false,
                });
            }
        };
        let tail_len = stmt.tail().len();
        let cur_len = z.len();
        let stmt_sql = &z[..cur_len - tail_len];
        if stmt_sql.bytes().all(|c| is_space(c) || c == b';')
            || line_is_all_whitespace(stmt_sql.as_bytes())
        {
            // "this happens for a comment or white-space"
            let _ = stmt.finalize();
            z = &z[cur_len - tail_len..];
            z = z.trim_start_matches(|c: char| c.is_ascii() && is_space(c as u8));
            if tail_len == cur_len {
                break;
            }
            continue;
        }
        let is_explain = qrf::stmt_isexplain(stmt_sql);
        if p.b_safe_mode && util::first_keyword_is(stmt_sql, "attach") {
            // safeModeAuth(): SQLITE_ATTACH is refused in safe mode (the shell exits).
            let _ = stmt.finalize();
            p.fail_if_safe_mode("cannot run ATTACH in safe mode");
            return Ok(());
        }
        if p.mode.auto_eqp != 0 && is_explain == 0 && p.dot.n_arg == 0 {
            let saved_timer = p.enable_timer;
            p.enable_timer = 0;
            let mut s2 = spec.clone();
            s2.e_style = QRF_STYLE_AUTO;
            let eqp_sql = format!("EXPLAIN QUERY PLAN {stmt_sql}");
            if let Ok(mut es) = p.db().prepare(&eqp_sql) {
                let mut buf = Vec::new();
                let _ = qrf::format_query_result(&mut es, 2, &s2, &mut buf);
                let _ = es.finalize();
                p.out_bytes(&buf);
            }
            if p.mode.auto_eqp >= 3 {
                let ex_sql = format!("EXPLAIN {stmt_sql}");
                if let Ok(mut es) = p.db().prepare(&ex_sql) {
                    let mut buf = Vec::new();
                    let _ = qrf::format_query_result(&mut es, 1, &s2, &mut buf);
                    let _ = es.finalize();
                    p.out_bytes(&buf);
                }
            }
            p.enable_timer = saved_timer;
        }
        bind_prepared_stmt(p, &mut stmt);
        let mut s2 = spec.clone();

        let rc = if is_explain != 0 && p.mode.auto_explain != 0 {
            s2.e_style = if is_explain == 1 {
                QRF_STYLE_EXPLAIN
            } else {
                QRF_STYLE_EQP
            };
            run_qrf(p, &mut stmt, is_explain, &s2)
        } else if p.mode.e_mode == MODE_WWW {
            p.outs("</PRE>\n<TABLE border='1' cellspacing='0' cellpadding='2'>\n");
            s2.e_style = QRF_STYLE_HTML;
            let _ = run_qrf(p, &mut stmt, is_explain, &s2);
            p.outs("</TABLE>\n<PRE>");
            Ok(())
        } else {
            s2.e_style = e_style;
            run_qrf(p, &mut stmt, is_explain, &s2)
        };
        if p.m_stats != 0 {
            dotcmd::display_stats(p);
        }
        if p.mode.scanstats_on != 0 {
            // The scan-status styles are "not available in this build" of the C shell either:
            // the message goes to stderr and the SQL line fails with the connection's
            // (unset) error message.
            eprintln!("Stats query failed: not available in this build");
            let _ = stmt.finalize();
            return Err(ShellSqlError {
                err: SqliteError::new(sqliters::ErrorCode::Error, "not an error"),
                in_prepare: false,
                sql: String::new(),
                stepping: true,
            });
        }
        let fin = stmt.finalize();
        if let Err(err) = rc.and(fin) {
            return Err(ShellSqlError {
                err,
                in_prepare: false,
                sql: String::new(),
                stepping: false,
            });
        }
        if tail_len == cur_len {
            break;
        }
        z = &z[cur_len - tail_len..];
        z = z.trim_start_matches(|c: char| c.is_ascii() && is_space(c as u8));
    }
    Ok(())
}

/// Runs the formatter with the shell's current output as the sink.
fn run_qrf(
    p: &mut ShellState,
    stmt: &mut sqliters::Statement,
    is_explain: u8,
    spec: &QrfSpec,
) -> Result<(), SqliteError> {
    if matches!(p.redirect, Some(Redirect::Null)) {
        let mut sink = io::sink();
        return qrf::format_query_result(stmt, is_explain, spec, &mut sink);
    }
    qrf::format_query_result(stmt, is_explain, spec, p.out())
}

/// `doAutoDetectRestore()`.
fn do_auto_detect_restore(p: &mut ShellState, z_sql: &str) {
    if p.e_restore_state >= 7 {
        return;
    }
    match p.e_restore_state {
        0 => {
            let expect = "PRAGMA foreign_keys=OFF;";
            p.e_restore_state = if !p.b_safe_mode && z_sql == expect {
                1
            } else {
                7
            };
        }
        1 => {
            let mut is_dump = false;
            if z_sql == "BEGIN TRANSACTION;" {
                is_dump = !matches!(
                    p.db().query_row("SELECT 1 FROM sqlite_schema LIMIT 1", &[]),
                    Ok(Some(_))
                );
            }
            if is_dump {
                let defense = p.db().db_config_defensive(None);
                let dqs_ddl = p.dbcfg_shadow[dotcmd::DBCFG_DQS_DDL] != 0;
                p.db().db_config_defensive(Some(false));
                p.dbcfg_shadow[dotcmd::DBCFG_DQS_DDL] = 1;
                p.e_restore_state = (if defense { 2 } else { 0 }) + (if dqs_ddl { 4 } else { 0 });
                if p.e_restore_state == 0 {
                    // (State 0 would restart the detection; C has the same quirk.)
                }
            } else {
                p.e_restore_state = 7;
            }
        }
        _ => {
            if p.db().is_autocommit() {
                if p.e_restore_state & 2 != 0 {
                    p.db().db_config_defensive(Some(true));
                }
                if p.e_restore_state & 4 != 0 {
                    p.dbcfg_shadow[dotcmd::DBCFG_DQS_DDL] = 0;
                }
                p.e_restore_state = 7;
            }
        }
    }
}

/// `runOneSqlLine()`: returns the number of errors (0 or 1).
pub fn run_one_sql_line(
    p: &mut ShellState,
    z_sql: &[u8],
    z_filename: Option<&str>,
    startline: i64,
) -> i32 {
    if !p.open_db(false) {
        return 1;
    }
    let owned;
    let bytes: &[u8] = if p.has_flag(SHFLG_BACKSLASH) {
        owned = resolve_backslashes(z_sql);
        &owned
    } else {
        z_sql
    };
    let sql_cow;
    let sql: &str = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            sql_cow = String::from_utf8_lossy(bytes);
            &sql_cow
        }
    };
    let timer = p.enable_timer != 0;
    let (t0, cpu0) = if timer {
        (Some(std::time::Instant::now()), cpu_times())
    } else {
        (None, (0.0, 0.0))
    };
    let res = shell_exec(p, sql);
    if p.enable_timer != 0
        && let Some(t0) = t0
    {
        let cpu1 = cpu_times();
        p.prev_timer = t0.elapsed().as_secs_f64();
        let line = format!(
            "Run Time: real {:.6} user {:.6} sys {:.6}\n",
            p.prev_timer,
            cpu1.0 - cpu0.0,
            cpu1.1 - cpu0.1
        );
        p.outs(&line);
        if p.enable_timer == 1 {
            p.enable_timer = 0;
        }
    }
    match res {
        Err(e) => {
            let kind = if e.in_prepare {
                "Parse error"
            } else if e.stepping {
                "Runtime error"
            } else {
                "Error"
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
                    .or_else(|| util::guess_error_offset(&e.sql, &e.err.message));
                tail.push_str(&util::shell_error_context(&e.sql, offset));
            }
            let prefix = if z_filename.is_some() || !p.stdin_is_interactive {
                match z_filename {
                    Some("cmdline") => format!(
                        "{kind} in {} command line argument:",
                        util::ordinal(startline as usize)
                    ),
                    Some("<stdin>") => format!("{kind} near line {startline}:"),
                    Some(f) => format!("{kind} near line {startline} of {f}:"),
                    None => format!("{kind} near line {startline} of (null):"),
                }
            } else {
                format!("{kind}:")
            };
            eprintln!("{prefix} {tail}");
            1
        }
        Ok(()) => {
            if p.has_flag(SHFLG_COUNT_CHANGES) {
                let line = format!(
                    "changes: {}   total_changes: {}\n",
                    p.db().changes(),
                    p.db().total_changes()
                );
                p.outs(&line);
            }
            do_auto_detect_restore(p, sql);
            0
        }
    }
}

/// `echo_group_input()`.
fn echo_group_input(p: &mut ShellState, z: &[u8]) {
    if p.mode.m_flags & MFLG_ECHO != 0 {
        p.out_bytes(z);
        p.out_bytes(b"\n");
        let _ = p.out().flush();
    }
}

/// `process_input()`: reads `p.input` to its end. Returns true when there was an error.
pub fn process_input(p: &mut ShellState, z_src: &str) -> bool {
    if p.input_nesting == MAX_INPUT_NESTING {
        eprintln!(
            "{z_src}: Input nesting limit ({MAX_INPUT_NESTING}) reached at line {}. Check recursion.",
            p.lineno
        );
        return true;
    }
    p.input_nesting += 1;
    let saved_in_file = p.z_in_file.replace(z_src.to_owned());
    let saved_lineno = p.lineno;
    p.lineno = 0;
    let mut z_sql: Vec<u8> = Vec::new();
    let mut have_sql = false;
    let mut err_cnt = 0i32;
    let mut has_semi = false;
    let mut startline = 0i64;
    let interactive_in = p.input.is_none() && p.stdin_is_interactive;
    while err_cnt == 0 || !p.bail_on_error || interactive_in {
        let _ = p.out().flush();
        if interactive_in {
            let prompt = if have_sql {
                p.prompts[1]
                    .clone()
                    .unwrap_or_else(|| "   ...> ".to_owned())
            } else {
                p.prompts[0]
                    .clone()
                    .unwrap_or_else(|| "sqlite> ".to_owned())
            };
            p.stdouts(&prompt);
            let _ = p.stdout.flush();
        }
        let line = match &mut p.input {
            Some(r) => local_getline(r.as_mut()),
            None => {
                let stdin = io::stdin();
                let mut lock = stdin.lock();
                local_getline(&mut lock)
            }
        };
        let Some(z_line) = line else {
            if interactive_in {
                p.outs("\n");
            }
            break;
        };
        p.lineno += 1;
        if !have_sql && line_is_all_whitespace(&z_line) {
            echo_group_input(p, &z_line);
            continue;
        }
        if !have_sql && matches!(z_line.first(), Some(b'.' | b'#')) {
            echo_group_input(p, &z_line);
            if z_line[0] == b'.' {
                let rc = dotcmd::do_meta_command(&z_line, p);
                if rc == 2 || p.exit_code.is_some() {
                    break;
                } else if rc != 0 {
                    err_cnt += 1;
                }
            }
            continue;
        }
        if z_line.contains(&b';') {
            has_semi = true;
        }
        if !have_sql {
            let i = z_line.iter().take_while(|&&c| is_space(c)).count();
            z_sql.clear();
            z_sql.extend_from_slice(&z_line[i..]);
            startline = p.lineno;
            have_sql = !z_sql.is_empty();
            if !have_sql {
                // (nSql==0: the next line starts a fresh statement.)
                continue;
            }
        } else {
            z_sql.push(b'\n');
            z_sql.extend_from_slice(&z_line);
        }
        if have_sql && has_semi && line_is_complete(&z_sql) {
            echo_group_input(p, &z_sql);
            let src = p.z_in_file.take();
            err_cnt += run_one_sql_line(p, &z_sql, src.as_deref(), startline);
            p.z_in_file = src;
            have_sql = false;
            z_sql.clear();
            if p.n_pop_output != 0 {
                p.output_reset();
                p.n_pop_output = 0;
            }
            if p.n_pop_mode != 0 {
                p.mode_pop();
                p.n_pop_mode = 0;
            }
            p.b_safe_mode = p.b_safe_mode_persist;
            if p.exit_code.is_some() {
                break;
            }
        } else if have_sql && line_is_all_whitespace(&z_sql) {
            echo_group_input(p, &z_sql);
            have_sql = false;
            z_sql.clear();
        }
    }
    if have_sql {
        echo_group_input(p, &z_sql);
        let src = p.z_in_file.clone();
        err_cnt += run_one_sql_line(p, &z_sql, src.as_deref(), startline);
    }
    p.input_nesting -= 1;
    p.z_in_file = saved_in_file;
    p.lineno = saved_lineno;
    err_cnt > 0
}

/// `fileSize()`.
fn file_size(z: &str) -> i64 {
    std::fs::metadata(z).map_or(-1, |m| if m.is_file() { m.len() as i64 } else { -1 })
}

/// `isDatabaseFile()`.
fn is_database_file(z: &str) -> bool {
    let sz = file_size(z);
    if sz < 512 || sz % 512 != 0 {
        return false;
    }
    match Connection::open_with_flags(z, OpenFlags::READONLY) {
        Ok(c) => matches!(
            c.query_row("SELECT count(*) FROM sqlite_schema", &[]),
            Ok(Some(_))
        ),
        Err(_) => false,
    }
}

/// `isScriptFile()`.
fn is_script_file(z: &str) -> bool {
    let sz = file_size(z);
    if sz <= 0 {
        return false;
    }
    if sz % 512 == 0 && is_database_file(z) {
        return false;
    }
    let l = z.to_ascii_lowercase();
    l.ends_with(".sql") || l.ends_with(".txt")
}

/// `openChrSource()`.
pub fn open_chr_source(z: &str) -> Option<Box<dyn BufRead>> {
    use std::os::unix::fs::FileTypeExt;
    let md = std::fs::metadata(z).ok()?;
    let ft = md.file_type();
    if ft.is_file() || ft.is_fifo() || ft.is_char_device() {
        File::open(z)
            .ok()
            .map(|f| Box::new(BufReader::new(f)) as Box<dyn BufRead>)
    } else {
        None
    }
}

/// `find_home_dir()`: the home directory of the user in the password database
/// (`getpwuid(getuid())`), else `$HOME`.
fn find_home_dir() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    if let Ok(md) = std::fs::metadata("/proc/self") {
        let uid = md.uid();
        if let Ok(pw) = std::fs::read_to_string("/etc/passwd") {
            let uid_text = uid.to_string();
            for line in pw.lines() {
                let mut f = line.split(':');
                if f.nth(2) == Some(uid_text.as_str()) {
                    // name:passwd:uid:gid:gecos:dir:shell
                    if let Some(dir) = f.nth(2) {
                        return Some(dir.to_owned());
                    }
                }
            }
        }
    }
    env::var("HOME").ok()
}

/// `process_sqliterc()`.
fn process_sqliterc(p: &mut ShellState, override_file: Option<&str>) {
    let mut rc_file = override_file.map(str::to_owned);
    if rc_file.is_none() {
        // find_xdg_file("XDG_CONFIG_HOME", ".config", "sqlite3/sqliterc")
        let f = match env::var("XDG_CONFIG_HOME") {
            Ok(x) => Some(format!("{x}/sqlite3/sqliterc")),
            Err(_) => find_home_dir().map(|h| format!("{h}/.config/sqlite3/sqliterc")),
        };
        if let Some(f) = f
            && std::path::Path::new(&f).exists()
        {
            rc_file = Some(f);
        }
    }
    if rc_file.is_none() {
        match find_home_dir() {
            Some(h) => rc_file = Some(format!("{h}/.sqliterc")),
            None => {
                eprintln!("-- warning: cannot find home directory; cannot read ~/.sqliterc");
                return;
            }
        }
    }
    let rc_file = rc_file.unwrap_or_default();
    let saved_in = p.input.take();
    let saved_lineno = p.lineno;
    match File::open(&rc_file) {
        Ok(f) => {
            p.input = Some(Box::new(BufReader::new(f)));
            if p.stdin_is_interactive {
                eprintln!("-- Loading resources from {rc_file}");
            }
            if process_input(p, &rc_file) && p.bail_on_error {
                p.exit_code = Some(1);
            }
        }
        Err(_) => {
            if override_file.is_some() {
                eprintln!("cannot open: \"{rc_file}\"");
                if p.bail_on_error {
                    p.exit_code = Some(1);
                }
            }
        }
    }
    p.input = saved_in;
    p.lineno = saved_lineno;
}

const Z_OPTIONS: &str = concat!(
    "   --                   treat no subsequent arguments as options\n",
    "   -append              append the database to the end of the file\n",
    "   -ascii               set '.mode ascii'\n",
    "   -bail                stop after hitting an error\n",
    "   -batch               force batch I/O\n",
    "   -box                 set '.mode box'\n",
    "   -cmd COMMAND         run \"COMMAND\" before reading stdin\n",
    "   -cmdline-edit BOOL   enable or disable command-line editing\n",
    "   -column              set '.mode column'\n",
    "   -csv                 set '.mode csv -limits off'\n",
    "   -deserialize         open the database using sqlite3_deserialize()\n",
    "   -echo                print inputs before execution\n",
    "   -escape T            ctrl-char escape; T is one of: symbol, ascii, off\n",
    "   -init FILENAME       read/process named file\n",
    "   -[no]header          turn headers on or off\n",
    "   -help                show this message\n",
    "   -html                set '.mode html'\n",
    "   -ifexists            only open if database already exists\n",
    "   -interactive         force interactive I/O\n",
    "   -json                set '.mode json'\n",
    "   -line                set '.mode line'\n",
    "   -list                set '.mode list'\n",
    "   -lookaside SIZE N    use N entries of SZ bytes for lookaside memory\n",
    "   -markdown            set '.mode markdown'\n",
    "   -maxsize N           maximum size for a --deserialize database\n",
    "   -memtrace            trace all memory allocations and deallocations\n",
    "   -mmap N              default mmap size set to N\n",
    "   -newline SEP         set output row separator. Default: '\\n'\n",
    "   -nofollow            refuse to open symbolic links to database files\n",
    "   -noinit              Do not read the ~/.sqliterc file at startup\n",
    "   -nonce STRING        set the safe-mode escape nonce\n",
    "   -no-rowid-in-view    Disable rowid-in-view using sqlite3_config()\n",
    "   -nullvalue TEXT      set text string for NULL values. Default ''\n",
    "   -pagecache SIZE N    use N slots of SZ bytes each for page cache memory\n",
    "   -pcachetrace         trace all page cache operations\n",
    "   -quote               set '.mode quote'\n",
    "   -readonly            open the database read-only\n",
    "   -safe                enable safe-mode\n",
    "   -screenwidth N       use N as the default screenwidth \n",
    "   -separator SEP       set output column separator. Default: '|'\n",
    "   -stats               print memory stats before each finalize\n",
    "   -table               set '.mode table'\n",
    "   -tabs                set '.mode tabs'\n",
    "   -unsafe-testing      allow unsafe commands and modes for testing\n",
    "   -version             show SQLite version\n",
    "   -vfs NAME            use NAME as the default VFS\n",
    "   -vfstrace            enable tracing of all VFS calls\n",
);

/// `usage()`.
fn usage(argv0: &str, show_detail: bool) {
    eprint!(
        "Usage: {argv0} [OPTIONS] [FILENAME [SQL...]]\n\
         FILENAME is the name of an SQLite database. A new database is created\n\
         if the file does not previously exist. Defaults to :memory:.\n"
    );
    if show_detail {
        eprint!("OPTIONS include:\n{Z_OPTIONS}");
    } else {
        eprintln!("Use the -help option for additional information");
    }
}

fn atoi(z: &str) -> i32 {
    let t = z.trim_start();
    let b = t.as_bytes();
    let mut i = usize::from(!b.is_empty() && (b[0] == b'-' || b[0] == b'+'));
    let mut v: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = (v * 10 + i64::from(b[i] - b'0')).min(i64::from(i32::MAX) + 1);
        i += 1;
    }
    if b.first() == Some(&b'-') {
        (-v) as i32
    } else {
        v.min(i64::from(i32::MAX)) as i32
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let rc = shell_main(&argv);
    ExitCode::from((rc & 0xff) as u8)
}

fn shell_main(argv: &[String]) -> i32 {
    let argc = argv.len();
    let argv0 = argv
        .first()
        .cloned()
        .unwrap_or_else(|| "sqliters".to_owned());
    let mut p = ShellState {
        db: None,
        open_readonly: false,
        open_create: false,
        open_flags_set: false,
        open_nofollow: false,
        open_deserialize: false,
        b_safe_mode: false,
        b_safe_mode_persist: false,
        e_restore_state: 0,
        m_stats: 0,
        n_pop_output: 0,
        n_pop_mode: 0,
        enable_timer: 0,
        input_nesting: 0,
        prev_timer: 0.0,
        lineno: 0,
        z_in_file: None,
        input: None,
        stdout: StdioBuf::new(io::stdout(), 4096),
        redirect: None,
        pipe_out: None,
        n_err: 0,
        writable_schema: false,
        shell_flgs: 0,
        z_err_prefix: None,
        mode: Mode::default(),
        mode_stack: Vec::new(),
        saved_modes: Vec::new(),
        aux: Default::default(),
        i_aux: 0,
        dot: DotCmdLine::default(),
        outfile: String::new(),
        bail_on_error: false,
        stdin_is_interactive: io::stdin().is_terminal(),
        stdout_is_console: io::stdout().is_terminal(),
        stdout_tty_width: -1,
        argv0: argv0.clone(),
        prompts: [None, None],
        busy_timeout: 0,
        dbcfg_shadow: dotcmd::DBCFG_DEFAULTS,
        limits: dotcmd::LIMIT_DEFAULTS,
        exit_code: None,
    };
    let mut z_init_file: Option<String> = None;
    let mut warn_inmemory_db = false;
    let mut read_stdin = true;
    let mut no_init = false;
    let mut az_cmd: Vec<(String, usize)> = Vec::new();
    let mut n_opts_end = argc;
    let mut rc = 0i32;

    macro_rules! option_value {
        ($i:expr) => {{
            if $i >= argc {
                eprintln!("{}: Error: missing argument to {}", argv0, argv[argc - 1]);
                return 1;
            }
            argv[$i].as_str()
        }};
    }

    // First pass: the database name, the init file and the options that must be known early.
    let mut i = 1usize;
    while i < argc {
        let z_full = argv[i].as_str();
        if !z_full.starts_with('-') || i > n_opts_end {
            if p.aux[0].z_db_filename.is_none() && !is_script_file(z_full) {
                p.aux[0].z_db_filename = Some(z_full.to_owned());
            } else {
                read_stdin = false;
                p.stdin_is_interactive = false;
                az_cmd.push((z_full.to_owned(), i));
            }
            i += 1;
            continue;
        }
        let z = if z_full.as_bytes().get(1) == Some(&b'-') {
            &z_full[1..]
        } else {
            z_full
        };
        match z {
            "-" => n_opts_end = i,
            "-separator" | "-nullvalue" | "-newline" | "-cmd" => {
                i += 1;
                let _ = option_value!(i);
            }
            "-init" => {
                i += 1;
                z_init_file = Some(option_value!(i).to_owned());
            }
            "-batch" => {
                p.stdin_is_interactive = false;
                p.stdout_is_console = false;
                p.mode_change(MODE_BATCH);
            }
            "-screenwidth" => {
                i += 1;
                let n = atoi(option_value!(i));
                if n < 2 {
                    eprintln!("minimum --screenwidth is 2");
                    return 1;
                }
                p.stdout_tty_width = n;
            }
            "-heap" | "-mmap" | "-vfs" | "-nonce" | "-cmdline-edit" | "-threadsafe" => {
                i += 1;
                let v = option_value!(i);
                if z == "-vfs" && !matches!(v, "unix" | "unix-none" | "unix-dotfile" | "unix-excl")
                {
                    eprintln!("no such VFS: \"{v}\"");
                    return 1;
                }
                if z == "-cmdline-edit" {
                    let _ = boolean_value(v);
                }
            }
            "-pagecache" | "-lookaside" => {
                i += 1;
                let _ = option_value!(i);
                i += 1;
                let _ = option_value!(i);
            }
            "-deserialize" => p.open_deserialize = true,
            "-maxsize" if i + 1 < argc => i += 1,
            "-readonly" => {
                p.open_readonly = true;
                p.open_create = false;
                p.open_flags_set = true;
            }
            "-nofollow" => p.open_nofollow = true,
            "-noinit" => no_init = true,
            "-ifexists" => {
                p.open_create = false;
                p.open_flags_set = true;
            }
            "-bail" => p.bail_on_error = true,
            "-unsafe-testing" => p.shell_flgs |= SHFLG_TESTING_MODE,
            "-escape" if i + 1 < argc => i += 1,
            "-test-argv" => {
                for (kk, a) in argv.iter().enumerate() {
                    println!("argv[{kk}] = \"{a}\"");
                }
                return 0;
            }
            _ => {}
        }
        i += 1;
    }
    if p.aux[0].z_db_filename.is_none() {
        p.aux[0].z_db_filename = Some(":memory:".to_owned());
        warn_inmemory_db = argc == 1;
    }
    p.mode_default();

    // Open the database now if the file exists; otherwise the open is delayed so that a
    // mistyped name does not create an empty file.
    if p.db_filename()
        .is_some_and(|f| std::path::Path::new(f).exists())
        && !p.open_db(false)
    {
        return finish(&mut p, 1);
    }

    if !no_init {
        process_sqliterc(&mut p, z_init_file.as_deref());
        if let Some(c) = p.exit_code {
            return finish(&mut p, c);
        }
    }

    // Second pass: options that override the init file, and the -cmd commands.
    let mut i = 1usize;
    'second: while i < argc {
        let z_full = argv[i].as_str();
        if !z_full.starts_with('-') || i >= n_opts_end {
            i += 1;
            continue;
        }
        let z = if z_full.as_bytes().get(1) == Some(&b'-') {
            &z_full[1..]
        } else {
            z_full
        };
        match z {
            "-init" => i += 1,
            "-html" => p.mode_change(6),
            "-list" => p.mode_change(MODE_LIST),
            "-quote" => p.mode_change(17),
            "-line" => p.mode_change(11),
            "-column" => p.mode_change(MODE_COLUMN),
            "-json" => p.mode_change(10),
            "-markdown" => p.mode_change(13),
            "-table" => p.mode_change(19),
            "-psql" => p.mode_change(15),
            "-box" => p.mode_change(1),
            "-csv" => {
                p.mode_change(MODE_CSV);
                p.mode_set_limit(Some("off"));
            }
            "-escape" if i + 1 < argc => {
                i += 1;
                let z_esc = argv[i].as_str();
                match dotcmd::QRF_ESC_NAMES
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(z_esc))
                {
                    Some(k) => p.mode.spec.e_esc = k as u8,
                    None => {
                        eprintln!(
                            "unknown control character escape mode \"{z_esc}\" - choices: {}",
                            dotcmd::QRF_ESC_NAMES.join(" ")
                        );
                        return 1;
                    }
                }
            }
            "-append" | "-deserialize" | "-nofollow" | "-noinit" | "-exclusive" | "-bail"
            | "-batch" | "-utf8" | "-no-utf8" | "-no-rowid-in-view" | "-unsafe-testing" => {}
            "-maxsize" if i + 1 < argc => i += 1,
            "-readonly" => {
                p.open_readonly = true;
                p.open_create = false;
                p.open_flags_set = true;
            }
            "-ifexists" => {
                p.open_create = false;
                p.open_flags_set = true;
            }
            "-ascii" => p.mode_change(MODE_ASCII),
            "-tabs" => p.mode_change(20),
            "-separator" => {
                i += 1;
                p.mode.spec.z_column_sep = Some(option_value!(i).to_owned());
            }
            "-newline" => {
                i += 1;
                p.mode.spec.z_row_sep = Some(option_value!(i).to_owned());
            }
            "-nullvalue" => {
                i += 1;
                p.mode.spec.z_null = Some(option_value!(i).to_owned());
            }
            "-header" => {
                p.mode.spec.b_titles = QRF_ALWAYS;
                p.mode.m_flags |= MFLG_HDR;
            }
            "-noheader" => {
                p.mode.spec.b_titles = QRF_NO;
                p.mode.m_flags |= MFLG_HDR;
            }
            "-echo" => p.mode.m_flags |= MFLG_ECHO,
            "-eqp" => p.mode.auto_eqp = 1,
            "-eqpfull" => p.mode.auto_eqp = 3,
            "-stats" => p.m_stats = CLISTAT_ON,
            "-scanstats" => p.mode.scanstats_on = 1,
            "-backslash" => p.shell_flgs |= SHFLG_BACKSLASH,
            "-version" => {
                p.stdouts(&format!("{SQLITE_VERSION} {} (64-bit)\n", source_id()));
                return finish(&mut p, 0);
            }
            "-interactive" => p.stdin_is_interactive = true,
            "-screenwidth" | "-heap" | "-mmap" | "-memtrace" | "-pcachetrace" | "-vfs"
            | "-vfstrace" => i += 1,
            "-pagecache" | "-lookaside" | "-threadsafe" | "-nonce" | "-cmdline-edit" => i += 2,
            "-help" => {
                usage(&argv0, true);
                return finish(&mut p, 0);
            }
            "-cmd" => {
                if i == argc - 1 {
                    break 'second;
                }
                i += 1;
                let zc = argv[i].clone();
                if zc.starts_with('.') {
                    rc = dotcmd::do_meta_command(zc.as_bytes(), &mut p);
                    if let Some(c) = p.exit_code {
                        return finish(&mut p, c);
                    }
                    if rc != 0 && (p.bail_on_error || rc == 2) {
                        if rc == 2 {
                            rc = 0;
                        }
                        return finish(&mut p, rc);
                    }
                } else {
                    rc = run_one_sql_line(&mut p, zc.as_bytes(), Some("cmdline"), i as i64);
                    if let Some(c) = p.exit_code {
                        return finish(&mut p, c);
                    }
                    if p.bail_on_error {
                        return finish(&mut p, rc);
                    }
                }
            }
            "-safe" => {
                p.b_safe_mode = true;
                p.b_safe_mode_persist = true;
            }
            _ => {
                eprintln!("{argv0}: Error: unknown option: {z}");
                eprintln!("Use -help for a list of options.");
                return finish(&mut p, 1);
            }
        }
        i += 1;
    }

    if !read_stdin {
        let n_cmd = az_cmd.len();
        let mut k = 0usize;
        while k < n_cmd {
            let (cmd, argi) = az_cmd[k].clone();
            echo_group_input(&mut p, cmd.as_bytes());
            if is_script_file(&cmd) {
                let saved_in = p.input.take();
                let saved_lineno = p.lineno;
                let mut res = true;
                if let Some(r) = open_chr_source(&cmd) {
                    p.input = Some(r);
                    res = process_input(&mut p, &cmd);
                }
                p.input = saved_in;
                p.lineno = saved_lineno;
                if let Some(c) = p.exit_code {
                    return finish(&mut p, c);
                }
                if res {
                    k = n_cmd;
                }
            } else if cmd.starts_with('.') {
                p.z_in_file = Some("<cmdline>".to_owned());
                p.z_err_prefix = Some(format!("argv[{argi}]:"));
                rc = dotcmd::do_meta_command(cmd.as_bytes(), &mut p);
                p.z_err_prefix = None;
                if let Some(c) = p.exit_code {
                    return finish(&mut p, c);
                }
                if rc != 0 {
                    if rc == 2 {
                        rc = 0;
                    }
                    return finish(&mut p, rc);
                }
            } else {
                rc = run_one_sql_line(&mut p, cmd.as_bytes(), Some("cmdline"), argi as i64);
                if p.n_pop_mode != 0 {
                    p.mode_pop();
                    p.n_pop_mode = 0;
                }
                if let Some(c) = p.exit_code {
                    return finish(&mut p, c);
                }
                if rc != 0 {
                    return finish(&mut p, rc);
                }
            }
            if p.n_pop_output != 0 && !cmd.starts_with('.') {
                p.output_reset();
                p.n_pop_output = 0;
            }
            k += 1;
        }
    } else if p.stdin_is_interactive {
        p.stdouts(&format!(
            "SQLite version {SQLITE_VERSION} {:.19}\nEnter \".help\" for usage hints.\n",
            source_id()
        ));
        if warn_inmemory_db {
            p.stdouts("Connected to a ");
            if env::var_os("NO_COLOR").is_some() {
                p.stdouts("transient in-memory database");
            } else {
                p.stdouts("\x1b[1;36m\x1b[3mtransient in-memory database\x1b[0m");
            }
            p.stdouts(".\nUse \".open FILENAME\" to reopen on a persistent database.\n");
        }
        p.input = None;
        if !p.open_db(false) {
            return finish(&mut p, 1);
        }
        rc = i32::from(process_input(&mut p, "<stdin>"));
    } else {
        // Standard input is read through one buffered reader so that `.import <<END` and
        // the line reader see the same stream position.
        p.input = Some(Box::new(BufReader::with_capacity(64 * 1024, io::stdin())));
        rc = i32::from(process_input(&mut p, "<stdin>"));
    }
    if let Some(c) = p.exit_code {
        return finish(&mut p, c);
    }
    finish(&mut p, rc)
}

/// `sqlite3_sourceid()` as the C shell prints it: the engine's identifier without the
/// " (sqliters)" marker that the port appends.
pub fn source_id() -> &'static str {
    SQLITE_SOURCE_ID
        .strip_suffix(" (sqliters)")
        .unwrap_or(SQLITE_SOURCE_ID)
}

/// The tail of `main()`: closes everything and returns the exit code.
fn finish(p: &mut ShellState, rc: i32) -> i32 {
    p.output_reset();
    let _ = p.stdout.flush();
    if let Some(db) = p.db.take()
        && let Err(e) = db.try_close()
    {
        eprintln!("Error: sqlite3_close() returns {}: {}", 5, e);
    }
    rc
}

/// The `Value` an SQL literal argument denotes when it is used as text by a dot-command.
pub fn text_of(v: &Value) -> String {
    String::from_utf8_lossy(&qrf::value_text(v)).into_owned()
}
