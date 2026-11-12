//! Pure safe Rust TCL interpreter implementing the TCL 12-rule parser,
//! variable/array/upvar frames, procedures, and standard built-in commands.
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::expr::{eval_expr, parse_tcl_bool, parse_tcl_float, parse_tcl_int};
use crate::list::{
    collapse_backslash_newlines, format_list, is_tcl_list_space, parse_backslash_escape, parse_list,
};
use crate::regex::{Regex, glob_match};

#[derive(Debug, Clone)]
pub enum TclControl {
    Error(String),
    Return { code: i32, value: String },
    Break,
    Continue,
    FinishTest,
}

impl TclControl {
    pub fn err_msg(&self) -> String {
        match self {
            Self::Error(m) => m.clone(),
            Self::Return { value, .. } => value.clone(),
            Self::Break => "invoked \"break\" outside of a loop".to_string(),
            Self::Continue => "invoked \"continue\" outside of a loop".to_string(),
            Self::FinishTest => String::new(),
        }
    }
}

pub type TclResult<T> = Result<T, TclControl>;

#[derive(Debug, Clone)]
enum VarSlot {
    Scalar(String),
    Array(HashMap<String, String>),
    Link { frame_idx: usize, name: String },
}

#[derive(Debug, Clone, Default)]
struct CallFrame {
    vars: HashMap<String, VarSlot>,
    caller_frame: usize,
}

#[derive(Debug, Clone)]
pub struct TclProc {
    pub params: Vec<(String, Option<String>)>,
    pub has_varargs: bool,
    pub body: String,
}

enum OpenChannel {
    File {
        file: fs::File,
        binary: bool,
        eof: bool,
    },
    Pipe {
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: Option<BufReader<ChildStdout>>,
        binary: bool,
        eof: bool,
    },
}

impl OpenChannel {
    fn is_binary(&self) -> bool {
        match self {
            Self::File { binary, .. } | Self::Pipe { binary, .. } => *binary,
        }
    }

    fn set_binary(&mut self, val: bool) {
        match self {
            Self::File { binary, .. } | Self::Pipe { binary, .. } => *binary = val,
        }
    }

    fn is_eof(&self) -> bool {
        match self {
            Self::File { eof, .. } | Self::Pipe { eof, .. } => *eof,
        }
    }

    fn close_bounded(&mut self) {
        if let Self::Pipe {
            child,
            stdin,
            stdout,
            ..
        } = self
        {
            drop(stdin.take());
            drop(stdout.take());
            for _ in 0..500 {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for OpenChannel {
    fn drop(&mut self) {
        if let Self::Pipe {
            child,
            stdin,
            stdout,
            ..
        } = self
        {
            drop(stdin.take());
            drop(stdout.take());
            for _ in 0..50 {
                if let Ok(Some(_)) = child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

const DEFAULT_TCL_BUILTINS: &[(&str, &str)] = &[
    ("set", "set"),
    ("unset", "unset"),
    ("unset_nocomplain", "unset_nocomplain"),
    ("incr", "incr"),
    ("append", "append"),
    ("lappend", "lappend"),
    ("global", "global"),
    ("variable", "variable"),
    ("upvar", "upvar"),
    ("proc", "proc"),
    ("rename", "rename"),
    ("if", "if"),
    ("for", "for"),
    ("foreach", "foreach"),
    ("while", "while"),
    ("switch", "switch"),
    ("break", "break"),
    ("continue", "continue"),
    ("return", "return"),
    ("error", "error"),
    ("catch", "catch"),
    ("eval", "eval"),
    ("uplevel", "uplevel"),
    ("subst", "subst"),
    ("expr", "expr"),
    ("list", "list"),
    ("concat", "concat"),
    ("split", "split"),
    ("join", "join"),
    ("llength", "llength"),
    ("lindex", "lindex"),
    ("lrange", "lrange"),
    ("linsert", "linsert"),
    ("lreplace", "lreplace"),
    ("lset", "lset"),
    ("lsearch", "lsearch"),
    ("lsort", "lsort"),
    ("string", "string"),
    ("array", "array"),
    ("info", "info"),
    ("format", "format"),
    ("scan", "scan"),
    ("regexp", "regexp"),
    ("regsub", "regsub"),
    ("binary", "binary"),
    ("file", "file"),
    ("glob", "glob"),
    ("cd", "cd"),
    ("pwd", "pwd"),
    ("open", "open"),
    ("puts", "puts"),
    ("puts_override", "puts_override"),
    ("gets", "gets"),
    ("read", "read"),
    ("close", "close"),
    ("flush", "flush"),
    ("fconfigure", "fconfigure"),
    ("fileevent", "fileevent"),
    ("eof", "eof"),
    ("seek", "seek"),
    ("tell", "tell"),
    ("time", "time"),
    ("clock", "clock"),
    ("namespace", "namespace"),
    ("encoding", "encoding"),
    ("after", "after"),
    ("pid", "pid"),
    ("exec", "exec"),
    ("exit", "exit"),
];

pub struct TclInterp {
    frames: Vec<CallFrame>,
    current_frame: usize,
    pub procs: HashMap<String, TclProc>,
    pub builtins: HashMap<String, &'static str>,
    pub cwd: PathBuf,
    channels: HashMap<String, OpenChannel>,
    next_chan_id: usize,
    pub host: Option<crate::harness::SharedHost>,
    pub script_path: String,
    pub subprocess_mode: bool,
    pub last_result_is_bytearray: bool,
    stdin_eof: bool,
    bg_children: Vec<Child>,
}

impl Drop for TclInterp {
    fn drop(&mut self) {
        self.channels.clear();
        self.kill_bg_children();
    }
}

impl TclInterp {
    pub(crate) fn new_dummy() -> Self {
        Self {
            frames: Vec::new(),
            current_frame: 0,
            procs: HashMap::new(),
            builtins: HashMap::new(),
            cwd: PathBuf::new(),
            channels: HashMap::new(),
            next_chan_id: 1,
            host: None,
            script_path: String::new(),
            subprocess_mode: false,
            last_result_is_bytearray: false,
            stdin_eof: false,
            bg_children: Vec::new(),
        }
    }

    pub(crate) fn kill_bg_children(&mut self) {
        for mut child in self.bg_children.drain(..) {
            if let Ok(None) = child.try_wait() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    pub fn new(cwd: PathBuf, host: Option<crate::harness::SharedHost>) -> Self {
        let mut global = CallFrame::default();
        let mut platform = HashMap::new();
        platform.insert("platform".to_string(), "unix".to_string());
        platform.insert("os".to_string(), "Linux".to_string());
        platform.insert("byteOrder".to_string(), "littleEndian".to_string());
        platform.insert("wordSize".to_string(), "8".to_string());
        platform.insert("pointerSize".to_string(), "8".to_string());
        platform.insert("machine".to_string(), "x86_64".to_string());
        global
            .vars
            .insert("tcl_platform".to_string(), VarSlot::Array(platform));
        global
            .vars
            .insert("env".to_string(), VarSlot::Array(HashMap::new()));
        global
            .vars
            .insert("errorInfo".to_string(), VarSlot::Scalar(String::new()));
        global
            .vars
            .insert("errorCode".to_string(), VarSlot::Scalar("NONE".to_string()));

        let mut builtins = HashMap::with_capacity(DEFAULT_TCL_BUILTINS.len());
        for &(name, target) in DEFAULT_TCL_BUILTINS {
            builtins.insert(name.to_string(), target);
        }

        Self {
            frames: vec![global],
            current_frame: 0,
            procs: HashMap::new(),
            builtins,
            cwd,
            channels: HashMap::new(),
            next_chan_id: 1,
            host,
            script_path: String::new(),
            subprocess_mode: false,
            last_result_is_bytearray: false,
            stdin_eof: false,
            bg_children: Vec::new(),
        }
    }

    pub(crate) fn remove_non_host_cmd(&mut self, name: &str) {
        self.procs.remove(name);
        self.builtins.remove(name);
    }

    pub fn resolve_path(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    pub(crate) fn frame_idx_internal(&self) -> usize {
        self.current_frame
    }

    pub(crate) fn set_frame_idx_internal(&mut self, idx: usize) {
        self.current_frame = idx;
    }

    pub(crate) fn record_script_error(&mut self, msg: String) {
        if let Some(ref host_rc) = self.host {
            host_rc.borrow_mut().report.script_error = Some(msg);
        }
    }

    // -------------------------------------------------------------------------
    // Variable & Array resolution
    // -------------------------------------------------------------------------

    fn parse_var_ref<'a>(&self, raw: &'a str) -> (bool, &'a str, Option<&'a str>) {
        let (is_global, rest) = if let Some(stripped) = raw.strip_prefix("::") {
            (true, stripped)
        } else {
            (false, raw)
        };
        if let Some(open_paren) = rest.find('(')
            && rest.ends_with(')')
        {
            let base = &rest[..open_paren];
            let key = &rest[open_paren + 1..rest.len() - 1];
            return (is_global, base, Some(key));
        }
        (is_global, rest, None)
    }

    fn resolve_target(&self, mut frame_idx: usize, mut name: String) -> (usize, String) {
        if let Some(stripped) = name.strip_prefix("::") {
            frame_idx = 0;
            name = stripped.to_string();
        }
        for _ in 0..16 {
            if let Some(VarSlot::Link {
                frame_idx: tf,
                name: tn,
            }) = self.frames.get(frame_idx).and_then(|f| f.vars.get(&name))
            {
                frame_idx = *tf;
                name = tn.clone();
            } else {
                break;
            }
        }
        (frame_idx, name)
    }

    pub fn get_var(&self, raw_name: &str) -> Result<String, String> {
        let (is_global, base, idx) = self.parse_var_ref(raw_name);
        let start_frame = if is_global { 0 } else { self.current_frame };
        let (fidx, bname) = self.resolve_target(start_frame, base.to_string());
        if fidx == 0
            && let Some(ref host_rc) = self.host
        {
            if bname == "sqlite_pending_byte" {
                return match idx {
                    None => Ok(crate::harness::get_pending_byte_str(host_rc)),
                    Some(_) => Err(format!("can't read \"{raw_name}\": variable isn't array")),
                };
            }
            if let Some(counter_val) = crate::harness::get_vfs_counter_str(host_rc, &bname) {
                return match idx {
                    None => Ok(counter_val),
                    Some(_) => Err(format!("can't read \"{raw_name}\": variable isn't array")),
                };
            }
        }
        let slot = self.frames.get(fidx).and_then(|f| f.vars.get(&bname));
        match (slot, idx) {
            (Some(VarSlot::Scalar(v)), None) => Ok(v.clone()),
            (Some(VarSlot::Array(_)), None) => {
                Err(format!("can't read \"{raw_name}\": variable is array"))
            }
            (Some(VarSlot::Array(map)), Some(k)) => map
                .get(k)
                .cloned()
                .ok_or_else(|| format!("can't read \"{raw_name}\": no such element in array")),
            (Some(VarSlot::Scalar(_)), Some(_)) => {
                Err(format!("can't read \"{raw_name}\": variable isn't array"))
            }
            _ => Err(format!("can't read \"{raw_name}\": no such variable")),
        }
    }

    pub fn set_var(&mut self, raw_name: &str, val: String) -> Result<String, String> {
        let (is_global, base, idx) = self.parse_var_ref(raw_name);
        let start_frame = if is_global { 0 } else { self.current_frame };
        let (fidx, bname) = self.resolve_target(start_frame, base.to_string());
        if fidx == 0
            && let Some(host_rc) = self.host.clone()
        {
            if bname == "sqlite_pending_byte" {
                return match idx {
                    None => {
                        crate::harness::set_pending_byte_str(&host_rc, &val)?;
                        Ok(val)
                    }
                    Some(_) => Err(format!("can't set \"{raw_name}\": variable isn't array")),
                };
            }
            if crate::harness::is_vfs_counter_var(&bname) {
                return match idx {
                    None => {
                        crate::harness::set_vfs_counter_str(&host_rc, &bname, &val)?;
                        Ok(val)
                    }
                    Some(_) => Err(format!("can't set \"{raw_name}\": variable isn't array")),
                };
            }
        }
        match idx {
            None => {
                if matches!(self.frames[fidx].vars.get(&bname), Some(VarSlot::Array(_))) {
                    return Err(format!("can't set \"{raw_name}\": variable is array"));
                }
                self.frames[fidx]
                    .vars
                    .insert(bname, VarSlot::Scalar(val.clone()));
                Ok(val)
            }
            Some(k) => {
                let entry = self.frames[fidx]
                    .vars
                    .entry(bname)
                    .or_insert_with(|| VarSlot::Array(HashMap::new()));
                match entry {
                    VarSlot::Array(map) => {
                        map.insert(k.to_string(), val.clone());
                        Ok(val)
                    }
                    _ => Err(format!("can't set \"{raw_name}\": variable isn't array")),
                }
            }
        }
    }

    pub fn unset_var(&mut self, raw_name: &str, nocomplain: bool) -> Result<(), String> {
        let (is_global, base, idx) = self.parse_var_ref(raw_name);
        let start_frame = if is_global { 0 } else { self.current_frame };
        let (fidx, bname) = self.resolve_target(start_frame, base.to_string());
        if fidx == 0 && bname == "sqlite_pending_byte" && self.host.is_some() {
            return match idx {
                None => Err(format!(
                    "can't unset \"{raw_name}\": linked test-domain variable cannot be unset"
                )),
                Some(_) if nocomplain => Ok(()),
                Some(_) => Err(format!("can't unset \"{raw_name}\": variable isn't array")),
            };
        }
        match idx {
            None => {
                if self.frames[fidx].vars.remove(&bname).is_none() && !nocomplain {
                    return Err(format!("can't unset \"{raw_name}\": no such variable"));
                }
                Ok(())
            }
            Some(k) => {
                if let Some(VarSlot::Array(map)) = self.frames[fidx].vars.get_mut(&bname) {
                    if map.remove(k).is_none() && !nocomplain {
                        return Err(format!(
                            "can't unset \"{raw_name}\": no such element in array"
                        ));
                    }
                    Ok(())
                } else if nocomplain {
                    Ok(())
                } else {
                    Err(format!("can't unset \"{raw_name}\": no such variable"))
                }
            }
        }
    }

    pub fn var_exists(&self, raw_name: &str) -> bool {
        let (is_global, base, idx) = self.parse_var_ref(raw_name);
        let start_frame = if is_global { 0 } else { self.current_frame };
        let (fidx, bname) = self.resolve_target(start_frame, base.to_string());
        if fidx == 0 && bname == "sqlite_pending_byte" && self.host.is_some() {
            return idx.is_none();
        }
        match (self.frames.get(fidx).and_then(|f| f.vars.get(&bname)), idx) {
            (Some(VarSlot::Scalar(_)), None) => true,
            (Some(VarSlot::Array(_)), None) => true,
            (Some(VarSlot::Array(map)), Some(k)) => map.contains_key(k),
            _ => false,
        }
    }

    pub fn set_array_entry(&mut self, arr_name: &str, key: &str, val: String) {
        let (is_global, base, _) = self.parse_var_ref(arr_name);
        let start_frame = if is_global { 0 } else { self.current_frame };
        let (fidx, bname) = self.resolve_target(start_frame, base.to_string());
        let slot = self.frames[fidx]
            .vars
            .entry(bname)
            .or_insert_with(|| VarSlot::Array(HashMap::new()));
        if !matches!(slot, VarSlot::Array(_)) {
            *slot = VarSlot::Array(HashMap::new());
        }
        if let VarSlot::Array(map) = slot {
            map.insert(key.to_string(), val);
        }
    }

    // -------------------------------------------------------------------------
    // TCL 12-rule Script Parser & Evaluator
    // -------------------------------------------------------------------------

    pub fn eval(&mut self, script: &str) -> TclResult<String> {
        let bytes = script.as_bytes();
        let len = bytes.len();
        let mut i = 0usize;
        let mut last_result = String::new();

        while i < len {
            // Skip command separators and whitespace
            while i < len {
                let b = bytes[i];
                if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' || b == b';' {
                    i += 1;
                } else if b == b'\\' && i + 1 < len && bytes[i + 1] == b'\n' {
                    i += 2;
                } else {
                    break;
                }
            }
            if i >= len {
                break;
            }

            // Comment at start of command
            if bytes[i] == b'#' {
                while i < len {
                    if bytes[i] == b'\\' && i + 1 < len && bytes[i + 1] == b'\n' {
                        i += 2;
                    } else if bytes[i] == b'\n' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }

            // Parse words of a single command
            let mut words: Vec<String> = Vec::new();
            while i < len {
                // Skip horizontal whitespace (and backslash-newline)
                while i < len {
                    if bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\r' {
                        i += 1;
                    } else if bytes[i] == b'\\' && i + 1 < len && bytes[i + 1] == b'\n' {
                        i += 2;
                    } else {
                        break;
                    }
                }
                if i >= len || bytes[i] == b'\n' || bytes[i] == b';' {
                    if i < len {
                        i += 1;
                    }
                    break;
                }

                // Check for `{*}` argument expansion
                let mut expand = false;
                if i + 3 < len
                    && &bytes[i..i + 3] == b"{*}"
                    && !matches!(bytes[i + 3], b' ' | b'\t' | b'\r' | b'\n' | b';')
                {
                    expand = true;
                    i += 3;
                }

                let (word, next_i) = self.parse_word(script, i)?;
                i = next_i;
                if expand {
                    let items = parse_list(&word).map_err(TclControl::Error)?;
                    words.extend(items);
                } else {
                    words.push(word);
                }
            }

            if !words.is_empty() {
                last_result = self.dispatch_command(&words)?;
            }
        }

        Ok(last_result)
    }

    fn parse_word(&mut self, src: &str, mut i: usize) -> TclResult<(String, usize)> {
        let bytes = src.as_bytes();
        let len = bytes.len();

        if bytes[i] == b'{' {
            i += 1;
            let start = i;
            let mut depth = 1usize;
            let mut has_bs_nl = false;
            while i < len && depth > 0 {
                if bytes[i] == b'\\' && i + 1 < len {
                    if bytes[i + 1] == b'\n' {
                        has_bs_nl = true;
                    }
                    i += 2;
                    continue;
                }
                if bytes[i] == b'{' {
                    depth += 1;
                } else if bytes[i] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                i += 1;
            }
            if depth != 0 {
                return Err(TclControl::Error("missing close-brace".to_string()));
            }
            let raw = &src[start..i];
            i += 1;
            let val = if has_bs_nl {
                collapse_backslash_newlines(raw)
            } else {
                raw.to_string()
            };
            return Ok((val, i));
        }

        if bytes[i] == b'"' {
            i += 1;
            let mut out = String::new();
            let mut closed = false;
            while i < len {
                match bytes[i] {
                    b'"' => {
                        closed = true;
                        i += 1;
                        break;
                    }
                    b'\\' => {
                        let (esc, nxt) = parse_backslash_escape(src, i);
                        out.push_str(&esc);
                        i = nxt;
                    }
                    b'$' => {
                        let (val, nxt) = self.subst_dollar(src, i, true)?;
                        out.push_str(&val);
                        i = nxt;
                    }
                    b'[' => {
                        let (val, nxt) = self.subst_bracket(src, i, true)?;
                        out.push_str(&val);
                        i = nxt;
                    }
                    _ => {
                        let ch = src[i..].chars().next().unwrap();
                        out.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            if !closed {
                return Err(TclControl::Error("missing \"".to_string()));
            }
            return Ok((out, i));
        }

        let mut out = String::new();
        while i < len {
            let b = bytes[i];
            if b == b' ' || b == b'\t' || b == b'\r' || b == b'\n' || b == b';' {
                break;
            }
            match b {
                b'\\' => {
                    if i + 1 < len && bytes[i + 1] == b'\n' {
                        break;
                    }
                    let (esc, nxt) = parse_backslash_escape(src, i);
                    out.push_str(&esc);
                    i = nxt;
                }
                b'$' => {
                    let (val, nxt) = self.subst_dollar(src, i, true)?;
                    out.push_str(&val);
                    i = nxt;
                }
                b'[' => {
                    let (val, nxt) = self.subst_bracket(src, i, true)?;
                    out.push_str(&val);
                    i = nxt;
                }
                _ => {
                    let ch = src[i..].chars().next().unwrap();
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        Ok((out, i))
    }

    pub fn subst_dollar(
        &mut self,
        src: &str,
        idx: usize,
        do_eval: bool,
    ) -> TclResult<(String, usize)> {
        // src.as_bytes()[idx] == b'$'
        let bytes = src.as_bytes();
        let len = bytes.len();
        let mut i = idx + 1;
        if i >= len {
            return Ok(("$".to_string(), i));
        }

        // `${varname}`
        if bytes[i] == b'{' {
            i += 1;
            let start = i;
            while i < len && bytes[i] != b'}' {
                i += 1;
            }
            if i >= len {
                return Err(TclControl::Error(
                    "missing close-brace for variable name".to_string(),
                ));
            }
            let name = &src[start..i];
            i += 1;
            if !do_eval {
                return Ok((String::new(), i));
            }
            let val = self.get_var(name).map_err(TclControl::Error)?;
            return Ok((val, i));
        }

        // `$varname` or `$::varname` or `$arr(index)`
        let name_start = i;
        while i < len
            && (bytes[i].is_ascii_alphanumeric()
                || bytes[i] == b'_'
                || (bytes[i] == b':' && i + 1 < len && bytes[i + 1] == b':'))
        {
            if bytes[i] == b':' {
                i += 2;
            } else {
                i += 1;
            }
        }

        if i == name_start {
            // Bare `$` not followed by varname
            return Ok(("$".to_string(), idx + 1));
        }

        let base_name = &src[name_start..i];

        // Array index `$arr(...)`
        if i < len && bytes[i] == b'(' {
            i += 1;
            let mut key = String::new();
            let mut closed = false;
            while i < len {
                match bytes[i] {
                    b')' => {
                        closed = true;
                        i += 1;
                        break;
                    }
                    b'\\' => {
                        let (esc, nxt) = parse_backslash_escape(src, i);
                        key.push_str(&esc);
                        i = nxt;
                    }
                    b'$' => {
                        let (v, nxt) = self.subst_dollar(src, i, do_eval)?;
                        key.push_str(&v);
                        i = nxt;
                    }
                    b'[' => {
                        let (v, nxt) = self.subst_bracket(src, i, do_eval)?;
                        key.push_str(&v);
                        i = nxt;
                    }
                    _ => {
                        let ch = src[i..].chars().next().unwrap();
                        key.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            if !closed {
                return Err(TclControl::Error(
                    "missing ')' in array variable".to_string(),
                ));
            }
            if !do_eval {
                return Ok((String::new(), i));
            }
            let full = format!("{base_name}({key})");
            let val = self.get_var(&full).map_err(TclControl::Error)?;
            return Ok((val, i));
        }

        if !do_eval {
            return Ok((String::new(), i));
        }
        let val = self.get_var(base_name).map_err(TclControl::Error)?;
        Ok((val, i))
    }

    pub fn subst_bracket(
        &mut self,
        src: &str,
        idx: usize,
        do_eval: bool,
    ) -> TclResult<(String, usize)> {
        // Find matching `]` respecting TCL quoting/braces/nested brackets
        let end = find_matching_bracket(src, idx).map_err(TclControl::Error)?;
        let inner = &src[idx + 1..end];
        if !do_eval {
            return Ok((String::new(), end + 1));
        }
        let res = self.eval(inner)?;
        Ok((res, end + 1))
    }

    pub fn subst_string(
        &mut self,
        src: &str,
        do_backslashes: bool,
        do_vars: bool,
        do_cmds: bool,
    ) -> TclResult<String> {
        let bytes = src.as_bytes();
        let mut i = 0;
        let mut out = String::new();
        while i < bytes.len() {
            if do_backslashes && bytes[i] == b'\\' {
                let (esc, nxt) = parse_backslash_escape(src, i);
                out.push_str(&esc);
                i = nxt;
            } else if do_vars && bytes[i] == b'$' {
                let (val, nxt) = self.subst_dollar(src, i, true)?;
                out.push_str(&val);
                i = nxt;
            } else if do_cmds && bytes[i] == b'[' {
                let (val, nxt) = self.subst_bracket(src, i, true)?;
                out.push_str(&val);
                i = nxt;
            } else {
                let ch = src[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
        Ok(out)
    }

    // -------------------------------------------------------------------------
    // Command Dispatcher
    // -------------------------------------------------------------------------

    pub fn dispatch_command(&mut self, words: &[String]) -> TclResult<String> {
        let raw_cmd = words[0].as_str();
        let cmd = raw_cmd.strip_prefix("::").unwrap_or(raw_cmd);
        let args = &words[1..];
        if cmd != "return" {
            self.last_result_is_bytearray = false;
        }

        // 1. User/tester.tcl defined procs take precedence over built-ins when overridden
        //    (e.g., `proc puts` in tester.tcl!)
        if let Some(proc_def) = self.procs.get(cmd).cloned() {
            return self.call_proc(cmd, &proc_def, args);
        }

        // 2. Host commands (`sqlite3`, `db`, `do_test`, `hexio_*`, etc.)
        if let Some(host_rc) = self.host.clone()
            && let Some(res) = crate::harness::invoke_host_cmd(&host_rc, self, cmd, args)
        {
            return res;
        }

        // 3. Built-in TCL commands (resolved through command-identity table)
        let Some(target) = self.builtins.get(cmd).copied() else {
            return Err(TclControl::Error(format!(
                "invalid command name \"{}\"",
                words[0]
            )));
        };

        match target {
            "set" => self.cmd_set(args),
            "unset" | "unset_nocomplain" => self.cmd_unset(target == "unset_nocomplain", args),
            "incr" => self.cmd_incr(args),
            "append" => self.cmd_append(args),
            "lappend" => self.cmd_lappend(args),
            "global" | "variable" => self.cmd_global(args),
            "upvar" => self.cmd_upvar(args),
            "proc" => self.cmd_proc(args),
            "rename" => self.cmd_rename(args),
            "if" => self.cmd_if(args),
            "for" => self.cmd_for(args),
            "foreach" => self.cmd_foreach(args),
            "while" => self.cmd_while(args),
            "switch" => self.cmd_switch(args),
            "break" => Err(TclControl::Break),
            "continue" => Err(TclControl::Continue),
            "return" => self.cmd_return(args),
            "error" => {
                let msg = args.first().cloned().unwrap_or_default();
                Err(TclControl::Error(msg))
            }
            "catch" => self.cmd_catch(args),
            "eval" => {
                let joined = args.join(" ");
                self.eval(&joined)
            }
            "uplevel" => self.cmd_uplevel(args),
            "subst" => self.cmd_subst(args),
            "expr" => {
                let expr_str = args.join(" ");
                let val = eval_expr(self, &expr_str)?;
                Ok(val.to_tcl_string())
            }
            "list" => Ok(format_list(args)),
            "concat" => {
                let mut parts = Vec::new();
                for a in args {
                    let t = a.trim();
                    if !t.is_empty() {
                        parts.push(t);
                    }
                }
                Ok(parts.join(" "))
            }
            "split" => self.cmd_split(args),
            "join" => self.cmd_join(args),
            "llength" => {
                let items = parse_list(args.first().map_or("", String::as_str))
                    .map_err(TclControl::Error)?;
                Ok(items.len().to_string())
            }
            "lindex" => self.cmd_lindex(args),
            "lrange" => self.cmd_lrange(args),
            "linsert" => self.cmd_linsert(args),
            "lreplace" => self.cmd_lreplace(args),
            "lset" => self.cmd_lset(args),
            "lsearch" => self.cmd_lsearch(args),
            "lsort" => self.cmd_lsort(args),
            "string" => self.cmd_string(args),
            "array" => self.cmd_array(args),
            "info" => self.cmd_info(args),
            "format" => self.cmd_format(args),
            "scan" => self.cmd_scan(args),
            "regexp" => self.cmd_regexp(args),
            "regsub" => self.cmd_regsub(args),
            "binary" => self.cmd_binary(args),
            "file" => self.cmd_file(args),
            "glob" => self.cmd_glob(args),
            "cd" => {
                if let Some(dir) = args.first() {
                    let p = self.resolve_path(dir);
                    self.cwd = p;
                }
                Ok(String::new())
            }
            "pwd" => Ok(self.cwd.to_string_lossy().into_owned()),
            "open" => self.cmd_open(args),
            "puts" | "puts_override" => self.cmd_puts(args),
            "gets" => self.cmd_gets(args),
            "read" => self.cmd_read(args),
            "close" => self.cmd_close(args),
            "flush" => self.cmd_flush(args),
            "fconfigure" => self.cmd_fconfigure(args),
            "fileevent" => Ok(String::new()),
            "eof" => self.cmd_eof(args),
            "seek" => self.cmd_seek(args),
            "tell" => self.cmd_tell(args),
            "time" => self.cmd_time(args),
            "clock" => self.cmd_clock(args),
            "namespace" => self.cmd_namespace(args),
            "encoding" => Ok("utf-8".to_string()),
            "after" => self.cmd_after(args),
            "pid" => Ok(std::process::id().to_string()),
            "exec" => self.cmd_exec(args),
            "exit" => self.cmd_exit(args),
            _ => Err(TclControl::Error(format!(
                "invalid command name \"{}\"",
                words[0]
            ))),
        }
    }

    fn call_proc(&mut self, _name: &str, proc_def: &TclProc, args: &[String]) -> TclResult<String> {
        let mut frame = CallFrame::default();
        let min_args = proc_def
            .params
            .iter()
            .filter(|(_, def)| def.is_none())
            .count();
        if args.len() < min_args || (!proc_def.has_varargs && args.len() > proc_def.params.len()) {
            return Err(TclControl::Error(format!(
                "wrong # args: should be \"{_name} ...\""
            )));
        }

        for (i, (pname, def_val)) in proc_def.params.iter().enumerate() {
            let val = if let Some(a) = args.get(i) {
                a.clone()
            } else {
                def_val.clone().unwrap_or_default()
            };
            frame.vars.insert(pname.clone(), VarSlot::Scalar(val));
        }

        if proc_def.has_varargs {
            let rest = if args.len() > proc_def.params.len() {
                &args[proc_def.params.len()..]
            } else {
                &[]
            };
            frame
                .vars
                .insert("args".to_string(), VarSlot::Scalar(format_list(rest)));
        }

        let prev_frame = self.current_frame;
        frame.caller_frame = prev_frame;
        let new_idx = self.frames.len();
        self.frames.push(frame);
        self.current_frame = new_idx;

        let res = self.eval(&proc_def.body);

        self.frames.truncate(new_idx);
        self.current_frame = prev_frame;

        match res {
            Ok(v) => Ok(v),
            Err(TclControl::Return { code, value }) => match code {
                0 | 2 => Ok(value),
                1 => Err(TclControl::Error(value)),
                3 => Err(TclControl::Break),
                4 => Err(TclControl::Continue),
                _ => Ok(value),
            },
            Err(other) => Err(other),
        }
    }

    // -------------------------------------------------------------------------
    // Individual Built-in Commands
    // -------------------------------------------------------------------------

    fn cmd_set(&mut self, args: &[String]) -> TclResult<String> {
        match args.len() {
            1 => self.get_var(&args[0]).map_err(TclControl::Error),
            2 => self
                .set_var(&args[0], args[1].clone())
                .map_err(TclControl::Error),
            _ => Err(TclControl::Error(
                "wrong # args: should be \"set varName ?newValue?\"".to_string(),
            )),
        }
    }

    fn cmd_unset(&mut self, mut nocomplain: bool, args: &[String]) -> TclResult<String> {
        let mut idx = 0;
        while idx < args.len() {
            if args[idx] == "-nocomplain" {
                nocomplain = true;
                idx += 1;
            } else if args[idx] == "--" {
                idx += 1;
                break;
            } else {
                break;
            }
        }
        for name in &args[idx..] {
            self.unset_var(name, nocomplain)
                .map_err(TclControl::Error)?;
        }
        Ok(String::new())
    }

    fn cmd_incr(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() || args.len() > 2 {
            return Err(TclControl::Error(
                "wrong # args: should be \"incr varName ?increment?\"".to_string(),
            ));
        }
        let delta = if args.len() == 2 {
            parse_tcl_int(args[1].trim()).ok_or_else(|| {
                TclControl::Error(format!("expected integer but got \"{}\"", args[1]))
            })?
        } else {
            1
        };
        let cur = match self.get_var(&args[0]) {
            Ok(s) => parse_tcl_int(s.trim())
                .ok_or_else(|| TclControl::Error(format!("expected integer but got \"{s}\"")))?,
            Err(_) => 0,
        };
        let next = cur.wrapping_add(delta).to_string();
        self.set_var(&args[0], next).map_err(TclControl::Error)
    }

    fn cmd_append(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error(
                "wrong # args: should be \"append varName ?value ...?\"".to_string(),
            ));
        }
        let mut cur = self.get_var(&args[0]).unwrap_or_default();
        for v in &args[1..] {
            cur.push_str(v);
        }
        self.set_var(&args[0], cur).map_err(TclControl::Error)
    }

    fn cmd_lappend(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error(
                "wrong # args: should be \"lappend varName ?value ...?\"".to_string(),
            ));
        }
        let mut items = match self.get_var(&args[0]) {
            Ok(s) => parse_list(&s).map_err(TclControl::Error)?,
            Err(_) => Vec::new(),
        };
        for v in &args[1..] {
            items.push(v.clone());
        }
        let formatted = format_list(&items);
        self.set_var(&args[0], formatted).map_err(TclControl::Error)
    }

    fn cmd_global(&mut self, args: &[String]) -> TclResult<String> {
        if self.current_frame == 0 {
            return Ok(String::new());
        }
        for raw in args {
            let clean = raw.strip_prefix("::").unwrap_or(raw);
            let local_name = clean.rsplit("::").next().unwrap_or(clean).to_string();
            self.frames[self.current_frame].vars.insert(
                local_name,
                VarSlot::Link {
                    frame_idx: 0,
                    name: clean.to_string(),
                },
            );
        }
        Ok(String::new())
    }

    fn cmd_upvar(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 2 {
            return Err(TclControl::Error("wrong # args to upvar".to_string()));
        }
        let (target_frame, rest) = if args.len() % 2 == 1 {
            let lvl = self.resolve_level(&args[0])?;
            (lvl, &args[1..])
        } else {
            let caller = if self.current_frame == 0 {
                0
            } else {
                self.frames[self.current_frame].caller_frame
            };
            (caller, args)
        };

        for pair in rest.as_chunks::<2>().0 {
            let other = &pair[0];
            let my_var = &pair[1];
            let (tf, tname) = self.resolve_target(target_frame, other.clone());
            if tf != self.current_frame || tname != *my_var {
                self.frames[self.current_frame].vars.insert(
                    my_var.clone(),
                    VarSlot::Link {
                        frame_idx: tf,
                        name: tname,
                    },
                );
            }
        }
        Ok(String::new())
    }

    fn resolve_level(&self, spec: &str) -> TclResult<usize> {
        if let Some(abs) = spec.strip_prefix('#') {
            let idx: usize = abs
                .parse()
                .map_err(|_| TclControl::Error(format!("bad level \"{spec}\"")))?;
            if idx == 0 {
                return Ok(0);
            }
            let mut chain = Vec::new();
            let mut cur = self.current_frame;
            while cur > 0 {
                chain.push(cur);
                cur = self.frames[cur].caller_frame;
            }
            chain.push(0);
            chain.reverse();
            chain
                .get(idx)
                .copied()
                .ok_or_else(|| TclControl::Error(format!("bad level \"{spec}\"")))
        } else {
            let rel: usize = spec
                .parse()
                .map_err(|_| TclControl::Error(format!("bad level \"{spec}\"")))?;
            let mut cur = self.current_frame;
            for _ in 0..rel {
                if cur == 0 {
                    return Err(TclControl::Error(format!("bad level \"{spec}\"")));
                }
                cur = self.frames[cur].caller_frame;
            }
            Ok(cur)
        }
    }

    fn cmd_proc(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() != 3 {
            return Err(TclControl::Error(
                "wrong # args: should be \"proc name args body\"".to_string(),
            ));
        }
        let name = args[0].strip_prefix("::").unwrap_or(&args[0]).to_string();
        let raw_params = parse_list(&args[1]).map_err(TclControl::Error)?;
        let mut params = Vec::new();
        let mut has_varargs = false;
        for (i, p) in raw_params.iter().enumerate() {
            if i + 1 == raw_params.len() && p == "args" {
                has_varargs = true;
                break;
            }
            let parts = parse_list(p).map_err(TclControl::Error)?;
            match parts.len() {
                1 => params.push((parts[0].clone(), None)),
                2 => params.push((parts[0].clone(), Some(parts[1].clone()))),
                _ => {
                    return Err(TclControl::Error(format!(
                        "argument with too many fields: \"{p}\""
                    )));
                }
            }
        }
        self.builtins.remove(&name);
        if let Some(host_rc) = self.host.clone() {
            crate::harness::delete_host_cmd_if_exists(&host_rc, &name);
        }
        self.procs.insert(
            name,
            TclProc {
                params,
                has_varargs,
                body: args[2].clone(),
            },
        );
        Ok(String::new())
    }

    fn cmd_rename(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() != 2 {
            return Err(TclControl::Error("wrong # args to rename".to_string()));
        }
        let old = args[0].strip_prefix("::").unwrap_or(&args[0]);
        let new = args[1].strip_prefix("::").unwrap_or(&args[1]);

        let in_procs = self.procs.contains_key(old);
        let in_host = self
            .host
            .as_ref()
            .is_some_and(|h| crate::harness::has_host_cmd(h, old));
        let in_builtins = self.builtins.contains_key(old);

        if !in_procs && !in_host && !in_builtins {
            return Err(TclControl::Error(format!(
                "can't rename \"{}\": command doesn't exist",
                args[0]
            )));
        }

        if !new.is_empty() {
            let new_exists = self.procs.contains_key(new)
                || self.builtins.contains_key(new)
                || self
                    .host
                    .as_ref()
                    .is_some_and(|h| crate::harness::has_host_cmd(h, new));
            if new_exists {
                return Err(TclControl::Error(format!(
                    "can't rename to \"{}\": command already exists",
                    args[1]
                )));
            }
        }

        if in_procs {
            if let Some(p) = self.procs.remove(old)
                && !new.is_empty()
            {
                self.procs.insert(new.to_string(), p);
            }
        } else if in_host {
            if let Some(host_rc) = self.host.clone() {
                crate::harness::rename_or_delete_host_cmd(&host_rc, old, new)?;
            }
        } else if let Some(target) = self.builtins.remove(old)
            && !new.is_empty()
        {
            self.builtins.insert(new.to_string(), target);
        }

        Ok(String::new())
    }

    fn cmd_if(&mut self, args: &[String]) -> TclResult<String> {
        let mut i = 0;
        while i < args.len() {
            let cond_str = &args[i];
            i += 1;
            if i < args.len() && args[i] == "then" {
                i += 1;
            }
            if i >= args.len() {
                return Err(TclControl::Error(
                    "wrong # args: no script following if condition".to_string(),
                ));
            }
            let body = &args[i];
            i += 1;

            let cond_val = eval_expr(self, cond_str)?;
            if cond_val.as_bool().map_err(TclControl::Error)? {
                return self.eval(body);
            }

            if i < args.len() {
                if args[i] == "elseif" {
                    i += 1;
                    continue;
                } else if args[i] == "else" {
                    i += 1;
                    if i < args.len() {
                        return self.eval(&args[i]);
                    }
                } else if i + 1 == args.len() {
                    // Implicit else block: `if {cond} {thenBody} {elseBody}`
                    return self.eval(&args[i]);
                }
            }
        }
        Ok(String::new())
    }

    fn cmd_for(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() != 4 {
            return Err(TclControl::Error(
                "wrong # args: should be \"for start test next command\"".to_string(),
            ));
        }
        self.eval(&args[0])?;
        loop {
            let cond = eval_expr(self, &args[1])?
                .as_bool()
                .map_err(TclControl::Error)?;
            if !cond {
                break;
            }
            match self.eval(&args[3]) {
                Ok(_) => {}
                Err(TclControl::Continue) => {}
                Err(TclControl::Break) => break,
                Err(other) => return Err(other),
            }
            self.eval(&args[2])?;
        }
        Ok(String::new())
    }

    fn cmd_foreach(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 3 || args.len().is_multiple_of(2) {
            return Err(TclControl::Error(
                "wrong # args: should be \"foreach varList list ?varList list ...? command\""
                    .to_string(),
            ));
        }
        let body = args.last().unwrap();
        let pairs_slice = &args[..args.len() - 1];
        let mut specs: Vec<(Vec<String>, Vec<String>)> = Vec::new();
        let mut max_iters = 0usize;

        for pair in pairs_slice.as_chunks::<2>().0 {
            let vars = parse_list(&pair[0]).map_err(TclControl::Error)?;
            if vars.is_empty() {
                return Err(TclControl::Error("foreach varlist is empty".to_string()));
            }
            let vals = parse_list(&pair[1]).map_err(TclControl::Error)?;
            let iters = vals.len().div_ceil(vars.len());
            if iters > max_iters {
                max_iters = iters;
            }
            specs.push((vars, vals));
        }

        for iter_idx in 0..max_iters {
            for (vars, vals) in &specs {
                let base = iter_idx * vars.len();
                for (vi, vname) in vars.iter().enumerate() {
                    let val = vals.get(base + vi).cloned().unwrap_or_default();
                    self.set_var(vname, val).map_err(TclControl::Error)?;
                }
            }
            if !body.trim().is_empty() {
                match self.eval(body) {
                    Ok(_) => {}
                    Err(TclControl::Continue) => {}
                    Err(TclControl::Break) => break,
                    Err(other) => return Err(other),
                }
            }
        }
        Ok(String::new())
    }

    fn cmd_while(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() != 2 {
            return Err(TclControl::Error(
                "wrong # args: should be \"while test command\"".to_string(),
            ));
        }
        loop {
            let cond = eval_expr(self, &args[0])?
                .as_bool()
                .map_err(TclControl::Error)?;
            if !cond {
                break;
            }
            match self.eval(&args[1]) {
                Ok(_) => {}
                Err(TclControl::Continue) => {}
                Err(TclControl::Break) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(String::new())
    }

    fn cmd_switch(&mut self, args: &[String]) -> TclResult<String> {
        #[derive(Clone, Copy)]
        enum Mode {
            Exact,
            Glob,
            Regexp,
        }
        let mut mode = Mode::Exact;
        let mut nocase = false;
        let mut idx = 0;
        while idx < args.len() && args[idx].starts_with('-') {
            match args[idx].as_str() {
                "-exact" => mode = Mode::Exact,
                "-glob" => mode = Mode::Glob,
                "-regexp" => mode = Mode::Regexp,
                "-nocase" => nocase = true,
                "--" => {
                    idx += 1;
                    break;
                }
                _ => break,
            }
            idx += 1;
        }
        if idx >= args.len() {
            return Err(TclControl::Error("wrong # args to switch".to_string()));
        }
        let target = &args[idx];
        let rest = &args[idx + 1..];
        let branches: Vec<String> = if rest.len() == 1 {
            parse_list(&rest[0]).map_err(TclControl::Error)?
        } else {
            rest.to_vec()
        };
        if !branches.len().is_multiple_of(2) {
            return Err(TclControl::Error(
                "extra switch pattern with no body".to_string(),
            ));
        }

        let mut matched = false;
        let num_pairs = branches.len() / 2;
        for (pi, pair) in branches.as_chunks::<2>().0.iter().enumerate() {
            let pat = &pair[0];
            let body = &pair[1];
            if !matched {
                matched = if pi + 1 == num_pairs && pat == "default" {
                    true
                } else {
                    match mode {
                        Mode::Exact => {
                            if nocase {
                                target.eq_ignore_ascii_case(pat)
                            } else {
                                target == pat
                            }
                        }
                        Mode::Glob => glob_match(pat, target, nocase),
                        Mode::Regexp => Regex::compile(pat, nocase)
                            .map_err(TclControl::Error)?
                            .is_match(target),
                    }
                };
            }
            if matched && body != "-" {
                return self.eval(body);
            }
        }
        Ok(String::new())
    }

    fn cmd_return(&mut self, args: &[String]) -> TclResult<String> {
        let mut code = 0i32;
        let mut i = 0;
        while i + 1 < args.len() && args[i].starts_with('-') {
            match args[i].as_str() {
                "-code" => {
                    code = match args[i + 1].as_str() {
                        "ok" => 0,
                        "error" => 1,
                        "return" => 2,
                        "break" => 3,
                        "continue" => 4,
                        n => n.parse::<i32>().unwrap_or(0),
                    };
                    i += 2;
                }
                "-level" | "-errorinfo" | "-errorcode" => {
                    i += 2;
                }
                _ => break,
            }
        }
        let value = args.get(i).cloned().unwrap_or_default();
        Err(TclControl::Return { code, value })
    }

    fn cmd_catch(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() || args.len() > 3 {
            return Err(TclControl::Error(
                "wrong # args: should be \"catch script ?resultVarName? ?optionVarName?\""
                    .to_string(),
            ));
        }
        let (code, val) = match self.eval(&args[0]) {
            Ok(v) => (0, v),
            Err(TclControl::FinishTest) => return Err(TclControl::FinishTest),
            Err(TclControl::Error(msg)) => (1, msg),
            Err(TclControl::Return { code, value }) => (if code == 0 { 2 } else { code }, value),
            Err(TclControl::Break) => (3, String::new()),
            Err(TclControl::Continue) => (4, String::new()),
        };
        if let Some(var_name) = args.get(1)
            && !var_name.is_empty()
        {
            self.set_var(var_name, val).map_err(TclControl::Error)?;
        }
        Ok(code.to_string())
    }

    fn cmd_uplevel(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to uplevel".to_string()));
        }
        let (target_frame, script_parts) = if args.len() > 1
            && (args[0].starts_with('#') || args[0].bytes().all(|b| b.is_ascii_digit()))
        {
            (self.resolve_level(&args[0])?, &args[1..])
        } else {
            let caller = if self.current_frame == 0 {
                0
            } else {
                self.frames[self.current_frame].caller_frame
            };
            (caller, args)
        };
        let script = if script_parts.len() == 1 {
            script_parts[0].clone()
        } else {
            script_parts.join(" ")
        };
        let prev = self.current_frame;
        self.current_frame = target_frame;
        let res = self.eval(&script);
        self.current_frame = prev;
        res
    }

    fn cmd_subst(&mut self, args: &[String]) -> TclResult<String> {
        let mut do_bs = true;
        let mut do_vars = true;
        let mut do_cmds = true;
        let mut i = 0;
        while i + 1 < args.len() && args[i].starts_with('-') {
            let opt = args[i].as_str();
            if opt.len() >= 4 && "-nobackslashes".starts_with(opt) {
                do_bs = false;
            } else if opt.len() >= 4 && "-novariables".starts_with(opt) {
                do_vars = false;
            } else if opt.len() >= 4 && "-nocommands".starts_with(opt) {
                do_cmds = false;
            } else {
                break;
            }
            i += 1;
        }
        let s = args.get(i).map_or("", String::as_str);
        self.subst_string(s, do_bs, do_vars, do_cmds)
    }

    fn cmd_split(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() || args.len() > 2 {
            return Err(TclControl::Error("wrong # args to split".to_string()));
        }
        let s = &args[0];
        if s.is_empty() {
            return Ok(String::new());
        }
        if args.len() == 2 && args[1].is_empty() {
            let chars: Vec<String> = s.chars().map(|c| c.to_string()).collect();
            return Ok(format_list(&chars));
        }
        let sep_chars: Vec<char> = if args.len() == 2 {
            args[1].chars().collect()
        } else {
            vec![' ', '\t', '\n', '\r']
        };
        let parts: Vec<&str> = s.split(|c: char| sep_chars.contains(&c)).collect();
        Ok(format_list(&parts))
    }

    fn cmd_join(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() || args.len() > 2 {
            return Err(TclControl::Error("wrong # args to join".to_string()));
        }
        let items = parse_list(&args[0]).map_err(TclControl::Error)?;
        let sep = args.get(1).map_or(" ", String::as_str);
        Ok(items.join(sep))
    }

    fn cmd_lindex(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to lindex".to_string()));
        }
        let mut cur = args[0].clone();
        let indices: Vec<String> = if args.len() == 2 {
            parse_list(&args[1]).unwrap_or_else(|_| vec![args[1].clone()])
        } else {
            args[1..].to_vec()
        };
        for idx_str in indices {
            let items = parse_list(&cur).map_err(TclControl::Error)?;
            let idx = parse_tcl_index(&idx_str, items.len()).map_err(TclControl::Error)?;
            if idx < 0 || (idx as usize) >= items.len() {
                return Ok(String::new());
            }
            cur = items[idx as usize].clone();
        }
        Ok(cur)
    }

    fn cmd_lrange(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() != 3 {
            return Err(TclControl::Error("wrong # args to lrange".to_string()));
        }
        let items = parse_list(&args[0]).map_err(TclControl::Error)?;
        if items.is_empty() {
            return Ok(String::new());
        }
        let first = parse_tcl_index(&args[1], items.len())
            .map_err(TclControl::Error)?
            .max(0) as usize;
        let last_i = parse_tcl_index(&args[2], items.len()).map_err(TclControl::Error)?;
        if last_i < 0 || first >= items.len() || (last_i as usize) < first {
            return Ok(String::new());
        }
        let last = (last_i as usize).min(items.len() - 1);
        Ok(format_list(&items[first..=last]))
    }

    fn cmd_linsert(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 2 {
            return Err(TclControl::Error("wrong # args to linsert".to_string()));
        }
        let mut items = parse_list(&args[0]).map_err(TclControl::Error)?;
        let idx = parse_tcl_index(&args[1], items.len() + 1)
            .map_err(TclControl::Error)?
            .clamp(0, items.len() as i64) as usize;
        for (off, elem) in args[2..].iter().enumerate() {
            items.insert(idx + off, elem.clone());
        }
        Ok(format_list(&items))
    }

    fn cmd_lreplace(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 3 {
            return Err(TclControl::Error("wrong # args to lreplace".to_string()));
        }
        let mut items = parse_list(&args[0]).map_err(TclControl::Error)?;
        let first = parse_tcl_index(&args[1], items.len())
            .map_err(TclControl::Error)?
            .clamp(0, items.len() as i64) as usize;
        let last = parse_tcl_index(&args[2], items.len()).map_err(TclControl::Error)?;
        let end = if last < first as i64 {
            first
        } else {
            ((last as usize) + 1).min(items.len())
        };
        items.splice(first..end, args[3..].iter().cloned());
        Ok(format_list(&items))
    }

    fn cmd_lset(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 3 {
            return Err(TclControl::Error("wrong # args to lset".to_string()));
        }
        let cur = self.get_var(&args[0]).map_err(TclControl::Error)?;
        let mut items = parse_list(&cur).map_err(TclControl::Error)?;
        let idx = parse_tcl_index(&args[1], items.len()).map_err(TclControl::Error)?;
        if idx < 0 || (idx as usize) >= items.len() {
            return Err(TclControl::Error("list index out of range".to_string()));
        }
        items[idx as usize] = args.last().unwrap().clone();
        let formatted = format_list(&items);
        self.set_var(&args[0], formatted).map_err(TclControl::Error)
    }

    fn cmd_lsearch(&mut self, args: &[String]) -> TclResult<String> {
        #[derive(Clone, Copy)]
        enum Mode {
            Glob,
            Exact,
            Regexp,
        }
        let mut mode = Mode::Glob;
        let mut nocase = false;
        let mut inline = false;
        let mut all = false;
        let mut invert = false;
        let mut i = 0;
        while i + 2 < args.len() && args[i].starts_with('-') {
            match args[i].as_str() {
                "-exact" => mode = Mode::Exact,
                "-glob" => mode = Mode::Glob,
                "-regexp" => mode = Mode::Regexp,
                "-nocase" => nocase = true,
                "-inline" => inline = true,
                "-all" => all = true,
                "-not" => invert = true,
                "-integer" | "-ascii" | "-dictionary" => {}
                "--" => {
                    i += 1;
                    break;
                }
                _ => break,
            }
            i += 1;
        }
        if i + 2 > args.len() {
            return Err(TclControl::Error("wrong # args to lsearch".to_string()));
        }
        let items = parse_list(&args[i]).map_err(TclControl::Error)?;
        let pat = &args[i + 1];
        let compiled_re = if matches!(mode, Mode::Regexp) {
            Some(Regex::compile(pat, nocase).map_err(TclControl::Error)?)
        } else {
            None
        };

        let mut results = Vec::new();
        for (idx, item) in items.iter().enumerate() {
            let mut hit = match mode {
                Mode::Exact => {
                    if nocase {
                        item.eq_ignore_ascii_case(pat)
                    } else {
                        item == pat
                    }
                }
                Mode::Glob => glob_match(pat, item, nocase),
                Mode::Regexp => compiled_re.as_ref().unwrap().is_match(item),
            };
            if invert {
                hit = !hit;
            }
            if hit {
                if inline {
                    results.push(item.clone());
                } else {
                    results.push(idx.to_string());
                }
                if !all {
                    break;
                }
            }
        }
        if all {
            Ok(format_list(&results))
        } else if let Some(first) = results.into_iter().next() {
            Ok(first)
        } else if inline {
            Ok(String::new())
        } else {
            Ok("-1".to_string())
        }
    }

    fn cmd_lsort(&mut self, args: &[String]) -> TclResult<String> {
        #[derive(Clone, Copy)]
        enum SortKind {
            Ascii,
            Integer,
            Real,
        }
        let mut kind = SortKind::Ascii;
        let mut decreasing = false;
        let mut unique = false;
        let mut nocase = false;
        let mut index_opt: Option<String> = None;
        let mut command_opt: Option<String> = None;
        let mut i = 0;
        while i + 1 < args.len() && args[i].starts_with('-') {
            match args[i].as_str() {
                "-integer" => kind = SortKind::Integer,
                "-real" => kind = SortKind::Real,
                "-ascii" | "-dictionary" => kind = SortKind::Ascii,
                "-decreasing" => decreasing = true,
                "-increasing" => decreasing = false,
                "-unique" => unique = true,
                "-nocase" => nocase = true,
                "-index" if i + 2 < args.len() => {
                    index_opt = Some(args[i + 1].clone());
                    i += 1;
                }
                "-command" if i + 2 < args.len() => {
                    command_opt = Some(args[i + 1].clone());
                    i += 1;
                }
                "--" => {
                    i += 1;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let list_str = args
            .get(i)
            .or_else(|| args.last())
            .map_or("", String::as_str);
        let mut items = parse_list(list_str).map_err(TclControl::Error)?;

        let extract_key = |elem: &str| -> String {
            if let Some(ref idx_s) = index_opt
                && let Ok(sub) = parse_list(elem)
                && let Ok(k) = parse_tcl_index(idx_s, sub.len())
                && k >= 0
                && (k as usize) < sub.len()
            {
                return sub[k as usize].clone();
            }
            elem.to_string()
        };

        if let Some(ref cmd_prefix) = command_opt {
            // Stable merge/insertion sort via custom TCL comparison command
            for idx in 1..items.len() {
                let mut pos = idx;
                while pos > 0 {
                    let ka = extract_key(&items[pos - 1]);
                    let kb = extract_key(&items[pos]);
                    let script = format!(
                        "{cmd_prefix} {} {}",
                        format_list(std::slice::from_ref(&ka)),
                        format_list(std::slice::from_ref(&kb))
                    );
                    let res = self.eval(&script)?;
                    let cmp_n = parse_tcl_int(res.trim()).unwrap_or(0);
                    let ord = cmp_n.cmp(&0);
                    let eff = if decreasing { ord.reverse() } else { ord };
                    if eff == std::cmp::Ordering::Greater {
                        items.swap(pos - 1, pos);
                        pos -= 1;
                    } else {
                        break;
                    }
                }
            }
        } else {
            items.sort_by(|a, b| {
                let ka = extract_key(a);
                let kb = extract_key(b);
                let ord = match kind {
                    SortKind::Integer => {
                        let ia = parse_tcl_int(ka.trim()).unwrap_or(0);
                        let ib = parse_tcl_int(kb.trim()).unwrap_or(0);
                        ia.cmp(&ib)
                    }
                    SortKind::Real => {
                        let fa = parse_tcl_float(ka.trim()).unwrap_or(0.0);
                        let fb = parse_tcl_float(kb.trim()).unwrap_or(0.0);
                        fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal)
                    }
                    SortKind::Ascii => {
                        if nocase {
                            ka.to_ascii_lowercase().cmp(&kb.to_ascii_lowercase())
                        } else {
                            ka.cmp(&kb)
                        }
                    }
                };
                if decreasing { ord.reverse() } else { ord }
            });
        }

        if unique {
            items.dedup_by(|a, b| {
                if nocase {
                    a.eq_ignore_ascii_case(b)
                } else {
                    a == b
                }
            });
        }
        Ok(format_list(&items))
    }

    fn cmd_string(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to string".to_string()));
        }
        let sub = args[0].as_str();
        let rest = &args[1..];
        match sub {
            "length" => Ok(rest.first().map_or(0, |s| s.chars().count()).to_string()),
            "bytelength" => Ok(rest.first().map_or(0, String::len).to_string()),
            "tolower" => Ok(rest.first().map_or(String::new(), |s| s.to_lowercase())),
            "toupper" => Ok(rest.first().map_or(String::new(), |s| s.to_uppercase())),
            "trim" | "trimleft" | "trimright" => {
                let s = rest.first().map_or("", String::as_str);
                let chars_opt = rest.get(1).map(|c| c.chars().collect::<Vec<char>>());
                let pred = |ch: char| {
                    if let Some(ref set) = chars_opt {
                        set.contains(&ch)
                    } else {
                        ch.is_whitespace()
                    }
                };
                let out = match sub {
                    "trim" => s.trim_matches(pred),
                    "trimleft" => s.trim_start_matches(pred),
                    _ => s.trim_end_matches(pred),
                };
                Ok(out.to_string())
            }
            "index" => {
                if rest.len() != 2 {
                    return Err(TclControl::Error(
                        "wrong # args to string index".to_string(),
                    ));
                }
                let chars: Vec<char> = rest[0].chars().collect();
                let idx = parse_tcl_index(&rest[1], chars.len()).map_err(TclControl::Error)?;
                if idx < 0 || (idx as usize) >= chars.len() {
                    Ok(String::new())
                } else {
                    Ok(chars[idx as usize].to_string())
                }
            }
            "range" => {
                if rest.len() != 3 {
                    return Err(TclControl::Error(
                        "wrong # args to string range".to_string(),
                    ));
                }
                let chars: Vec<char> = rest[0].chars().collect();
                if chars.is_empty() {
                    return Ok(String::new());
                }
                let first = parse_tcl_index(&rest[1], chars.len())
                    .map_err(TclControl::Error)?
                    .max(0) as usize;
                let last_i = parse_tcl_index(&rest[2], chars.len()).map_err(TclControl::Error)?;
                if last_i < 0 || first >= chars.len() || (last_i as usize) < first {
                    return Ok(String::new());
                }
                let last = (last_i as usize).min(chars.len() - 1);
                Ok(chars[first..=last].iter().collect())
            }
            "replace" => {
                if rest.len() < 3 {
                    return Err(TclControl::Error(
                        "wrong # args to string replace".to_string(),
                    ));
                }
                let mut chars: Vec<char> = rest[0].chars().collect();
                if chars.is_empty() {
                    return Ok(String::new());
                }
                let first = parse_tcl_index(&rest[1], chars.len())
                    .map_err(TclControl::Error)?
                    .max(0) as usize;
                let last_i = parse_tcl_index(&rest[2], chars.len()).map_err(TclControl::Error)?;
                if last_i < (first as i64) || first >= chars.len() {
                    return Ok(rest[0].clone());
                }
                let end = ((last_i as usize) + 1).min(chars.len());
                let rep: Vec<char> = rest.get(3).map_or("", String::as_str).chars().collect();
                chars.splice(first..end, rep);
                Ok(chars.into_iter().collect())
            }
            "repeat" => {
                if rest.len() != 2 {
                    return Err(TclControl::Error(
                        "wrong # args to string repeat".to_string(),
                    ));
                }
                let n = parse_tcl_int(rest[1].trim()).unwrap_or(0).max(0) as usize;
                Ok(rest[0].repeat(n))
            }
            "reverse" => Ok(rest
                .first()
                .map_or(String::new(), |s| s.chars().rev().collect())),
            "map" => {
                let (nocase, map_str, target) = if rest.len() == 3 && rest[0] == "-nocase" {
                    (true, &rest[1], &rest[2])
                } else if rest.len() == 2 {
                    (false, &rest[0], &rest[1])
                } else {
                    return Err(TclControl::Error("wrong # args to string map".to_string()));
                };
                let pairs = parse_list(map_str).map_err(TclControl::Error)?;
                if pairs.len() % 2 != 0 {
                    return Err(TclControl::Error("char map list unbalanced".to_string()));
                }
                let mut out = String::with_capacity(target.len());
                let mut pos = 0usize;
                while pos < target.len() {
                    let rem = &target[pos..];
                    let mut matched = false;
                    for kv in pairs.as_chunks::<2>().0 {
                        let k = &kv[0];
                        let v = &kv[1];
                        if k.is_empty() {
                            continue;
                        }
                        let hit = if nocase {
                            rem.len() >= k.len()
                                && rem.is_char_boundary(k.len())
                                && rem[..k.len()].eq_ignore_ascii_case(k)
                        } else {
                            rem.starts_with(k.as_str())
                        };
                        if hit {
                            out.push_str(v);
                            pos += k.len();
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        let ch = rem.chars().next().unwrap();
                        out.push(ch);
                        pos += ch.len_utf8();
                    }
                }
                Ok(out)
            }
            "match" => {
                let (nocase, pat, txt) = if rest.len() == 3 && rest[0] == "-nocase" {
                    (true, &rest[1], &rest[2])
                } else if rest.len() == 2 {
                    (false, &rest[0], &rest[1])
                } else {
                    return Err(TclControl::Error(
                        "wrong # args to string match".to_string(),
                    ));
                };
                Ok(if glob_match(pat, txt, nocase) {
                    "1".to_string()
                } else {
                    "0".to_string()
                })
            }
            "compare" | "equal" => {
                let mut nocase = false;
                let mut len_limit: Option<usize> = None;
                let mut i = 0;
                while i + 2 < rest.len() && rest[i].starts_with('-') {
                    match rest[i].as_str() {
                        "-nocase" => nocase = true,
                        "-length" if i + 3 <= rest.len() => {
                            len_limit =
                                Some(parse_tcl_int(&rest[i + 1]).unwrap_or(0).max(0) as usize);
                            i += 1;
                        }
                        _ => break,
                    }
                    i += 1;
                }
                if i + 2 != rest.len() {
                    return Err(TclControl::Error(format!("wrong # args to string {sub}")));
                }
                let mut a: String = rest[i].clone();
                let mut b: String = rest[i + 1].clone();
                if let Some(lim) = len_limit {
                    a = a.chars().take(lim).collect();
                    b = b.chars().take(lim).collect();
                }
                if nocase {
                    a = a.to_lowercase();
                    b = b.to_lowercase();
                }
                if sub == "equal" {
                    Ok(if a == b { "1" } else { "0" }.to_string())
                } else {
                    let c = match a.cmp(&b) {
                        std::cmp::Ordering::Less => "-1",
                        std::cmp::Ordering::Equal => "0",
                        std::cmp::Ordering::Greater => "1",
                    };
                    Ok(c.to_string())
                }
            }
            "first" | "last" => {
                if rest.len() < 2 {
                    return Err(TclControl::Error(format!("wrong # args to string {sub}")));
                }
                let needle = &rest[0];
                let hay_chars: Vec<char> = rest[1].chars().collect();
                let needle_chars: Vec<char> = needle.chars().collect();
                if needle_chars.is_empty() || needle_chars.len() > hay_chars.len() {
                    return Ok("-1".to_string());
                }
                if sub == "first" {
                    let start = if let Some(st) = rest.get(2) {
                        parse_tcl_index(st, hay_chars.len()).unwrap_or(0).max(0) as usize
                    } else {
                        0
                    };
                    for i in start..=(hay_chars.len() - needle_chars.len()) {
                        if hay_chars[i..i + needle_chars.len()] == needle_chars[..] {
                            return Ok(i.to_string());
                        }
                    }
                    Ok("-1".to_string())
                } else {
                    let max_i = hay_chars.len() - needle_chars.len();
                    for i in (0..=max_i).rev() {
                        if hay_chars[i..i + needle_chars.len()] == needle_chars[..] {
                            return Ok(i.to_string());
                        }
                    }
                    Ok("-1".to_string())
                }
            }
            "is" => {
                if rest.len() < 2 {
                    return Err(TclControl::Error("wrong # args to string is".to_string()));
                }
                let cls = &rest[0];
                let mut strict = false;
                let mut i = 1;
                while i + 1 < rest.len() && rest[i].starts_with('-') {
                    if rest[i] == "-strict" {
                        strict = true;
                    }
                    i += 1;
                }
                let val = &rest[i];
                if val.is_empty() {
                    return Ok(if strict { "0" } else { "1" }.to_string());
                }
                let ok = match cls.as_str() {
                    "integer" | "wideinteger" | "entier" => parse_tcl_int(val.trim()).is_some(),
                    "double" => parse_tcl_float(val.trim()).is_some(),
                    "boolean" => parse_tcl_bool(val).is_ok(),
                    "true" => matches!(parse_tcl_bool(val), Ok(true)),
                    "false" => matches!(parse_tcl_bool(val), Ok(false)),
                    "digit" => val.chars().all(|c| c.is_ascii_digit()),
                    "alpha" => val.chars().all(|c| c.is_alphabetic()),
                    "alnum" => val.chars().all(|c| c.is_alphanumeric()),
                    "ascii" => val.is_ascii(),
                    "space" => val.chars().all(|c| c.is_whitespace()),
                    "xdigit" => val.chars().all(|c| c.is_ascii_hexdigit()),
                    _ => true,
                };
                Ok(if ok { "1" } else { "0" }.to_string())
            }
            other => Err(TclControl::Error(format!(
                "unknown string subcommand \"{other}\""
            ))),
        }
    }

    fn cmd_array(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 2 {
            return Err(TclControl::Error("wrong # args to array".to_string()));
        }
        let sub = args[0].as_str();
        let arr_name = &args[1];
        let (is_global, base, _) = self.parse_var_ref(arr_name);
        let start_frame = if is_global { 0 } else { self.current_frame };
        let (fidx, bname) = self.resolve_target(start_frame, base.to_string());

        match sub {
            "exists" => {
                let exists = matches!(self.frames[fidx].vars.get(&bname), Some(VarSlot::Array(_)));
                Ok(if exists { "1" } else { "0" }.to_string())
            }
            "size" => {
                let sz = match self.frames[fidx].vars.get(&bname) {
                    Some(VarSlot::Array(m)) => m.len(),
                    _ => 0,
                };
                Ok(sz.to_string())
            }
            "set" => {
                let list_str = args.get(2).map_or("", String::as_str);
                let items = parse_list(list_str).map_err(TclControl::Error)?;
                if items.len() % 2 != 0 {
                    return Err(TclControl::Error(
                        "list must have an even number of elements".to_string(),
                    ));
                }
                let slot = self.frames[fidx]
                    .vars
                    .entry(bname)
                    .or_insert_with(|| VarSlot::Array(HashMap::new()));
                let VarSlot::Array(map) = slot else {
                    return Err(TclControl::Error(format!(
                        "can't array set \"{arr_name}\": variable isn't array"
                    )));
                };
                for kv in items.as_chunks::<2>().0 {
                    map.insert(kv[0].clone(), kv[1].clone());
                }
                Ok(String::new())
            }
            "get" => {
                let pat = args.get(2).map_or("*", String::as_str);
                let mut out = Vec::new();
                if let Some(VarSlot::Array(map)) = self.frames[fidx].vars.get(&bname) {
                    let mut keys: Vec<&String> =
                        map.keys().filter(|k| glob_match(pat, k, false)).collect();
                    keys.sort();
                    for k in keys {
                        out.push(k.clone());
                        out.push(map[k].clone());
                    }
                }
                Ok(format_list(&out))
            }
            "names" => {
                let pat = args
                    .last()
                    .filter(|_| args.len() >= 3)
                    .map_or("*", String::as_str);
                let mut out = Vec::new();
                if let Some(VarSlot::Array(map)) = self.frames[fidx].vars.get(&bname) {
                    for k in map.keys() {
                        if glob_match(pat, k, false) {
                            out.push(k.clone());
                        }
                    }
                    out.sort();
                }
                Ok(format_list(&out))
            }
            "unset" => {
                if let Some(pat) = args.get(2) {
                    if let Some(VarSlot::Array(map)) = self.frames[fidx].vars.get_mut(&bname) {
                        map.retain(|k, _| !glob_match(pat, k, false));
                    }
                } else {
                    self.frames[fidx].vars.remove(&bname);
                }
                Ok(String::new())
            }
            other => Err(TclControl::Error(format!(
                "unknown array subcommand \"{other}\""
            ))),
        }
    }

    fn cmd_info(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to info".to_string()));
        }
        match args[0].as_str() {
            "exists" => {
                let name = args.get(1).map_or("", String::as_str);
                Ok(if self.var_exists(name) { "1" } else { "0" }.to_string())
            }
            "procs" => {
                let pat = args.get(1).map_or("*", String::as_str);
                let mut hits: Vec<String> = self
                    .procs
                    .keys()
                    .filter(|k| glob_match(pat, k, false))
                    .cloned()
                    .collect();
                hits.sort();
                Ok(format_list(&hits))
            }
            "commands" => {
                let pat = args.get(1).map_or("*", String::as_str);
                let mut hits: Vec<String> = Vec::new();
                for k in self.procs.keys() {
                    if glob_match(pat, k, false) {
                        hits.push(k.clone());
                    }
                }
                for k in self.builtins.keys() {
                    if glob_match(pat, k, false) {
                        hits.push(k.clone());
                    }
                }
                if let Some(ref host_rc) = self.host {
                    for k in crate::harness::list_host_cmds(host_rc) {
                        if glob_match(pat, &k, false) {
                            hits.push(k);
                        }
                    }
                }
                hits.sort();
                hits.dedup();
                Ok(format_list(&hits))
            }
            "vars" | "globals" | "locals" => {
                let pat = args.get(1).map_or("*", String::as_str);
                let fidx = if args[0] == "globals" {
                    0
                } else {
                    self.current_frame
                };
                let mut hits: Vec<String> = self.frames[fidx]
                    .vars
                    .keys()
                    .filter(|k| glob_match(pat, k, false))
                    .cloned()
                    .collect();
                if fidx == 0 && self.host.is_some() && glob_match(pat, "sqlite_pending_byte", false)
                {
                    hits.push("sqlite_pending_byte".to_string());
                }
                hits.sort();
                hits.dedup();
                Ok(format_list(&hits))
            }
            "level" => {
                let mut depth = 0usize;
                let mut c = self.current_frame;
                while c > 0 {
                    depth += 1;
                    c = self.frames[c].caller_frame;
                }
                Ok(depth.to_string())
            }
            "script" => Ok(self.script_path.clone()),
            "nameofexec" => Ok(resolve_testfixture_binary().to_string_lossy().into_owned()),
            "patchlevel" | "tclversion" => Ok("8.6".to_string()),
            _ => Ok(String::new()),
        }
    }

    fn cmd_format(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to format".to_string()));
        }
        tcl_format(&args[0], &args[1..]).map_err(TclControl::Error)
    }

    fn cmd_scan(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 2 {
            return Err(TclControl::Error("wrong # args to scan".to_string()));
        }
        let s = &args[0];
        let fmt = args[1].trim();
        let val = match fmt {
            "%c" => s.chars().next().map(|c| (c as u32).to_string()),
            "%d" | "%i" | "%lld" => parse_tcl_int(s.trim()).map(|v| v.to_string()),
            "%x" | "%X" => i64::from_str_radix(s.trim().trim_start_matches("0x"), 16)
                .ok()
                .map(|v| v.to_string()),
            "%f" | "%g" | "%e" => parse_tcl_float(s.trim()).map(|v| v.to_string()),
            _ => Some(s.clone()),
        };
        if args.len() >= 3 {
            if let Some(v) = val {
                self.set_var(&args[2], v).map_err(TclControl::Error)?;
                Ok("1".to_string())
            } else {
                Ok("0".to_string())
            }
        } else {
            Ok(val.unwrap_or_default())
        }
    }

    fn cmd_regexp(&mut self, args: &[String]) -> TclResult<String> {
        let mut nocase = false;
        let mut inline = false;
        let mut all = false;
        let mut i = 0;
        while i < args.len() && args[i].starts_with('-') {
            match args[i].as_str() {
                "-nocase" => nocase = true,
                "-inline" => inline = true,
                "-all" => all = true,
                "-start" => {
                    i += 1;
                }
                "--" => {
                    i += 1;
                    break;
                }
                _ => break,
            }
            i += 1;
        }
        if i + 2 > args.len() {
            return Err(TclControl::Error("wrong # args to regexp".to_string()));
        }
        let pat = &args[i];
        let txt = &args[i + 1];
        let match_vars = &args[i + 2..];
        let re = Regex::compile(pat, nocase).map_err(TclControl::Error)?;
        let chars: Vec<char> = txt.chars().collect();

        if inline {
            if let Some(caps) = re.find_captures(txt) {
                let mut out = Vec::new();
                for grp in caps.groups {
                    if let Some((s, e)) = grp {
                        out.push(chars[s..e].iter().collect::<String>());
                    } else {
                        out.push(String::new());
                    }
                }
                return Ok(format_list(&out));
            }
            return Ok(String::new());
        }

        if let Some(caps) = re.find_captures(txt) {
            for (vi, vname) in match_vars.iter().enumerate() {
                let sub = caps
                    .groups
                    .get(vi)
                    .copied()
                    .flatten()
                    .map(|(s, e)| chars[s..e].iter().collect::<String>())
                    .unwrap_or_default();
                self.set_var(vname, sub).map_err(TclControl::Error)?;
            }
            if all {
                let (_, cnt) = re.replace(txt, "", true);
                return Ok(cnt.max(1).to_string());
            }
            Ok("1".to_string())
        } else {
            Ok("0".to_string())
        }
    }

    fn cmd_regsub(&mut self, args: &[String]) -> TclResult<String> {
        let mut nocase = false;
        let mut all = false;
        let mut i = 0;
        while i < args.len() && args[i].starts_with('-') {
            match args[i].as_str() {
                "-nocase" => nocase = true,
                "-all" => all = true,
                "--" => {
                    i += 1;
                    break;
                }
                _ => break,
            }
            i += 1;
        }
        if i + 3 > args.len() {
            return Err(TclControl::Error("wrong # args to regsub".to_string()));
        }
        let pat = &args[i];
        let txt = &args[i + 1];
        let rep = &args[i + 2];
        let var_opt = args.get(i + 3);
        let re = Regex::compile(pat, nocase).map_err(TclControl::Error)?;
        let (replaced, count) = re.replace(txt, rep, all);
        if let Some(var_name) = var_opt {
            self.set_var(var_name, replaced)
                .map_err(TclControl::Error)?;
            Ok(count.to_string())
        } else {
            Ok(replaced)
        }
    }

    fn cmd_binary(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() < 2 {
            return Err(TclControl::Error("wrong # args to binary".to_string()));
        }
        match args[0].as_str() {
            "format" => {
                let fmt = &args[1];
                let vals = &args[2..];
                let bytes = tcl_binary_format(fmt, vals)?;
                self.last_result_is_bytearray = true;
                Ok(bytes.into_iter().map(char::from).collect())
            }
            "scan" => {
                if args.len() < 3 {
                    return Err(TclControl::Error("wrong # args to binary scan".to_string()));
                }
                let data_bytes: Vec<u8> =
                    args[1].chars().map(|ch| (ch as u32 & 0xff) as u8).collect();
                let fmt = &args[2];
                let vars = &args[3..];
                let assigned = tcl_binary_scan(&data_bytes, fmt, vars.len())?;
                let count = assigned.len();
                for (var_name, val_opt) in vars.iter().zip(assigned) {
                    if let Some(v) = val_opt {
                        self.set_var(var_name, v).map_err(TclControl::Error)?;
                    }
                }
                Ok(count.to_string())
            }
            _ => Ok(String::new()),
        }
    }

    fn cmd_file(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to file".to_string()));
        }
        match args[0].as_str() {
            "exists" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                Ok(if p.exists() { "1" } else { "0" }.to_string())
            }
            "isfile" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                Ok(if p.is_file() { "1" } else { "0" }.to_string())
            }
            "isdir" | "isdirectory" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                Ok(if p.is_dir() { "1" } else { "0" }.to_string())
            }
            "executable" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let ok = fs::metadata(&p)
                        .map(|m| (m.permissions().mode() & 0o111) != 0)
                        .unwrap_or(false);
                    Ok(if ok { "1" } else { "0" }.to_string())
                }
                #[cfg(not(unix))]
                {
                    Ok(if p.exists() { "1" } else { "0" }.to_string())
                }
            }
            "readable" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                let ok = if p.is_dir() {
                    fs::read_dir(&p).is_ok()
                } else {
                    fs::File::open(&p).is_ok()
                };
                Ok(if ok { "1" } else { "0" }.to_string())
            }
            "writable" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                let ok = if !p.exists() {
                    false
                } else if p.is_dir() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::metadata(&p)
                            .map(|m| (m.permissions().mode() & 0o222) != 0)
                            .unwrap_or(false)
                    }
                    #[cfg(not(unix))]
                    {
                        !fs::metadata(&p)
                            .map(|m| m.permissions().readonly())
                            .unwrap_or(true)
                    }
                } else {
                    fs::OpenOptions::new().write(true).open(&p).is_ok()
                };
                Ok(if ok { "1" } else { "0" }.to_string())
            }
            "attributes" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                if args.len() == 3 {
                    match args[2].as_str() {
                        "-permissions" | "-permission" => {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::PermissionsExt;
                                let m = fs::metadata(&p)
                                    .map_err(|e| TclControl::Error(e.to_string()))?;
                                return Ok(format!("0{:04o}", m.permissions().mode() & 0o7777));
                            }
                            #[cfg(not(unix))]
                            {
                                return Ok("00644".to_string());
                            }
                        }
                        "-readonly" => {
                            let m =
                                fs::metadata(&p).map_err(|e| TclControl::Error(e.to_string()))?;
                            return Ok(
                                if m.permissions().readonly() { "1" } else { "0" }.to_string()
                            );
                        }
                        _ => return Ok(String::new()),
                    }
                }
                let mut i = 2;
                while i + 1 < args.len() {
                    match args[i].as_str() {
                        "-permissions" | "-permission" => {
                            let raw_perm = args[i + 1].trim();
                            let mode_opt = if let Some(rest) = raw_perm
                                .strip_prefix("0o")
                                .or_else(|| raw_perm.strip_prefix("0O"))
                            {
                                u32::from_str_radix(rest, 8).ok()
                            } else if raw_perm.starts_with('0')
                                && raw_perm.chars().all(|c| ('0'..='7').contains(&c))
                            {
                                u32::from_str_radix(raw_perm, 8).ok()
                            } else {
                                parse_tcl_int(raw_perm).map(|v| v as u32)
                            };
                            if let Some(mode) = mode_opt {
                                #[cfg(unix)]
                                {
                                    use std::os::unix::fs::PermissionsExt;
                                    fs::set_permissions(&p, fs::Permissions::from_mode(mode))
                                        .map_err(|e| TclControl::Error(e.to_string()))?;
                                }
                                #[cfg(not(unix))]
                                {
                                    let mut perms = fs::metadata(&p)
                                        .map_err(|e| TclControl::Error(e.to_string()))?
                                        .permissions();
                                    perms.set_readonly((mode & 0o200) == 0);
                                    fs::set_permissions(&p, perms)
                                        .map_err(|e| TclControl::Error(e.to_string()))?;
                                }
                            }
                        }
                        "-readonly" => {
                            let ro = parse_tcl_bool(&args[i + 1]).unwrap_or(false);
                            let mut perms = fs::metadata(&p)
                                .map_err(|e| TclControl::Error(e.to_string()))?
                                .permissions();
                            perms.set_readonly(ro);
                            fs::set_permissions(&p, perms)
                                .map_err(|e| TclControl::Error(e.to_string()))?;
                        }
                        _ => {}
                    }
                    i += 2;
                }
                Ok(String::new())
            }
            "size" => {
                let p = self.resolve_path(args.get(1).map_or("", String::as_str));
                let sz = fs::metadata(p)
                    .map(|m| m.len())
                    .map_err(|e| TclControl::Error(e.to_string()))?;
                Ok(sz.to_string())
            }
            "delete" => {
                let mut i = 1;
                while i < args.len() && args[i].starts_with('-') {
                    if args[i] == "--" {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                for f in &args[i..] {
                    let p = self.resolve_path(f);
                    if p.is_dir() {
                        let _ = fs::remove_dir_all(&p);
                    } else {
                        let _ = fs::remove_file(&p);
                    }
                }
                Ok(String::new())
            }
            "copy" | "rename" => {
                let mut i = 1;
                while i < args.len() && args[i].starts_with('-') {
                    if args[i] == "--" {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                if i + 2 <= args.len() {
                    let src = self.resolve_path(&args[i]);
                    let dst = self.resolve_path(&args[i + 1]);
                    if args[0] == "copy" {
                        fs::copy(src, dst).map_err(|e| TclControl::Error(e.to_string()))?;
                    } else {
                        fs::rename(src, dst).map_err(|e| TclControl::Error(e.to_string()))?;
                    }
                }
                Ok(String::new())
            }
            "mkdir" => {
                for d in &args[1..] {
                    let p = self.resolve_path(d);
                    fs::create_dir_all(p).map_err(|e| TclControl::Error(e.to_string()))?;
                }
                Ok(String::new())
            }
            "dirname" => {
                let p = Path::new(args.get(1).map_or(".", String::as_str));
                let parent = p
                    .parent()
                    .filter(|pp| !pp.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                Ok(parent.to_string_lossy().into_owned())
            }
            "tail" => {
                let p = Path::new(args.get(1).map_or("", String::as_str));
                Ok(p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default())
            }
            "join" => {
                let mut buf = PathBuf::new();
                for part in &args[1..] {
                    buf.push(part);
                }
                Ok(buf.to_string_lossy().into_owned())
            }
            "normalize" => {
                let p = self.resolve_path(args.get(1).map_or(".", String::as_str));
                Ok(p.to_string_lossy().into_owned())
            }
            "extension" => {
                let p = Path::new(args.get(1).map_or("", String::as_str));
                Ok(p.extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default())
            }
            "rootname" => {
                let s = args.get(1).map_or("", String::as_str);
                if let Some(dot) = s.rfind('.') {
                    Ok(s[..dot].to_string())
                } else {
                    Ok(s.to_string())
                }
            }
            _ => Ok(String::new()),
        }
    }

    fn cmd_glob(&mut self, args: &[String]) -> TclResult<String> {
        let mut nocomplain = false;
        let mut dir = self.cwd.clone();
        let mut tails = false;
        let mut i = 0;
        while i < args.len() && args[i].starts_with('-') {
            match args[i].as_str() {
                "-nocomplain" => nocomplain = true,
                "-tails" => tails = true,
                "-directory" if i + 1 < args.len() => {
                    dir = self.resolve_path(&args[i + 1]);
                    i += 1;
                }
                "--" => {
                    i += 1;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let mut matches = Vec::new();
        for pat in &args[i..] {
            let (search_dir, file_pat, prefix) = if let Some(slash) = pat.rfind('/') {
                let d = self.resolve_path(&pat[..slash]);
                (d, &pat[slash + 1..], Some(&pat[..=slash]))
            } else {
                (dir.clone(), pat.as_str(), None)
            };
            if let Ok(entries) = fs::read_dir(&search_dir) {
                for entry in entries.flatten() {
                    let fname = entry.file_name().to_string_lossy().into_owned();
                    if glob_match(file_pat, &fname, false) {
                        if tails {
                            matches.push(fname);
                        } else if let Some(pfx) = prefix {
                            matches.push(format!("{pfx}{fname}"));
                        } else {
                            matches.push(fname);
                        }
                    }
                }
            }
        }
        matches.sort();
        if matches.is_empty() && !nocomplain {
            return Err(TclControl::Error(
                "no files matched glob pattern".to_string(),
            ));
        }
        Ok(format_list(&matches))
    }

    fn cmd_open(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to open".to_string()));
        }
        let target_arg = &args[0];
        let mode = args.get(1).map_or("r", String::as_str);
        let binary = mode.contains('b');

        if let Some(pipe_cmd) = target_arg.strip_prefix('|') {
            let cmd_words = parse_list(pipe_cmd)
                .ok()
                .filter(|w| !w.is_empty())
                .unwrap_or_else(|| pipe_cmd.split_whitespace().map(str::to_string).collect());
            if cmd_words.is_empty() {
                return Err(TclControl::Error(
                    "illegal must specify command".to_string(),
                ));
            }
            let mut child = Command::new(&cmd_words[0])
                .args(&cmd_words[1..])
                .current_dir(&self.cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| TclControl::Error(e.to_string()))?;
            let stdin = child.stdin.take();
            let stdout = child.stdout.take().map(BufReader::new);
            let id = format!("file{}", self.next_chan_id);
            self.next_chan_id += 1;
            self.channels.insert(
                id.clone(),
                OpenChannel::Pipe {
                    child,
                    stdin,
                    stdout,
                    binary,
                    eof: false,
                },
            );
            return Ok(id);
        }

        let path = self.resolve_path(target_arg);
        let mut opts = fs::OpenOptions::new();
        match mode {
            "r" | "rb" => {
                opts.read(true);
            }
            "r+" | "rb+" => {
                opts.read(true).write(true);
            }
            "w" | "wb" => {
                opts.write(true).create(true).truncate(true);
            }
            "w+" | "wb+" => {
                opts.read(true).write(true).create(true).truncate(true);
            }
            "a" | "ab" | "a+" => {
                opts.read(true).append(true).create(true);
            }
            _ => {
                opts.read(true).write(true).create(true);
            }
        }
        let file = opts
            .open(path)
            .map_err(|e| TclControl::Error(e.to_string()))?;
        let id = format!("file{}", self.next_chan_id);
        self.next_chan_id += 1;
        self.channels.insert(
            id.clone(),
            OpenChannel::File {
                file,
                binary,
                eof: false,
            },
        );
        Ok(id)
    }

    fn cmd_fconfigure(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Ok(String::new());
        }
        let chan_id = &args[0];
        if args.len() == 2 {
            let opt = args[1].as_str();
            if let Some(ch) = self.channels.get(chan_id)
                && (opt == "-translation" || opt == "-encoding")
            {
                return Ok(if ch.is_binary() {
                    "binary".to_string()
                } else if opt == "-translation" {
                    "auto".to_string()
                } else {
                    "utf-8".to_string()
                });
            }
            return Ok(String::new());
        }
        let mut i = 1;
        while i + 1 < args.len() {
            let opt = args[i].as_str();
            let val = args[i + 1].as_str();
            if (opt == "-translation" || opt == "-encoding")
                && let Some(ch) = self.channels.get_mut(chan_id)
            {
                ch.set_binary(val.eq_ignore_ascii_case("binary"));
            }
            i += 2;
        }
        Ok(String::new())
    }

    fn cmd_puts(&mut self, args: &[String]) -> TclResult<String> {
        let mut nonewline = false;
        let mut i = 0;
        if args.first().map(String::as_str) == Some("-nonewline") {
            nonewline = true;
            i += 1;
        }
        let rem = &args[i..];
        let (chan, msg) = match rem.len() {
            1 => ("stdout", rem[0].as_str()),
            2 => (rem[0].as_str(), rem[1].as_str()),
            _ => return Ok(String::new()),
        };
        if chan == "stdout" {
            if self.subprocess_mode {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(msg.as_bytes());
                if !nonewline {
                    let _ = out.write_all(b"\n");
                }
                let _ = out.flush();
            }
            return Ok(String::new());
        }
        if chan == "stderr" {
            if self.subprocess_mode {
                let mut err = std::io::stderr().lock();
                let _ = err.write_all(msg.as_bytes());
                if !nonewline {
                    let _ = err.write_all(b"\n");
                }
                let _ = err.flush();
            }
            return Ok(String::new());
        }
        if let Some(ref host_rc) = self.host
            && crate::harness::has_incrblob_channel(host_rc, chan)
        {
            return crate::harness::incrblob_puts(host_rc, chan, msg, nonewline);
        }
        if let Some(ch) = self.channels.get_mut(chan) {
            let is_bin = ch.is_binary();
            match ch {
                OpenChannel::File { file, .. } => {
                    if is_bin {
                        let raw: Vec<u8> = msg.chars().map(|c| (c as u32 & 0xff) as u8).collect();
                        let _ = file.write_all(&raw);
                    } else {
                        let _ = file.write_all(msg.as_bytes());
                    }
                    if !nonewline {
                        let _ = file.write_all(b"\n");
                    }
                }
                OpenChannel::Pipe {
                    stdin: Some(sin), ..
                } => {
                    if is_bin {
                        let raw: Vec<u8> = msg.chars().map(|c| (c as u32 & 0xff) as u8).collect();
                        let _ = sin.write_all(&raw);
                    } else {
                        let _ = sin.write_all(msg.as_bytes());
                    }
                    if !nonewline {
                        let _ = sin.write_all(b"\n");
                    }
                    let _ = sin.flush();
                }
                OpenChannel::Pipe { stdin: None, .. } => {}
            }
        }
        Ok(String::new())
    }

    fn cmd_flush(&mut self, args: &[String]) -> TclResult<String> {
        let chan = args.first().map_or("stdout", String::as_str);
        if chan == "stdout" {
            if self.subprocess_mode {
                let _ = std::io::stdout().flush();
            }
            return Ok(String::new());
        }
        if chan == "stderr" {
            if self.subprocess_mode {
                let _ = std::io::stderr().flush();
            }
            return Ok(String::new());
        }
        if let Some(ch) = self.channels.get_mut(chan) {
            match ch {
                OpenChannel::File { file, .. } => {
                    let _ = file.flush();
                }
                OpenChannel::Pipe {
                    stdin: Some(sin), ..
                } => {
                    let _ = sin.flush();
                }
                OpenChannel::Pipe { stdin: None, .. } => {}
            }
        }
        Ok(String::new())
    }

    fn cmd_gets(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to gets".to_string()));
        }
        let chan = args[0].as_str();
        let mut content = String::new();
        let mut hit_eof = false;

        if chan == "stdin" {
            if self.subprocess_mode {
                let stdin = std::io::stdin();
                let mut lock = stdin.lock();
                let mut line = String::new();
                match lock.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        self.stdin_eof = true;
                        hit_eof = true;
                    }
                    Ok(_) => {
                        if line.ends_with("\r\n") {
                            line.truncate(line.len() - 2);
                        } else if line.ends_with('\n') {
                            line.pop();
                        }
                        content = line;
                    }
                }
            } else {
                self.stdin_eof = true;
                hit_eof = true;
            }
        } else if let Some(ch) = self.channels.get_mut(chan) {
            match ch {
                OpenChannel::File { file, eof, .. } => {
                    let mut byte = [0u8; 1];
                    let mut buf = Vec::new();
                    let mut read_any = false;
                    while let Ok(1) = file.read(&mut byte) {
                        read_any = true;
                        if byte[0] == b'\n' {
                            break;
                        }
                        if byte[0] != b'\r' {
                            buf.push(byte[0]);
                        }
                    }
                    if !read_any {
                        *eof = true;
                        hit_eof = true;
                    } else {
                        content = String::from_utf8_lossy(&buf).into_owned();
                    }
                }
                OpenChannel::Pipe {
                    stdout: Some(sout),
                    eof,
                    ..
                } => {
                    let mut line = String::new();
                    match sout.read_line(&mut line) {
                        Ok(0) | Err(_) => {
                            *eof = true;
                            hit_eof = true;
                        }
                        Ok(_) => {
                            if line.ends_with("\r\n") {
                                line.truncate(line.len() - 2);
                            } else if line.ends_with('\n') {
                                line.pop();
                            }
                            content = line;
                        }
                    }
                }
                OpenChannel::Pipe {
                    stdout: None, eof, ..
                } => {
                    *eof = true;
                    hit_eof = true;
                }
            }
        }

        if let Some(var) = args.get(1) {
            if hit_eof {
                self.set_var(var, String::new())
                    .map_err(TclControl::Error)?;
                Ok("-1".to_string())
            } else {
                let len = content.chars().count();
                self.set_var(var, content).map_err(TclControl::Error)?;
                Ok(len.to_string())
            }
        } else {
            Ok(content)
        }
    }

    fn cmd_eof(&mut self, args: &[String]) -> TclResult<String> {
        let Some(chan) = args.first().map(String::as_str) else {
            return Err(TclControl::Error("wrong # args to eof".to_string()));
        };
        if chan == "stdin" {
            return Ok(if self.stdin_eof { "1" } else { "0" }.to_string());
        }
        if let Some(ref host_rc) = self.host
            && crate::harness::has_incrblob_channel(host_rc, chan)
        {
            return Ok(if crate::harness::incrblob_eof(host_rc, chan) {
                "1"
            } else {
                "0"
            }
            .to_string());
        }
        if let Some(ch) = self.channels.get(chan) {
            return Ok(if ch.is_eof() { "1" } else { "0" }.to_string());
        }
        Ok("0".to_string())
    }

    fn cmd_read(&mut self, args: &[String]) -> TclResult<String> {
        let mut i = 0;
        let mut nonewline = false;
        if args.first().map(String::as_str) == Some("-nonewline") {
            nonewline = true;
            i += 1;
        }
        let Some(chan_id) = args.get(i) else {
            return Err(TclControl::Error("wrong # args to read".to_string()));
        };
        if let Some(ref host_rc) = self.host
            && crate::harness::has_incrblob_channel(host_rc, chan_id)
        {
            return crate::harness::incrblob_read(
                host_rc,
                chan_id,
                args.get(i + 1).map(String::as_str),
                nonewline,
            );
        }
        let mut buf = Vec::new();
        let mut is_binary = false;
        if let Some(ch) = self.channels.get_mut(chan_id) {
            is_binary = ch.is_binary();
            match ch {
                OpenChannel::File { file, eof, .. } => {
                    if let Some(n_str) = args.get(i + 1) {
                        let n = parse_tcl_int(n_str).unwrap_or(0).max(0) as usize;
                        buf.resize(n, 0);
                        let k = file.read(&mut buf).unwrap_or(0);
                        if k < n {
                            *eof = true;
                        }
                        buf.truncate(k);
                    } else {
                        let _ = file.read_to_end(&mut buf);
                        *eof = true;
                    }
                }
                OpenChannel::Pipe {
                    stdout: Some(sout),
                    eof,
                    ..
                } => {
                    if let Some(n_str) = args.get(i + 1) {
                        let n = parse_tcl_int(n_str).unwrap_or(0).max(0) as usize;
                        buf.resize(n, 0);
                        let k = sout.read(&mut buf).unwrap_or(0);
                        if k < n {
                            *eof = true;
                        }
                        buf.truncate(k);
                    } else {
                        let _ = sout.read_to_end(&mut buf);
                        *eof = true;
                    }
                }
                OpenChannel::Pipe {
                    stdout: None, eof, ..
                } => {
                    *eof = true;
                }
            }
        }
        let mut s = if is_binary {
            buf.into_iter().map(char::from).collect()
        } else {
            String::from_utf8_lossy(&buf).into_owned()
        };
        if nonewline && s.ends_with('\n') {
            s.pop();
        }
        Ok(s)
    }

    fn cmd_close(&mut self, args: &[String]) -> TclResult<String> {
        if let Some(id) = args.first() {
            if let Some(ref host_rc) = self.host
                && crate::harness::has_incrblob_channel(host_rc, id)
            {
                return crate::harness::incrblob_close(host_rc, id);
            }
            if let Some(mut ch) = self.channels.remove(id) {
                ch.close_bounded();
            }
        }
        Ok(String::new())
    }

    fn cmd_seek(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() >= 2 {
            if let Some(ref host_rc) = self.host
                && crate::harness::has_incrblob_channel(host_rc, &args[0])
            {
                return crate::harness::incrblob_seek(host_rc, &args[0], &args[1..]);
            }
            if let Some(OpenChannel::File { file, eof, .. }) = self.channels.get_mut(&args[0]) {
                let off = parse_tcl_int(&args[1]).unwrap_or(0);
                let pos = match args.get(2).map_or("start", String::as_str) {
                    "current" => SeekFrom::Current(off),
                    "end" => SeekFrom::End(off),
                    _ => SeekFrom::Start(off.max(0) as u64),
                };
                let _ = file.seek(pos);
                *eof = false;
            }
        }
        Ok(String::new())
    }

    fn cmd_tell(&mut self, args: &[String]) -> TclResult<String> {
        if let Some(id) = args.first() {
            if let Some(ref host_rc) = self.host
                && crate::harness::has_incrblob_channel(host_rc, id)
            {
                return crate::harness::incrblob_tell(host_rc, id);
            }
            if let Some(OpenChannel::File { file, .. }) = self.channels.get_mut(id)
                && let Ok(pos) = file.stream_position()
            {
                return Ok(pos.to_string());
            }
        }
        Ok("0".to_string())
    }

    fn cmd_after(&mut self, args: &[String]) -> TclResult<String> {
        if args.len() == 1
            && let Some(ms) = parse_tcl_int(&args[0])
            && ms > 0
        {
            std::thread::sleep(Duration::from_millis((ms as u64).min(100)));
        }
        Ok(String::new())
    }

    fn cmd_exec(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to exec".to_string()));
        }
        if args.last().map(String::as_str) == Some("&") {
            let cmd_args = &args[..args.len() - 1];
            if cmd_args.is_empty() {
                return Err(TclControl::Error("wrong # args to exec".to_string()));
            }
            let child = Command::new(&cmd_args[0])
                .args(&cmd_args[1..])
                .current_dir(&self.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| TclControl::Error(e.to_string()))?;
            let pid = child.id().to_string();
            self.bg_children.push(child);
            return Ok(pid);
        }
        let output = Command::new(&args[0])
            .args(&args[1..])
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| TclControl::Error(e.to_string()))?;
        if !output.status.success() {
            let err_str = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let out_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let msg = if !err_str.is_empty() {
                err_str
            } else if !out_str.is_empty() {
                out_str
            } else {
                "child process exited abnormally".to_string()
            };
            return Err(TclControl::Error(msg));
        }
        let mut out_str = String::from_utf8_lossy(&output.stdout).into_owned();
        if out_str.ends_with("\r\n") {
            out_str.truncate(out_str.len() - 2);
        } else if out_str.ends_with('\n') {
            out_str.pop();
        }
        Ok(out_str)
    }

    fn cmd_exit(&mut self, args: &[String]) -> TclResult<String> {
        let code = args.first().and_then(|s| parse_tcl_int(s)).unwrap_or(0) as i32;
        if self.subprocess_mode {
            self.channels.clear();
            self.kill_bg_children();
            if let Some(ref host_rc) = self.host {
                crate::harness::close_all_host_connections(host_rc);
            }
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(code);
        }
        Err(TclControl::FinishTest)
    }

    fn cmd_time(&mut self, args: &[String]) -> TclResult<String> {
        if args.is_empty() {
            return Err(TclControl::Error("wrong # args to time".to_string()));
        }
        let count = args
            .get(1)
            .and_then(|s| parse_tcl_int(s))
            .unwrap_or(1)
            .max(1);
        let t0 = Instant::now();
        for _ in 0..count {
            self.eval(&args[0])?;
        }
        let micros = t0.elapsed().as_micros() as u64 / (count as u64);
        Ok(format!("{micros} microseconds per iteration"))
    }

    fn cmd_clock(&mut self, args: &[String]) -> TclResult<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        match args.first().map_or("seconds", String::as_str) {
            "seconds" => Ok(now.as_secs().to_string()),
            "milliseconds" => Ok(now.as_millis().to_string()),
            "microseconds" | "clicks" => Ok(now.as_micros().to_string()),
            _ => Ok("0".to_string()),
        }
    }

    fn cmd_namespace(&mut self, args: &[String]) -> TclResult<String> {
        match args.first().map_or("", String::as_str) {
            "eval" if args.len() >= 3 => {
                let script = args[2..].join(" ");
                self.eval(&script)
            }
            "current" => Ok("::".to_string()),
            _ => Ok(String::new()),
        }
    }
}

/// Finds the matching `]` for `[` at `src[start_idx]`, honoring nested braces, quotes, and brackets.
fn find_matching_bracket(src: &str, start_idx: usize) -> Result<usize, String> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = start_idx + 1;
    let mut cmd_start = true;

    while i < len {
        // Skip command whitespace
        while i < len && (bytes[i] == b' ' || bytes[i] == b'\t' || bytes[i] == b'\r') {
            i += 1;
        }
        if i >= len {
            break;
        }
        if bytes[i] == b']' {
            return Ok(i);
        }
        if bytes[i] == b'\n' || bytes[i] == b';' {
            cmd_start = true;
            i += 1;
            continue;
        }
        if cmd_start && bytes[i] == b'#' {
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'\n' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            cmd_start = true;
            continue;
        }
        cmd_start = false;

        if bytes[i] == b'{' {
            let mut depth = 1usize;
            i += 1;
            while i < len && depth > 0 {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'{' {
                    depth += 1;
                } else if bytes[i] == b'}' {
                    depth -= 1;
                }
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            while i < len && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'[' {
                    i = find_matching_bracket(src, i)? + 1;
                } else {
                    i += 1;
                }
            }
            if i < len {
                i += 1;
            }
        } else {
            while i < len && !is_tcl_list_space(bytes[i]) && bytes[i] != b';' && bytes[i] != b']' {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'[' {
                    i = find_matching_bracket(src, i)? + 1;
                } else {
                    i += 1;
                }
            }
        }
    }

    Err("missing close-bracket".to_string())
}

/// Parses TCL index expressions (`0`, `end`, `end-1`, `end+1`, `2+1`, `5-2`).
pub fn parse_tcl_index(spec: &str, len: usize) -> Result<i64, String> {
    let t = spec.trim();
    if t == "end" {
        return Ok((len as i64) - 1);
    }
    if let Some(rest) = t.strip_prefix("end-") {
        let off = parse_tcl_int(rest).ok_or_else(|| format!("bad index \"{spec}\""))?;
        return Ok((len as i64) - 1 - off);
    }
    if let Some(rest) = t.strip_prefix("end+") {
        let off = parse_tcl_int(rest).ok_or_else(|| format!("bad index \"{spec}\""))?;
        return Ok((len as i64) - 1 + off);
    }
    if let Some(i) = parse_tcl_int(t) {
        return Ok(i);
    }
    if let Some(plus) = t.get(1..).and_then(|s| s.find('+')).map(|p| p + 1) {
        let a = parse_tcl_int(&t[..plus]).ok_or_else(|| format!("bad index \"{spec}\""))?;
        let b = parse_tcl_int(&t[plus + 1..]).ok_or_else(|| format!("bad index \"{spec}\""))?;
        return Ok(a + b);
    }
    if let Some(minus) = t.get(1..).and_then(|s| s.find('-')).map(|p| p + 1) {
        let a = parse_tcl_int(&t[..minus]).ok_or_else(|| format!("bad index \"{spec}\""))?;
        let b = parse_tcl_int(&t[minus + 1..]).ok_or_else(|| format!("bad index \"{spec}\""))?;
        return Ok(a - b);
    }
    Err(format!("bad index \"{spec}\""))
}

/// Implements TCL `[format fmt arg1 arg2 ...]`.
fn tcl_format(fmt: &str, args: &[String]) -> Result<String, String> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0usize;
    let mut arg_idx = 0usize;
    let mut out = String::with_capacity(fmt.len() + 16);

    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i < chars.len() && chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }

        let mut left_align = false;
        let mut zero_pad = false;
        let mut plus_sign = false;
        let mut space_sign = false;
        while i < chars.len() {
            match chars[i] {
                '-' => left_align = true,
                '0' => zero_pad = true,
                '+' => plus_sign = true,
                ' ' => space_sign = true,
                '#' => {}
                _ => break,
            }
            i += 1;
        }

        let width: Option<usize> = if i < chars.len() && chars[i] == '*' {
            i += 1;
            let w = args
                .get(arg_idx)
                .and_then(|s| parse_tcl_int(s.trim()))
                .unwrap_or(0);
            arg_idx += 1;
            if w < 0 {
                left_align = true;
            }
            Some(w.unsigned_abs() as usize)
        } else {
            let ws = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i > ws {
                chars[ws..i].iter().collect::<String>().parse().ok()
            } else {
                None
            }
        };

        let prec: Option<usize> = if i < chars.len() && chars[i] == '.' {
            i += 1;
            if i < chars.len() && chars[i] == '*' {
                i += 1;
                let p = args
                    .get(arg_idx)
                    .and_then(|s| parse_tcl_int(s.trim()))
                    .unwrap_or(0)
                    .max(0) as usize;
                arg_idx += 1;
                Some(p)
            } else {
                let ps = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                Some(chars[ps..i].iter().collect::<String>().parse().unwrap_or(0))
            }
        } else {
            None
        };

        // Skip length modifiers `l`, `ll`, `w`, `h`
        while i < chars.len() && matches!(chars[i], 'l' | 'w' | 'h') {
            i += 1;
        }
        if i >= chars.len() {
            return Err("format string ended in middle of field specifier".to_string());
        }
        let spec = chars[i];
        i += 1;

        let raw_arg = args.get(arg_idx).map_or("", String::as_str);
        arg_idx += 1;

        let formatted = match spec {
            's' => {
                if let Some(p) = prec {
                    raw_arg.chars().take(p).collect::<String>()
                } else {
                    raw_arg.to_string()
                }
            }
            'c' => {
                let code = parse_tcl_int(raw_arg.trim()).unwrap_or(0) as u32;
                char::from_u32(code).unwrap_or('\0').to_string()
            }
            'd' | 'i' => {
                let v = parse_tcl_int(raw_arg.trim()).unwrap_or(0);
                let mut digits = v.unsigned_abs().to_string();
                if let Some(p) = prec {
                    while digits.len() < p {
                        digits.insert(0, '0');
                    }
                }
                if v < 0 {
                    format!("-{digits}")
                } else if plus_sign {
                    format!("+{digits}")
                } else if space_sign {
                    format!(" {digits}")
                } else {
                    digits
                }
            }
            'u' => {
                let v = parse_tcl_int(raw_arg.trim()).unwrap_or(0) as u64;
                v.to_string()
            }
            'x' | 'X' => {
                let v = parse_tcl_int(raw_arg.trim()).unwrap_or(0) as u64;
                let mut s = if spec == 'x' {
                    format!("{v:x}")
                } else {
                    format!("{v:X}")
                };
                if let Some(p) = prec {
                    while s.len() < p {
                        s.insert(0, '0');
                    }
                }
                s
            }
            'o' => {
                let v = parse_tcl_int(raw_arg.trim()).unwrap_or(0) as u64;
                format!("{v:o}")
            }
            'f' | 'F' => {
                let v = parse_tcl_float(raw_arg.trim()).unwrap_or(0.0);
                let p = prec.unwrap_or(6);
                format!("{v:.p$}")
            }
            'e' | 'E' => {
                let v = parse_tcl_float(raw_arg.trim()).unwrap_or(0.0);
                let p = prec.unwrap_or(6);
                if spec == 'e' {
                    format!("{v:.p$e}")
                } else {
                    format!("{v:.p$E}")
                }
            }
            'g' | 'G' => {
                let v = parse_tcl_float(raw_arg.trim()).unwrap_or(0.0);
                sqliters_types::format_sqlite_real(v)
            }
            other => other.to_string(),
        };

        if let Some(w) = width {
            let char_len = formatted.chars().count();
            if char_len < w {
                let pad_len = w - char_len;
                if left_align {
                    out.push_str(&formatted);
                    for _ in 0..pad_len {
                        out.push(' ');
                    }
                } else if zero_pad
                    && matches!(spec, 'd' | 'i' | 'u' | 'x' | 'X' | 'o' | 'f' | 'e' | 'g')
                {
                    if let Some(rest) = formatted.strip_prefix('-') {
                        out.push('-');
                        for _ in 0..pad_len {
                            out.push('0');
                        }
                        out.push_str(rest);
                    } else {
                        for _ in 0..pad_len {
                            out.push('0');
                        }
                        out.push_str(&formatted);
                    }
                } else {
                    for _ in 0..pad_len {
                        out.push(' ');
                    }
                    out.push_str(&formatted);
                }
            } else {
                out.push_str(&formatted);
            }
        } else {
            out.push_str(&formatted);
        }
    }

    Ok(out)
}

#[derive(Clone, Copy)]
enum BinCount {
    DefaultOne,
    Exact(usize),
    Star,
}

fn parse_bin_spec(chars: &[char], idx: &mut usize) -> Option<(char, bool, BinCount)> {
    while *idx < chars.len() && chars[*idx].is_ascii_whitespace() {
        *idx += 1;
    }
    if *idx >= chars.len() {
        return None;
    }
    let cmd = chars[*idx];
    *idx += 1;
    let mut unsigned = false;
    if *idx < chars.len() && chars[*idx] == 'u' {
        unsigned = true;
        *idx += 1;
    }
    let count = if *idx < chars.len() && chars[*idx] == '*' {
        *idx += 1;
        BinCount::Star
    } else if *idx < chars.len() && chars[*idx].is_ascii_digit() {
        let start = *idx;
        while *idx < chars.len() && chars[*idx].is_ascii_digit() {
            *idx += 1;
        }
        let s: String = chars[start..*idx].iter().collect();
        BinCount::Exact(s.parse::<usize>().unwrap_or(0))
    } else {
        BinCount::DefaultOne
    };
    Some((cmd, unsigned, count))
}

fn tcl_binary_format(fmt: &str, vals: &[String]) -> TclResult<Vec<u8>> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut idx = 0usize;
    let mut arg_idx = 0usize;
    let mut out: Vec<u8> = Vec::new();
    let mut cursor = 0usize;

    let write_bytes = |out: &mut Vec<u8>, cursor: &mut usize, bytes: &[u8]| {
        if *cursor + bytes.len() > out.len() {
            out.resize(*cursor + bytes.len(), 0);
        }
        out[*cursor..*cursor + bytes.len()].copy_from_slice(bytes);
        *cursor += bytes.len();
    };

    while let Some((cmd, _unsigned, count)) = parse_bin_spec(&chars, &mut idx) {
        match cmd {
            'x' => {
                let n = match count {
                    BinCount::DefaultOne => 1,
                    BinCount::Exact(n) => n,
                    BinCount::Star => 0,
                };
                let zeros = vec![0u8; n];
                write_bytes(&mut out, &mut cursor, &zeros);
            }
            '@' => {
                cursor = match count {
                    BinCount::Star => out.len(),
                    BinCount::Exact(n) => n,
                    BinCount::DefaultOne => 0,
                };
                if cursor > out.len() {
                    out.resize(cursor, 0);
                }
            }
            'a' | 'A' => {
                let Some(val) = vals.get(arg_idx) else {
                    return Err(TclControl::Error(
                        "not enough arguments for all format specifiers".to_string(),
                    ));
                };
                arg_idx += 1;
                let raw: Vec<u8> = val.chars().map(|c| (c as u32 & 0xff) as u8).collect();
                let pad = if cmd == 'A' { b' ' } else { 0u8 };
                match count {
                    BinCount::Star => write_bytes(&mut out, &mut cursor, &raw),
                    BinCount::DefaultOne => {
                        let b = raw.first().copied().unwrap_or(pad);
                        write_bytes(&mut out, &mut cursor, &[b]);
                    }
                    BinCount::Exact(n) => {
                        let mut buf = vec![pad; n];
                        let copy_len = n.min(raw.len());
                        buf[..copy_len].copy_from_slice(&raw[..copy_len]);
                        write_bytes(&mut out, &mut cursor, &buf);
                    }
                }
            }
            'H' | 'h' => {
                let Some(val) = vals.get(arg_idx) else {
                    return Err(TclControl::Error(
                        "not enough arguments for all format specifiers".to_string(),
                    ));
                };
                arg_idx += 1;
                let hex_chars: Vec<char> = val.chars().filter(|c| !c.is_whitespace()).collect();
                let n_nibbles = match count {
                    BinCount::Star => hex_chars.len(),
                    BinCount::DefaultOne => 1,
                    BinCount::Exact(n) => n,
                };
                let mut bytes = Vec::with_capacity(n_nibbles.div_ceil(2));
                let mut j = 0usize;
                while j < n_nibbles {
                    let d1 = hex_chars.get(j).and_then(|c| c.to_digit(16)).unwrap_or(0) as u8;
                    let d2 = if j + 1 < n_nibbles {
                        hex_chars
                            .get(j + 1)
                            .and_then(|c| c.to_digit(16))
                            .unwrap_or(0) as u8
                    } else {
                        0u8
                    };
                    let b = if cmd == 'H' {
                        (d1 << 4) | d2
                    } else {
                        d1 | (d2 << 4)
                    };
                    bytes.push(b);
                    j += 2;
                }
                write_bytes(&mut out, &mut cursor, &bytes);
            }
            'c' | 'C' | 's' | 'S' | 'i' | 'I' | 'w' | 'W' => {
                let Some(val) = vals.get(arg_idx) else {
                    return Err(TclControl::Error(
                        "not enough arguments for all format specifiers".to_string(),
                    ));
                };
                arg_idx += 1;
                let nums: Vec<i64> = match count {
                    BinCount::DefaultOne => {
                        let n = parse_tcl_int(val.trim()).or_else(|| {
                            parse_list(val)
                                .ok()
                                .and_then(|l| l.first().and_then(|s| parse_tcl_int(s.trim())))
                        });
                        vec![n.ok_or_else(|| {
                            TclControl::Error(format!("expected integer but got \"{val}\""))
                        })?]
                    }
                    BinCount::Star => {
                        let items = parse_list(val).map_err(TclControl::Error)?;
                        let mut parsed = Vec::with_capacity(items.len());
                        for item in items {
                            let n = parse_tcl_int(item.trim()).ok_or_else(|| {
                                TclControl::Error(format!("expected integer but got \"{item}\""))
                            })?;
                            parsed.push(n);
                        }
                        parsed
                    }
                    BinCount::Exact(n_req) => {
                        let items = parse_list(val).map_err(TclControl::Error)?;
                        if items.len() < n_req {
                            return Err(TclControl::Error(
                                "number of elements in list does not match count".to_string(),
                            ));
                        }
                        let mut parsed = Vec::with_capacity(n_req);
                        for item in items.into_iter().take(n_req) {
                            let n = parse_tcl_int(item.trim()).ok_or_else(|| {
                                TclControl::Error(format!("expected integer but got \"{item}\""))
                            })?;
                            parsed.push(n);
                        }
                        parsed
                    }
                };
                for v in nums {
                    match cmd {
                        'c' | 'C' => write_bytes(&mut out, &mut cursor, &[(v & 0xff) as u8]),
                        's' => write_bytes(&mut out, &mut cursor, &(v as i16).to_le_bytes()),
                        'S' => write_bytes(&mut out, &mut cursor, &(v as i16).to_be_bytes()),
                        'i' => write_bytes(&mut out, &mut cursor, &(v as i32).to_le_bytes()),
                        'I' => write_bytes(&mut out, &mut cursor, &(v as i32).to_be_bytes()),
                        'w' => write_bytes(&mut out, &mut cursor, &v.to_le_bytes()),
                        'W' => write_bytes(&mut out, &mut cursor, &v.to_be_bytes()),
                        _ => unreachable!(),
                    }
                }
            }
            'f' | 'r' | 'R' | 'd' | 'q' | 'Q' => {
                let Some(val) = vals.get(arg_idx) else {
                    return Err(TclControl::Error(
                        "not enough arguments for all format specifiers".to_string(),
                    ));
                };
                arg_idx += 1;
                let parse_f = |s: &str| -> TclResult<f64> {
                    s.trim().parse::<f64>().map_err(|_| {
                        TclControl::Error(format!("expected floating-point number but got \"{s}\""))
                    })
                };
                let nums: Vec<f64> = match count {
                    BinCount::DefaultOne => {
                        let f = parse_f(val).or_else(|e| {
                            parse_list(val)
                                .ok()
                                .and_then(|l| l.first().cloned())
                                .map_or(Err(e), |s| parse_f(&s))
                        })?;
                        vec![f]
                    }
                    BinCount::Star => {
                        let items = parse_list(val).map_err(TclControl::Error)?;
                        let mut parsed = Vec::with_capacity(items.len());
                        for item in items {
                            parsed.push(parse_f(&item)?);
                        }
                        parsed
                    }
                    BinCount::Exact(n_req) => {
                        let items = parse_list(val).map_err(TclControl::Error)?;
                        if items.len() < n_req {
                            return Err(TclControl::Error(
                                "number of elements in list does not match count".to_string(),
                            ));
                        }
                        let mut parsed = Vec::with_capacity(n_req);
                        for item in items.into_iter().take(n_req) {
                            parsed.push(parse_f(&item)?);
                        }
                        parsed
                    }
                };
                for v in nums {
                    match cmd {
                        'f' | 'r' => write_bytes(&mut out, &mut cursor, &(v as f32).to_le_bytes()),
                        'R' => write_bytes(&mut out, &mut cursor, &(v as f32).to_be_bytes()),
                        'd' | 'q' => write_bytes(&mut out, &mut cursor, &v.to_le_bytes()),
                        'Q' => write_bytes(&mut out, &mut cursor, &v.to_be_bytes()),
                        _ => unreachable!(),
                    }
                }
            }
            other => {
                return Err(TclControl::Error(format!(
                    "bad field specifier \"{other}\" in binary format"
                )));
            }
        }
    }
    Ok(out)
}

fn tcl_binary_scan(data: &[u8], fmt: &str, max_vars: usize) -> TclResult<Vec<Option<String>>> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut idx = 0usize;
    let mut pos = 0usize;
    let mut out: Vec<Option<String>> = Vec::new();

    while let Some((cmd, unsigned, count)) = parse_bin_spec(&chars, &mut idx) {
        match cmd {
            'x' => {
                let n = match count {
                    BinCount::DefaultOne => 1,
                    BinCount::Exact(n) => n,
                    BinCount::Star => data.len().saturating_sub(pos),
                };
                if pos + n > data.len() {
                    break;
                }
                pos += n;
            }
            '@' => {
                let next_pos = match count {
                    BinCount::Star => data.len(),
                    BinCount::Exact(n) => n,
                    BinCount::DefaultOne => 0,
                };
                if next_pos > data.len() {
                    break;
                }
                pos = next_pos;
            }
            'a' | 'A' => {
                if out.len() >= max_vars {
                    return Err(TclControl::Error(
                        "not enough arguments for all format specifiers".to_string(),
                    ));
                }
                let n = match count {
                    BinCount::DefaultOne => 1,
                    BinCount::Exact(n) => n,
                    BinCount::Star => data.len().saturating_sub(pos),
                };
                if pos + n > data.len() {
                    break;
                }
                let mut slice = &data[pos..pos + n];
                pos += n;
                if cmd == 'A' {
                    while let Some(&last) = slice.last() {
                        if last == b' ' || last == 0 {
                            slice = &slice[..slice.len() - 1];
                        } else {
                            break;
                        }
                    }
                }
                let s: String = slice.iter().map(|&b| char::from(b)).collect();
                out.push(Some(s));
            }
            'H' | 'h' => {
                if out.len() >= max_vars {
                    return Err(TclControl::Error(
                        "not enough arguments for all format specifiers".to_string(),
                    ));
                }
                let n_nibbles = match count {
                    BinCount::DefaultOne => 1,
                    BinCount::Exact(n) => n,
                    BinCount::Star => data.len().saturating_sub(pos) * 2,
                };
                let bytes_needed = n_nibbles.div_ceil(2);
                if pos + bytes_needed > data.len() {
                    break;
                }
                let mut hex = String::with_capacity(n_nibbles);
                for i in 0..n_nibbles {
                    let b = data[pos + i / 2];
                    let nib = if cmd == 'H' {
                        if i % 2 == 0 { b >> 4 } else { b & 0x0f }
                    } else if i % 2 == 0 {
                        b & 0x0f
                    } else {
                        b >> 4
                    };
                    hex.push(char::from_digit(u32::from(nib), 16).unwrap_or('0'));
                }
                pos += bytes_needed;
                out.push(Some(hex));
            }
            'c' | 'C' | 's' | 'S' | 'i' | 'I' | 'w' | 'W' | 'f' | 'r' | 'R' | 'd' | 'q' | 'Q' => {
                if out.len() >= max_vars {
                    return Err(TclControl::Error(
                        "not enough arguments for all format specifiers".to_string(),
                    ));
                }
                let elem_size = match cmd {
                    'c' | 'C' => 1usize,
                    's' | 'S' => 2usize,
                    'i' | 'I' | 'f' | 'r' | 'R' => 4usize,
                    _ => 8usize,
                };
                let is_unsigned = unsigned || cmd == 'C';
                let decode_one = |chunk: &[u8]| -> String {
                    match cmd {
                        'c' | 'C' => {
                            if is_unsigned {
                                chunk[0].to_string()
                            } else {
                                (chunk[0] as i8).to_string()
                            }
                        }
                        's' => {
                            let u = u16::from_le_bytes([chunk[0], chunk[1]]);
                            if is_unsigned {
                                u.to_string()
                            } else {
                                (u as i16).to_string()
                            }
                        }
                        'S' => {
                            let u = u16::from_be_bytes([chunk[0], chunk[1]]);
                            if is_unsigned {
                                u.to_string()
                            } else {
                                (u as i16).to_string()
                            }
                        }
                        'i' => {
                            let u = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            if is_unsigned {
                                u.to_string()
                            } else {
                                (u as i32).to_string()
                            }
                        }
                        'I' => {
                            let u = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            if is_unsigned {
                                u.to_string()
                            } else {
                                (u as i32).to_string()
                            }
                        }
                        'w' => {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(chunk);
                            let u = u64::from_le_bytes(b);
                            if is_unsigned {
                                u.to_string()
                            } else {
                                (u as i64).to_string()
                            }
                        }
                        'W' => {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(chunk);
                            let u = u64::from_be_bytes(b);
                            if is_unsigned {
                                u.to_string()
                            } else {
                                (u as i64).to_string()
                            }
                        }
                        'f' | 'r' => {
                            let v = f64::from(f32::from_le_bytes([
                                chunk[0], chunk[1], chunk[2], chunk[3],
                            ]));
                            crate::expr::ExprVal::Float(v).to_tcl_string()
                        }
                        'R' => {
                            let v = f64::from(f32::from_be_bytes([
                                chunk[0], chunk[1], chunk[2], chunk[3],
                            ]));
                            crate::expr::ExprVal::Float(v).to_tcl_string()
                        }
                        'd' | 'q' => {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(chunk);
                            crate::expr::ExprVal::Float(f64::from_le_bytes(b)).to_tcl_string()
                        }
                        'Q' => {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(chunk);
                            crate::expr::ExprVal::Float(f64::from_be_bytes(b)).to_tcl_string()
                        }
                        _ => unreachable!(),
                    }
                };
                match count {
                    BinCount::DefaultOne => {
                        if pos + elem_size > data.len() {
                            break;
                        }
                        let val_str = decode_one(&data[pos..pos + elem_size]);
                        pos += elem_size;
                        out.push(Some(val_str));
                    }
                    BinCount::Exact(n) => {
                        if pos + n * elem_size > data.len() {
                            break;
                        }
                        let mut vals = Vec::with_capacity(n);
                        for _ in 0..n {
                            vals.push(decode_one(&data[pos..pos + elem_size]));
                            pos += elem_size;
                        }
                        out.push(Some(format_list(&vals)));
                    }
                    BinCount::Star => {
                        let n = data.len().saturating_sub(pos) / elem_size;
                        let mut vals = Vec::with_capacity(n);
                        for _ in 0..n {
                            vals.push(decode_one(&data[pos..pos + elem_size]));
                            pos += elem_size;
                        }
                        out.push(Some(format_list(&vals)));
                    }
                }
            }
            other => {
                return Err(TclControl::Error(format!(
                    "bad field specifier \"{other}\" in binary scan"
                )));
            }
        }
    }

    Ok(out)
}

static TESTFIXTURE_BIN_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn resolve_testfixture_binary() -> PathBuf {
    TESTFIXTURE_BIN_PATH
        .get_or_init(|| {
            let bin_name = if cfg!(windows) {
                "testfixture.exe"
            } else {
                "testfixture"
            };
            if let Ok(exe) = std::env::current_exe() {
                if exe.file_name().is_some_and(|n| n == bin_name) && exe.exists() {
                    return exe;
                }
                if let Some(parent) = exe.parent() {
                    let profile_dir = if parent.file_name().is_some_and(|n| n == "deps") {
                        parent.parent().unwrap_or(parent)
                    } else {
                        parent
                    };
                    let candidate = profile_dir.join(bin_name);
                    let is_fresh = std::fs::metadata(&candidate)
                        .and_then(|cm| Ok((cm.modified()?, std::fs::metadata(&exe)?.modified()?)))
                        .is_ok_and(|(ct, et)| ct >= et);
                    if is_fresh {
                        return candidate;
                    }
                    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
                    let manifest_dir =
                        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
                    let _ = Command::new(cargo)
                        .current_dir(manifest_dir)
                        .args([
                            "build",
                            "--quiet",
                            "-p",
                            "sqliters-tcltest",
                            "--bin",
                            "testfixture",
                        ])
                        .status();
                    if candidate.exists() {
                        return candidate;
                    }
                }
            }
            PathBuf::from(bin_name)
        })
        .clone()
}
