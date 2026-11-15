//! Pure safe Rust implementation of SQLite's `fts5` and `fts5vocab` full-text search
//! virtual table modules (`sqlite/ext/fts5/`).
#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};

use crate::fts5_index::{
    Detail as IndexDetail, FTS5_CORRUPT, FTS5_CURRENT_VERSION, Fts5Index, IndexConfig,
    get_varint32 as fts5_get_varint32,
    put_varint as fts5_put_varint, rc_only,
};
use crate::fts5_tokenize::{FTS5_MAX_TOKEN_SIZE, Tokenizer, TokenizerError, TokenizerPattern};
use sqliters_types::ShadowHint;
use sqliters_types::real_to_text;
use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConflictMode, VTabConstraintOp,
    VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor,
    VirtualTable, VirtualTableModule,
};

const FTS5_MAX_EXPR_DEPTH: usize = 256;
const FTS5_SOURCE_ID: &str =
    "fts5: 2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76";

const SHADOW_SUFFIXES: &[&str] = &["data", "idx", "content", "docsize", "config"];

// ============================================================================
// Quote Helpers & SQLite FTS5 Varint Codec (`fts5_varint.c`)
// ============================================================================

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn quote_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `sqlite3_result_double`: NaN becomes NULL.
fn real_result(r: f64) -> Value {
    if r.is_nan() {
        Value::Null
    } else {
        Value::real(r)
    }
}

/// The tokenizer of one FTS5 table (`Fts5Config.t`): a C-exact built-in tokenizer plus the
/// conversions the rest of this module needs.
#[derive(Debug, Clone)]
struct TokenizerSpec {
    tok: Tokenizer,
}

impl Default for TokenizerSpec {
    fn default() -> Self {
        Self::load(&[]).expect("default unicode61 tokenizer")
    }
}

impl TokenizerSpec {
    /// `sqlite3Fts5LoadTokenizer`: `args` is the dequoted word list of the `tokenize=` option
    /// (empty = default `unicode61`).
    fn load(args: &[String]) -> Result<Self> {
        match Tokenizer::load(args) {
            Ok(tok) => Ok(Self { tok }),
            Err(TokenizerError::NoSuch(name)) => Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such tokenizer: {name}"),
            )),
            Err(TokenizerError::Constructor) => Err(SqliteError::new(
                ErrorCode::Error,
                "error in tokenizer constructor",
            )),
        }
    }

    /// Invokes `cb` with the bytes of every token of a document column, truncated like
    /// `fts5StorageInsertCallback` does.
    fn for_each_doc_token(&self, text: &[u8], mut cb: impl FnMut(&[u8])) {
        self.tok.tokenize(text, &mut |tok, _, _| {
            cb(&tok[..tok.len().min(FTS5_MAX_TOKEN_SIZE)]);
        });
    }
}

// ============================================================================
// FTS5 Table Schema & Options
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailMode {
    Full,
    Col,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ContentMode {
    Normal,
    Contentless,
    External {
        table: String,
        rowid_col: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct Fts5Config {
    columns: Vec<String>,
    unindexed: Vec<bool>,
    tokenizer: TokenizerSpec,
    prefixes: Vec<usize>,
    content_mode: ContentMode,
    columnsize: bool,
    detail: DetailMode,
    contentless_delete: bool,
    /// Contentless table that still stores its UNINDEXED columns in `%_content`
    /// (`contentless_unindexed=1`, C `FTS5_CONTENT_UNINDEXED`).
    unindexed_content: bool,
    /// `locale=1`: `%_content` carries one `l<N>` column per indexed column.
    locale: bool,
    /// `tokendata=1` (`bTokendata`).
    tokendata: bool,
    /// `pGlobal->aLocaleHdr` of the connection the table belongs to.
    locale_hdr: [u8; 16],
}

/// `FTS5_MAX_PREFIX_INDEXES`.
const FTS5_MAX_PREFIX_INDEXES: usize = 31;

fn cfg_err(msg: impl Into<String>) -> SqliteError {
    SqliteError::new(ErrorCode::Error, msg.into())
}

/// `sqlite3Fts5IsBareword`.
fn fts5_is_bareword(c: u8) -> bool {
    (c & 0x80) != 0 || c.is_ascii_alphanumeric() || c == b'_' || c == 0x1a
}

fn fts5_is_open_quote(c: u8) -> bool {
    matches!(c, b'"' | b'\'' | b'[' | b'`')
}

/// `fts5ConfigSkipWhitespace` (only the space character counts).
fn cfg_skip_ws(z: &[u8]) -> &[u8] {
    let n = z.iter().take_while(|&&c| c == b' ').count();
    &z[n..]
}

/// `fts5ConfigSkipBareword`: length of the leading bareword, `None` if there is none.
fn cfg_bareword_len(z: &[u8]) -> Option<usize> {
    let n = z.iter().take_while(|&&c| fts5_is_bareword(c)).count();
    (n > 0).then_some(n)
}

/// `fts5Dequote`: returns the dequoted text and the number of input bytes consumed. An
/// unterminated quote runs to the end of the input.
fn cfg_dequote(z: &[u8]) -> (Vec<u8>, usize) {
    let q = if z[0] == b'[' { b']' } else { z[0] };
    let mut i = 1usize;
    let mut out = Vec::with_capacity(z.len());
    while i < z.len() {
        if z[i] == q {
            if z.get(i + 1) != Some(&q) {
                i += 1;
                break;
            }
            i += 2;
            out.push(q);
        } else {
            out.push(z[i]);
            i += 1;
        }
    }
    (out, i)
}

/// `sqlite3Fts5Dequote`.
fn cfg_dequote_word(z: &[u8]) -> Vec<u8> {
    if !z.is_empty() && fts5_is_open_quote(z[0]) {
        cfg_dequote(z).0
    } else {
        z.to_vec()
    }
}

/// `fts5ConfigGobbleWord`: `Some((word, quoted, rest))` or `None` on a parse error.
fn cfg_gobble_word(z: &[u8]) -> Option<(Vec<u8>, bool, &[u8])> {
    if !z.is_empty() && fts5_is_open_quote(z[0]) {
        let (word, used) = cfg_dequote(z);
        Some((word, true, &z[used..]))
    } else {
        let n = cfg_bareword_len(z)?;
        Some((z[..n].to_vec(), false, &z[n..]))
    }
}

/// `fts5ConfigSkipLiteral`: length of the SQL literal at the start of `z`.
fn cfg_literal_len(z: &[u8]) -> Option<usize> {
    let at = |i: usize| z.get(i).copied().unwrap_or(0);
    match at(0) {
        b'n' | b'N' => (z.len() >= 4 && z[..4].eq_ignore_ascii_case(b"null")).then_some(4),
        b'x' | b'X' => {
            if at(1) != b'\'' {
                return None;
            }
            let mut p = 2usize;
            while at(p).is_ascii_hexdigit() {
                p += 1;
            }
            (at(p) == b'\'' && p.is_multiple_of(2)).then_some(p + 1)
        }
        b'\'' => {
            let mut p = 1usize;
            loop {
                if at(p) == b'\'' {
                    p += 1;
                    if at(p) != b'\'' {
                        return Some(p);
                    }
                }
                p += 1;
                if at(p) == 0 {
                    return None;
                }
            }
        }
        _ => {
            let mut p = 0usize;
            if at(p) == b'+' || at(p) == b'-' {
                p += 1;
            }
            while at(p).is_ascii_digit() {
                p += 1;
            }
            if at(p) == b'.' && at(p + 1).is_ascii_digit() {
                p += 2;
                while at(p).is_ascii_digit() {
                    p += 1;
                }
            }
            (p > 0).then_some(p)
        }
    }
}

/// `sqlite3_strnicmp(name, cmd, strlen(cmd))==0`: `cmd` is a case-insensitive prefix of
/// `name` (the way `fts5ConfigParseSpecial` recognises option names).
fn cfg_is_option(name: &str, cmd: &[u8]) -> bool {
    cmd.len() <= name.len() && name.as_bytes()[..cmd.len()].eq_ignore_ascii_case(cmd)
}

fn cfg_bool_arg(arg: &[u8]) -> Option<bool> {
    match arg {
        b"0" => Some(false),
        b"1" => Some(true),
        _ => None,
    }
}

/// Intermediate state of `sqlite3Fts5ConfigParse`.
#[derive(Default)]
struct Fts5ConfigBuilder {
    columns: Vec<String>,
    unindexed: Vec<bool>,
    tokenize_args: Option<Vec<String>>,
    prefixes: Vec<usize>,
    /// `None` = normal content, `Some("")` = contentless, `Some(tbl)` = external content.
    content: Option<String>,
    content_rowid: Option<String>,
    columnsize: Option<bool>,
    detail: Option<DetailMode>,
    contentless_delete: bool,
    contentless_unindexed: bool,
    locale: bool,
    tokendata: bool,
}

impl Fts5ConfigBuilder {
    /// `fts5ConfigParseSpecial`.
    fn parse_special(&mut self, cmd: &[u8], arg: &[u8]) -> Result<()> {
        if cfg_is_option("prefix", cmd) {
            let at = |i: usize| arg.get(i).copied().unwrap_or(0);
            let mut p = 0usize;
            let mut first = true;
            loop {
                while at(p) == b' ' {
                    p += 1;
                }
                if !first && at(p) == b',' {
                    p += 1;
                    while at(p) == b' ' {
                        p += 1;
                    }
                } else if at(p) == 0 {
                    break;
                }
                if !at(p).is_ascii_digit() {
                    return Err(cfg_err("malformed prefix=... directive"));
                }
                if self.prefixes.len() == FTS5_MAX_PREFIX_INDEXES {
                    return Err(cfg_err(format!(
                        "too many prefix indexes (max {FTS5_MAX_PREFIX_INDEXES})"
                    )));
                }
                let mut n_pre = 0usize;
                while at(p).is_ascii_digit() && n_pre < 1000 {
                    n_pre = n_pre * 10 + (at(p) - b'0') as usize;
                    p += 1;
                }
                if n_pre == 0 || n_pre >= 1000 {
                    return Err(cfg_err("prefix length out of range (max 999)"));
                }
                self.prefixes.push(n_pre);
                first = false;
            }
            return Ok(());
        }

        if cfg_is_option("tokenize", cmd) {
            if self.tokenize_args.is_some() {
                return Err(cfg_err("multiple tokenize=... directives"));
            }
            let mut args = Vec::new();
            let mut p = arg;
            while !p.is_empty() {
                let p2 = cfg_skip_ws(p);
                let len = if p2.first() == Some(&b'\'') {
                    cfg_literal_len(p2)
                } else {
                    cfg_bareword_len(p2)
                };
                let Some(len) = len else {
                    return Err(cfg_err("parse error in tokenize directive"));
                };
                args.push(String::from_utf8_lossy(&cfg_dequote_word(&p2[..len])).into_owned());
                p = cfg_skip_ws(&p2[len..]);
            }
            self.tokenize_args = Some(args);
            return Ok(());
        }

        if cfg_is_option("content", cmd) {
            if self.content.is_some() {
                return Err(cfg_err("multiple content=... directives"));
            }
            self.content = Some(String::from_utf8_lossy(arg).into_owned());
            return Ok(());
        }

        if cfg_is_option("contentless_delete", cmd) {
            self.contentless_delete = cfg_bool_arg(arg)
                .ok_or_else(|| cfg_err("malformed contentless_delete=... directive"))?;
            return Ok(());
        }

        if cfg_is_option("contentless_unindexed", cmd) {
            // (the C message names contentless_delete here as well)
            self.contentless_unindexed = cfg_bool_arg(arg)
                .ok_or_else(|| cfg_err("malformed contentless_delete=... directive"))?;
            return Ok(());
        }

        if cfg_is_option("content_rowid", cmd) {
            if self.content_rowid.is_some() {
                return Err(cfg_err("multiple content_rowid=... directives"));
            }
            self.content_rowid = Some(String::from_utf8_lossy(arg).into_owned());
            return Ok(());
        }

        if cfg_is_option("columnsize", cmd) {
            self.columnsize = Some(
                cfg_bool_arg(arg).ok_or_else(|| cfg_err("malformed columnsize=... directive"))?,
            );
            return Ok(());
        }

        if cfg_is_option("locale", cmd) {
            self.locale =
                cfg_bool_arg(arg).ok_or_else(|| cfg_err("malformed locale=... directive"))?;
            return Ok(());
        }

        if cfg_is_option("detail", cmd) {
            // `fts5ConfigSetEnum`: the value may be any unambiguous prefix of a name.
            let mut found: Option<DetailMode> = None;
            for (name, mode) in [
                ("none", DetailMode::None),
                ("full", DetailMode::Full),
                ("columns", DetailMode::Col),
            ] {
                if cfg_is_option(name, arg) {
                    if found.is_some() {
                        return Err(cfg_err("malformed detail=... directive"));
                    }
                    found = Some(mode);
                }
            }
            self.detail = Some(found.ok_or_else(|| cfg_err("malformed detail=... directive"))?);
            return Ok(());
        }

        if cfg_is_option("tokendata", cmd) {
            self.tokendata = cfg_bool_arg(arg)
                .ok_or_else(|| cfg_err("malformed tokendata=... directive"))?;
            return Ok(());
        }

        Err(cfg_err(format!(
            "unrecognized option: \"{}\"",
            String::from_utf8_lossy(cmd)
        )))
    }

    /// `fts5ConfigParseColumn`.
    fn parse_column(&mut self, col: &[u8], arg: Option<&[u8]>) -> Result<()> {
        let name = String::from_utf8_lossy(col).into_owned();
        if name.eq_ignore_ascii_case("rank") || name.eq_ignore_ascii_case("rowid") {
            return Err(cfg_err(format!("reserved fts5 column name: {name}")));
        }
        let mut unindexed = false;
        if let Some(arg) = arg {
            if arg.eq_ignore_ascii_case(b"unindexed") {
                unindexed = true;
            } else {
                return Err(cfg_err(format!(
                    "unrecognized column option: {}",
                    String::from_utf8_lossy(arg)
                )));
            }
        }
        self.columns.push(name);
        self.unindexed.push(unindexed);
        Ok(())
    }
}

impl Fts5Config {
    /// `sqlite3Fts5ConfigParse`.
    fn parse(table_name: &str, raw_args: &[String]) -> Result<Self> {
        if table_name.eq_ignore_ascii_case("rank") {
            return Err(cfg_err(format!("reserved fts5 table name: {table_name}")));
        }
        let mut b = Fts5ConfigBuilder::default();
        for raw in raw_args {
            let orig = raw.as_bytes();
            let parse_error = || {
                cfg_err(format!(
                    "parse error in \"{}\"",
                    String::from_utf8_lossy(orig)
                ))
            };
            let Some((one, must_be_col, rest)) = cfg_gobble_word(orig) else {
                return Err(parse_error());
            };
            let mut z = Some(cfg_skip_ws(rest));
            let mut is_option = false;
            if let Some(r) = z
                && r.first() == Some(&b'=')
            {
                is_option = true;
                z = if must_be_col { None } else { Some(&r[1..]) };
            }
            z = z.map(cfg_skip_ws);
            let mut two: Option<Vec<u8>> = None;
            if let Some(r) = z
                && !r.is_empty()
            {
                match cfg_gobble_word(r) {
                    Some((w, _, rest2)) => {
                        two = Some(w);
                        z = if rest2.is_empty() { Some(rest2) } else { None };
                    }
                    None => z = None,
                }
            }
            if z.is_none() {
                return Err(parse_error());
            }
            if is_option {
                b.parse_special(&one, two.as_deref().unwrap_or(b""))?;
            } else {
                b.parse_column(&one, two.as_deref())?;
            }
        }

        let contentless = b.content.as_deref() == Some("");
        if b.contentless_delete && !contentless {
            return Err(cfg_err("contentless_delete=1 requires a contentless table"));
        }
        let columnsize = b.columnsize.unwrap_or(true);
        if b.contentless_delete && !columnsize {
            return Err(cfg_err(
                "contentless_delete=1 is incompatible with columnsize=0",
            ));
        }
        if b.contentless_unindexed && !contentless {
            return Err(cfg_err(
                "contentless_unindexed=1 requires a contentless table",
            ));
        }

        // The hidden columns of the declared table (`sqlite3Fts5ConfigDeclareVtab`) make a
        // user column of the same name a duplicate.
        for (i, c) in b.columns.iter().enumerate() {
            if b.columns[..i].iter().any(|p| p.eq_ignore_ascii_case(c))
                || c.eq_ignore_ascii_case(table_name)
            {
                return Err(cfg_err(format!("duplicate column name: {c}")));
            }
        }

        let tokenizer = TokenizerSpec::load(b.tokenize_args.as_deref().unwrap_or(&[]))?;
        let content_mode = match b.content {
            None => ContentMode::Normal,
            Some(s) if s.is_empty() => ContentMode::Contentless,
            Some(tbl) => ContentMode::External {
                table: tbl,
                rowid_col: b.content_rowid,
            },
        };
        let unindexed_content = content_mode == ContentMode::Contentless
            && b.contentless_unindexed
            && b.unindexed.iter().any(|&u| u);

        Ok(Self {
            columns: b.columns,
            unindexed: b.unindexed,
            tokenizer,
            prefixes: b.prefixes,
            content_mode,
            columnsize,
            detail: b.detail.unwrap_or(DetailMode::Full),
            contentless_delete: b.contentless_delete,
            unindexed_content,
            locale: b.locale,
            tokendata: b.tokendata,
            locale_hdr: [0; 16],
        })
    }

    /// `sqlite3Fts5IsLocaleValue`: a blob longer than the header that starts with it.
    fn is_locale_value(&self, v: &Value) -> bool {
        matches!(v, Value::Blob(b) if b.len() > 16 && b[..16] == self.locale_hdr)
    }

    /// `sqlite3Fts5DecodeLocaleValue`: (text, locale) of an `fts5_locale()` blob;
    /// SQLITE_MISMATCH when no 0x00 ends the locale before the last byte.
    fn decode_locale_value<'a>(&self, blob: &'a [u8]) -> Result<(&'a [u8], &'a [u8])> {
        let n = blob.len();
        let mut n_loc = 16usize;
        while blob.get(n_loc).is_some_and(|&b| b != 0) {
            if n_loc == n - 1 {
                return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
            }
            n_loc += 1;
        }
        if n_loc >= n {
            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
        }
        Ok((&blob[n_loc + 1..], &blob[16..n_loc]))
    }

    /// The text a column value is tokenized as (`sqlite3_value_text`, or the text part of
    /// an `fts5_locale()` value on a locale=1 table).
    fn doc_text<'a>(&self, v: &'a Value) -> Result<Option<std::borrow::Cow<'a, [u8]>>> {
        if self.locale
            && self.is_locale_value(v)
            && let Value::Blob(b) = v
        {
            let (text, _) = self.decode_locale_value(b)?;
            return Ok(Some(std::borrow::Cow::Borrowed(text)));
        }
        Ok(fts5_value_bytes(v))
    }

    fn declare_sql(&self, table_name: &str) -> String {
        let mut parts = Vec::with_capacity(self.columns.len() + 2);
        for c in &self.columns {
            parts.push(quote_ident(c));
        }
        parts.push(format!("{} HIDDEN", quote_ident(table_name)));
        parts.push("rank HIDDEN".to_owned());
        format!("CREATE TABLE x({})", parts.join(", "))
    }

    /// True when the table has a `%_content` shadow table (normal content, or a contentless
    /// table that stores its UNINDEXED columns: C `FTS5_CONTENT_UNINDEXED`).
    fn has_content_table(&self) -> bool {
        self.content_mode == ContentMode::Normal || self.unindexed_content
    }

    /// Column definitions of `%_content` (`sqlite3Fts5StorageOpen`).
    fn content_table_defn(&self) -> String {
        let mut defn = String::from("id INTEGER PRIMARY KEY");
        for i in 0..self.columns.len() {
            if self.content_mode == ContentMode::Normal || self.unindexed[i] {
                defn.push_str(&format!(", c{i}"));
            }
        }
        if self.locale {
            for i in 0..self.columns.len() {
                if !self.unindexed[i] {
                    defn.push_str(&format!(", l{i}"));
                }
            }
        }
        defn
    }

    /// The `%_content` row for `col_vals` (`FTS5_STMT_INSERT_CONTENT` bindings in
    /// `sqlite3Fts5StorageContentInsert`): an `fts5_locale()` value is stored as its text
    /// in `c<N>` and its locale in `l<N>`; `locs` are the `l<N>` values of the columns
    /// taken from the saved row of an UPDATE.
    fn content_row(&self, rowid: i64, col_vals: &[Value], locs: &[Value]) -> Result<Vec<Value>> {
        let n = self.columns.len();
        let mut row = Vec::with_capacity(1 + 2 * n);
        let mut row_locs: Vec<Value> = (0..n)
            .map(|i| locs.get(i).cloned().unwrap_or(Value::Null))
            .collect();
        row.push(Value::Integer(rowid));
        for i in 0..n {
            if self.content_mode == ContentMode::Normal || self.unindexed[i] {
                match col_vals.get(i) {
                    Some(v) if self.is_locale_value(v) => {
                        let Value::Blob(b) = v else { unreachable!() };
                        let (text, loc) = self.decode_locale_value(b)?;
                        row.push(Value::Text(String::from_utf8_lossy(text).into_owned()));
                        row_locs[i] = Value::Text(String::from_utf8_lossy(loc).into_owned());
                    }
                    Some(v) => row.push(v.clone()),
                    None => row.push(Value::Null),
                }
            }
        }
        if self.locale {
            for (i, l) in row_locs.into_iter().enumerate() {
                if !self.unindexed[i] {
                    row.push(l);
                }
            }
        }
        Ok(row)
    }

    /// Maps a raw `%_content` row (as returned by `SELECT *`) to one value per user column.
    fn content_row_to_cols(&self, mut row: Vec<Value>) -> Vec<Value> {
        let n = self.columns.len();
        if self.content_mode == ContentMode::Normal {
            if !row.is_empty() {
                row.drain(..1);
            }
            row.truncate(n);
            row.resize(n, Value::Null);
            return row;
        }
        let mut it = row.into_iter().skip(1);
        (0..n)
            .map(|i| {
                if self.unindexed[i] {
                    it.next().unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            })
            .collect()
    }
}

// ============================================================================
// FTS5 Query AST & Bounded-Depth Recursive-Descent Parser
// ============================================================================

#[derive(Debug, Clone)]
struct QueryPhraseToken {
    /// The token as the tokenizer produced it (at most `FTS5_MAX_TOKEN_SIZE` bytes).
    raw: Vec<u8>,
    is_prefix: bool,
}

/// One phrase of a query (`Fts5ExprPhrase`).
#[derive(Debug, Clone, Default)]
struct QueryPhrase {
    tokens: Vec<QueryPhraseToken>,
    /// `aTerm[0].bFirst`: the phrase must start at the first token of a column (`^`).
    anchored_start: bool,
}

/// A NEAR group of phrases with an optional column filter (`Fts5ExprNearset` together with
/// its `FTS5_STRING` / `FTS5_TERM` / `FTS5_EOF` node).
#[derive(Debug, Clone)]
struct QueryNearset {
    /// Index of the first phrase of the group in the query's phrase list; assigned by
    /// `number_phrases` once parsing is finished.
    first_phrase: usize,
    phrases: Vec<QueryPhrase>,
    /// `nNear`.
    distance: i64,
    /// `pColset` (sorted column numbers).
    colset: Option<Vec<usize>>,
    /// An `FTS5_EOF` node: a phrase without terms or an empty column set. Never matches.
    eof: bool,
}

#[derive(Debug, Clone)]
enum FtsQueryExpr {
    Near(QueryNearset),
    And(Vec<FtsQueryExpr>),
    Or(Vec<FtsQueryExpr>),
    Not(Box<FtsQueryExpr>, Box<FtsQueryExpr>),
}

impl FtsQueryExpr {
    fn is_eof(&self) -> bool {
        matches!(self, Self::Near(n) if n.eof)
    }

    /// `Fts5ExprNode.iHeight`.
    fn height(&self) -> usize {
        match self {
            Self::Near(_) => 0,
            Self::And(ch) | Self::Or(ch) => 1 + ch.iter().map(Self::height).max().unwrap_or(0),
            Self::Not(l, r) => 1 + l.height().max(r.height()),
        }
    }

    /// Assigns phrase numbers in query order and returns the phrases.
    fn number_phrases(&mut self, out: &mut Vec<QueryPhrase>) {
        match self {
            Self::Near(n) => {
                n.first_phrase = out.len();
                out.extend(n.phrases.iter().cloned());
            }
            Self::And(ch) | Self::Or(ch) => {
                for c in ch {
                    c.number_phrases(out);
                }
            }
            Self::Not(l, r) => {
                l.number_phrases(out);
                r.number_phrases(out);
            }
        }
    }

    /// `fts5ParseSetColset`: restricts every phrase group below this node to `colset`.
    fn set_colset(&mut self, colset: &[usize]) {
        match self {
            Self::Near(n) => match &mut n.colset {
                Some(cur) => {
                    cur.retain(|c| colset.contains(c));
                    if cur.is_empty() {
                        n.eof = true;
                    }
                }
                None => n.colset = Some(colset.to_vec()),
            },
            Self::And(ch) | Self::Or(ch) => {
                for c in ch {
                    c.set_colset(colset);
                }
            }
            Self::Not(l, r) => {
                l.set_colset(colset);
                r.set_colset(colset);
            }
        }
    }
}

/// Token types of the FTS5 query language (`fts5ExprGetToken`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QTok {
    Lp,
    Rp,
    Lcp,
    Rcp,
    Colon,
    Comma,
    Plus,
    Star,
    Minus,
    Caret,
    Eof,
    Or,
    Not,
    And,
    Str,
}

fn fts5_query_err(msg: impl Into<String>) -> SqliteError {
    SqliteError::new(ErrorCode::Error, msg.into())
}

/// Recursive-descent equivalent of the `fts5parse.y` grammar:
///
/// ```text
/// expr     ::= expr OR expr | expr AND expr | expr NOT expr      (OR < AND < NOT)
///            | colset COLON LP expr RP | LP expr RP | exprlist
/// exprlist ::= cnearset | exprlist cnearset                      (implicit AND)
/// cnearset ::= nearset | colset COLON nearset
/// nearset  ::= phrase | CARET phrase | STRING LP nearphrases [COMMA STRING] RP
/// phrase   ::= STRING [STAR] | phrase PLUS STRING [STAR]
/// colset   ::= [MINUS] STRING | [MINUS] LCP STRING... RCP
/// ```
struct QueryParser<'a> {
    cfg: &'a Fts5Config,
    z: &'a [u8],
    pos: usize,
    tok: QTok,
    tok_text: &'a [u8],
    /// Number of grammar symbols on the stack of the equivalent LALR parser; the generated
    /// C parser has room for 99 (`fts5: parser stack overflow`).
    stack: usize,
    /// `Fts5Parse.bPhraseToAnd`: "a+b" becomes "a AND b" (LIKE / GLOB patterns on a
    /// trigram table without detail=full).
    b_phrase_to_and: bool,
}

/// Symbols the lemon-generated FTS5 parser can hold (`YYSTACKDEPTH` 100 minus the base entry).
const FTS5_PARSER_STACK_MAX: usize = 99;

impl<'a> QueryParser<'a> {
    fn parse_query_opt(
        cfg: &'a Fts5Config,
        text: &'a str,
        b_phrase_to_and: bool,
    ) -> Result<(FtsQueryExpr, Vec<QueryPhrase>)> {
        let mut p = Self {
            cfg,
            z: text.as_bytes(),
            pos: 0,
            tok: QTok::Eof,
            tok_text: b"",
            stack: 0,
            b_phrase_to_and,
        };
        p.advance()?;
        let mut expr = if p.tok == QTok::Eof {
            // An empty query is a syntax error at the end-of-input token.
            return Err(p.syntax_error());
        } else {
            p.parse_or()?
        };
        if p.tok != QTok::Eof {
            return Err(p.syntax_error());
        }
        let mut phrases = Vec::new();
        expr.number_phrases(&mut phrases);
        Ok((expr, phrases))
    }

    /// `fts5ExprGetToken`.
    fn advance(&mut self) -> Result<()> {
        let z = self.z;
        let mut i = self.pos;
        while i < z.len() && matches!(z[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        if i >= z.len() || z[i] == 0 {
            self.tok = QTok::Eof;
            self.tok_text = b"";
            self.pos = z.len();
            return Ok(());
        }
        let start = i;
        let simple = match z[i] {
            b'(' => Some(QTok::Lp),
            b')' => Some(QTok::Rp),
            b'{' => Some(QTok::Lcp),
            b'}' => Some(QTok::Rcp),
            b':' => Some(QTok::Colon),
            b',' => Some(QTok::Comma),
            b'+' => Some(QTok::Plus),
            b'*' => Some(QTok::Star),
            b'-' => Some(QTok::Minus),
            b'^' => Some(QTok::Caret),
            _ => None,
        };
        if let Some(t) = simple {
            self.tok = t;
            self.tok_text = &z[start..start + 1];
            self.pos = start + 1;
            return Ok(());
        }
        if z[i] == b'"' {
            i += 1;
            loop {
                if i < z.len() && z[i] == b'"' {
                    i += 1;
                    if i >= z.len() || z[i] != b'"' {
                        break;
                    }
                }
                if i >= z.len() || z[i] == 0 {
                    return Err(fts5_query_err("unterminated string"));
                }
                i += 1;
            }
            self.tok = QTok::Str;
        } else {
            if !fts5_is_bareword(z[i]) {
                return Err(fts5_query_err(format!(
                    "fts5: syntax error near \"{}\"",
                    String::from_utf8_lossy(&z[i..i + 1])
                )));
            }
            while i < z.len() && fts5_is_bareword(z[i]) {
                i += 1;
            }
            self.tok = match &z[start..i] {
                b"OR" => QTok::Or,
                b"NOT" => QTok::Not,
                b"AND" => QTok::And,
                _ => QTok::Str,
            };
        }
        self.tok_text = &z[start..i];
        self.pos = i;
        Ok(())
    }

    /// A shift (or the reduction of an empty rule) in the LALR parser: one more symbol.
    fn push(&mut self) -> Result<()> {
        if self.stack >= FTS5_PARSER_STACK_MAX {
            return Err(fts5_query_err("fts5: parser stack overflow"));
        }
        self.stack += 1;
        Ok(())
    }

    /// A reduction that replaces `n + 1` symbols by one.
    fn pop(&mut self, n: usize) {
        self.stack = self.stack.saturating_sub(n);
    }

    /// The lemon `%syntax_error` action.
    fn syntax_error(&self) -> SqliteError {
        fts5_query_err(format!(
            "fts5: syntax error near \"{}\"",
            String::from_utf8_lossy(self.tok_text)
        ))
    }

    /// `sqlite3Fts5ParseNode` for AND / OR / NOT: children of the same type are merged into
    /// one node and the tree height is limited.
    fn make_node(kind: QTok, left: FtsQueryExpr, right: FtsQueryExpr) -> Result<FtsQueryExpr> {
        let node = match kind {
            QTok::Not => FtsQueryExpr::Not(Box::new(left), Box::new(right)),
            QTok::And => {
                let mut ch = Vec::new();
                for side in [left, right] {
                    match side {
                        FtsQueryExpr::And(c) => ch.extend(c),
                        other => ch.push(other),
                    }
                }
                FtsQueryExpr::And(ch)
            }
            _ => {
                let mut ch = Vec::new();
                for side in [left, right] {
                    match side {
                        FtsQueryExpr::Or(c) => ch.extend(c),
                        other => ch.push(other),
                    }
                }
                FtsQueryExpr::Or(ch)
            }
        };
        if node.height() > FTS5_MAX_EXPR_DEPTH {
            return Err(fts5_query_err(format!(
                "fts5 expression tree is too large (maximum depth {FTS5_MAX_EXPR_DEPTH})"
            )));
        }
        Ok(node)
    }

    fn parse_or(&mut self) -> Result<FtsQueryExpr> {
        let mut lhs = self.parse_and()?;
        while self.tok == QTok::Or {
            self.push()?;
            self.advance()?;
            let rhs = self.parse_and()?;
            self.pop(2);
            lhs = Self::make_node(QTok::Or, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<FtsQueryExpr> {
        let mut lhs = self.parse_not()?;
        while self.tok == QTok::And {
            self.push()?;
            self.advance()?;
            let rhs = self.parse_not()?;
            self.pop(2);
            lhs = Self::make_node(QTok::And, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<FtsQueryExpr> {
        let mut lhs = self.parse_primary()?;
        while self.tok == QTok::Not {
            self.push()?;
            self.advance()?;
            let rhs = self.parse_primary()?;
            self.pop(2);
            lhs = Self::make_node(QTok::Not, lhs, rhs)?;
        }
        Ok(lhs)
    }

    /// True if the next two tokens are `STRING COLON` (a one-column colset).
    fn string_then_colon(&self) -> bool {
        let mut look = Self {
            cfg: self.cfg,
            z: self.z,
            pos: self.pos,
            tok: self.tok,
            tok_text: self.tok_text,
            stack: 0,
            b_phrase_to_and: self.b_phrase_to_and,
        };
        look.advance().is_ok() && look.tok == QTok::Colon
    }

    fn at_colset_start(&self) -> bool {
        matches!(self.tok, QTok::Minus | QTok::Lcp)
            || (self.tok == QTok::Str && self.string_then_colon())
    }

    /// `LP expr RP`, `colset COLON LP expr RP` or an `exprlist`.
    fn parse_primary(&mut self) -> Result<FtsQueryExpr> {
        if self.tok == QTok::Lp {
            self.push()?;
            self.advance()?;
            let inner = self.parse_or()?;
            if self.tok != QTok::Rp {
                return Err(self.syntax_error());
            }
            self.push()?;
            self.advance()?;
            self.pop(2);
            return Ok(inner);
        }
        let mut list: Option<FtsQueryExpr> = None;
        loop {
            let node = if self.at_colset_start() {
                let colset = self.parse_colset()?;
                if self.tok != QTok::Colon {
                    return Err(self.syntax_error());
                }
                self.push()?;
                self.advance()?;
                if self.tok == QTok::Lp {
                    // `colset : ( expr )` is an expression of its own, not part of a list.
                    if list.is_some() {
                        return Err(self.syntax_error());
                    }
                    self.push()?;
                    self.advance()?;
                    let mut inner = self.parse_or()?;
                    if self.tok != QTok::Rp {
                        return Err(self.syntax_error());
                    }
                    self.push()?;
                    self.advance()?;
                    self.pop(4);
                    self.apply_colset(&mut inner, &colset)?;
                    return Ok(inner);
                }
                let mut node = self.parse_nearset()?;
                self.pop(2);
                self.apply_colset(&mut node, &colset)?;
                node
            } else if matches!(self.tok, QTok::Str | QTok::Caret) {
                self.parse_nearset()?
            } else if list.is_none() {
                return Err(self.syntax_error());
            } else {
                break;
            };
            list = Some(match list {
                None => node,
                Some(left) => {
                    self.pop(1);
                    Self::implicit_and(left, node)?
                }
            });
            if !(matches!(self.tok, QTok::Str | QTok::Caret) || self.at_colset_start()) {
                break;
            }
        }
        list.ok_or_else(|| self.syntax_error())
    }

    /// `sqlite3Fts5ParseImplicitAnd`: phrases without terms disappear from a list.
    fn implicit_and(left: FtsQueryExpr, right: FtsQueryExpr) -> Result<FtsQueryExpr> {
        if right.is_eof() {
            return Ok(left);
        }
        match left {
            FtsQueryExpr::And(mut ch) => {
                if ch.last().is_some_and(FtsQueryExpr::is_eof) {
                    ch.pop();
                    ch.push(right);
                    Ok(FtsQueryExpr::And(ch))
                } else {
                    Self::make_node(QTok::And, FtsQueryExpr::And(ch), right)
                }
            }
            other if other.is_eof() => Ok(right),
            other => Self::make_node(QTok::And, other, right),
        }
    }

    /// `sqlite3Fts5ParseSetColset`.
    fn apply_colset(&self, node: &mut FtsQueryExpr, colset: &[usize]) -> Result<()> {
        if self.cfg.detail == DetailMode::None {
            return Err(fts5_query_err(
                "fts5: column queries are not supported (detail=none)",
            ));
        }
        node.set_colset(colset);
        Ok(())
    }

    /// `sqlite3Fts5ParseColset`: the column number of a (possibly quoted) column name.
    fn column_number(&self, raw: &[u8]) -> Result<usize> {
        let name = String::from_utf8_lossy(&cfg_dequote_word(raw)).into_owned();
        self.cfg
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(&name))
            .ok_or_else(|| fts5_query_err(format!("no such column: {name}")))
    }

    fn parse_colset(&mut self) -> Result<Vec<usize>> {
        let base = self.stack;
        let invert = self.tok == QTok::Minus;
        if invert {
            self.push()?;
            self.advance()?;
        }
        let mut cols: Vec<usize> = Vec::new();
        if self.tok == QTok::Lcp {
            self.push()?;
            self.advance()?;
            if self.tok != QTok::Str {
                return Err(self.syntax_error());
            }
            let list_base = self.stack;
            while self.tok == QTok::Str {
                self.push()?;
                let c = self.column_number(self.tok_text)?;
                if !cols.contains(&c) {
                    cols.push(c);
                }
                self.advance()?;
                self.stack = list_base + 1;
            }
            if self.tok != QTok::Rcp {
                return Err(self.syntax_error());
            }
            self.push()?;
            self.advance()?;
        } else if self.tok == QTok::Str {
            self.push()?;
            cols.push(self.column_number(self.tok_text)?);
            self.advance()?;
        } else {
            return Err(self.syntax_error());
        }
        self.stack = base + 1;
        cols.sort_unstable();
        if invert {
            // `sqlite3Fts5ParseColsetInvert`
            cols = (0..self.cfg.columns.len())
                .filter(|c| !cols.contains(c))
                .collect();
        }
        Ok(cols)
    }

    /// `phrase ::= STRING [STAR] (PLUS STRING [STAR])*` (`sqlite3Fts5ParseTerm`).
    fn parse_phrase(&mut self) -> Result<QueryPhrase> {
        let mut phrase = QueryPhrase::default();
        let base = self.stack;
        loop {
            if self.tok != QTok::Str {
                return Err(self.syntax_error());
            }
            self.push()?;
            let text = cfg_dequote_word(self.tok_text);
            self.advance()?;
            let is_prefix = self.tok == QTok::Star;
            // STAR is shifted, an absent one is the empty `star_opt` rule: one symbol each way
            self.push()?;
            if is_prefix {
                self.advance()?;
            }
            self.stack = base + 1;
            self.cfg.tokenizer.tok.tokenize(&text, &mut |tok, _, _| {
                phrase.tokens.push(QueryPhraseToken {
                    raw: tok[..tok.len().min(FTS5_MAX_TOKEN_SIZE)].to_vec(),
                    is_prefix: false,
                });
            });
            // The prefix flag always lands on the last term of the phrase so far.
            if let Some(last) = phrase.tokens.last_mut() {
                last.is_prefix = is_prefix;
            }
            if self.tok == QTok::Plus {
                self.push()?;
                self.advance()?;
            } else {
                break;
            }
        }
        Ok(phrase)
    }

    /// `nearset` followed by `sqlite3Fts5ParseNode(FTS5_STRING)`.
    fn parse_nearset(&mut self) -> Result<FtsQueryExpr> {
        let mut near = QueryNearset {
            first_phrase: 0,
            phrases: Vec::new(),
            distance: 10,
            colset: None,
            eof: false,
        };
        let mut is_near_group = false;
        let base = self.stack;
        if self.tok == QTok::Caret {
            self.push()?;
            self.advance()?;
            let mut phrase = self.parse_phrase()?;
            // `sqlite3Fts5ParseSetCaret`
            phrase.anchored_start = !phrase.tokens.is_empty();
            near.phrases.push(phrase);
        } else if self.tok == QTok::Str && {
            let mut look = Self {
                cfg: self.cfg,
                z: self.z,
                pos: self.pos,
                tok: self.tok,
                tok_text: self.tok_text,
                stack: 0,
                b_phrase_to_and: self.b_phrase_to_and,
            };
            look.advance().is_ok() && look.tok == QTok::Lp
        } {
            // STRING LP nearphrases neardist_opt RP
            let name = self.tok_text;
            self.push()?;
            self.advance()?;
            self.push()?;
            self.advance()?;
            if self.tok != QTok::Str {
                return Err(self.syntax_error());
            }
            let list_base = self.stack;
            while self.tok == QTok::Str {
                let phrase = self.parse_phrase()?;
                self.stack = list_base + 1;
                // `sqlite3Fts5ParseNearset`: a phrase without terms is dropped from a group
                // that has another phrase.
                if near.phrases.is_empty() {
                    near.phrases.push(phrase);
                } else if phrase.tokens.is_empty() {
                    // dropped
                } else if near.phrases.last().is_some_and(|l| l.tokens.is_empty()) {
                    near.phrases.pop();
                    near.phrases.push(phrase);
                } else {
                    near.phrases.push(phrase);
                }
            }
            let mut dist_tok: Option<&[u8]> = None;
            if self.tok == QTok::Comma {
                self.push()?;
                self.advance()?;
                if self.tok != QTok::Str {
                    return Err(self.syntax_error());
                }
                self.push()?;
                dist_tok = Some(self.tok_text);
                self.advance()?;
                self.pop(1);
            } else {
                self.push()?;
            }
            if self.tok != QTok::Rp {
                return Err(self.syntax_error());
            }
            self.push()?;
            self.advance()?;
            // `sqlite3Fts5ParseNear`
            if name != b"NEAR" {
                return Err(fts5_query_err(format!(
                    "fts5: syntax error near \"{}\"",
                    String::from_utf8_lossy(name)
                )));
            }
            // `sqlite3Fts5ParseSetDistance`
            if let Some(d) = dist_tok {
                let mut n_near: i64 = 0;
                for &c in d {
                    if !c.is_ascii_digit() {
                        return Err(fts5_query_err(format!(
                            "expected integer, got \"{}\"",
                            String::from_utf8_lossy(d)
                        )));
                    }
                    if n_near < 214_748_363 {
                        n_near = n_near * 10 + i64::from(c - b'0');
                    }
                }
                near.distance = n_near;
            }
            is_near_group = true;
        } else {
            let phrase = self.parse_phrase()?;
            near.phrases.push(phrase);
        }

        // `sqlite3Fts5ParseNode(FTS5_STRING)`
        near.eof = near.phrases.iter().any(|p| p.tokens.is_empty());
        if self.cfg.detail != DetailMode::Full
            && !(self.b_phrase_to_and && near.phrases[0].tokens.len() > 1)
        {
            let first = &near.phrases[0];
            if near.phrases.len() != 1
                || first.tokens.len() > 1
                || (!first.tokens.is_empty() && first.anchored_start)
            {
                return Err(fts5_query_err(format!(
                    "fts5: {} queries are not supported (detail!=full)",
                    if near.phrases.len() == 1 {
                        "phrase"
                    } else {
                        "NEAR"
                    }
                )));
            }
        }
        let _ = is_near_group;
        self.stack = base + 1;
        Ok(FtsQueryExpr::Near(near))
    }
}

// ============================================================================
// `Fts5Module`, `Fts5SharedState`, & `Fts5Table`
// ============================================================================

#[derive(Debug, Clone)]
struct Fts5ShadowNames {
    table_name: String,
    data: String,
    idx: String,
    content: String,
    docsize: String,
    config: String,
}

impl Fts5ShadowNames {
    fn new(tbl_name: &str) -> Self {
        Self {
            table_name: tbl_name.to_owned(),
            data: format!("{tbl_name}_data"),
            idx: format!("{tbl_name}_idx"),
            content: format!("{tbl_name}_content"),
            docsize: format!("{tbl_name}_docsize"),
            config: format!("{tbl_name}_config"),
        }
    }
}

/// A row of the content source as `FTS5_STMT_LOOKUP` returns it.
#[derive(Debug, Clone)]
struct SavedRow {
    /// One value per user column.
    cols: Vec<Value>,
    /// The `l<N>` values (NULL where there is none).
    locs: Vec<Value>,
}

/// The state of one FTS5 table: C's `Fts5Index` (pending-terms hash, cached structure,
/// configuration), the totals of `Fts5Storage`, `Fts5FullTable.iSavepoint` and the read
/// caches of this implementation.
#[derive(Debug)]
struct Fts5SharedState {
    shadows: Fts5ShadowNames,
    index: Fts5Index,
    /// `Fts5Storage.bTotalsValid` / `nTotalRow` / `aTotalSize`.
    b_totals_valid: bool,
    n_total_row: i64,
    a_total_size: Vec<i64>,
    /// `Fts5FullTable.iSavepoint`.
    i_savepoint: i64,
    docsize_hint: ShadowHint,
    /// What parameter 3 (`origin`) of the module's cached `REPLACE INTO %_docsize VALUES(?,?,?)`
    /// statement is bound to. `fts5StorageInsertDocsize` binds it, `fts5StorageNewRowid` binds
    /// only parameters 1 and 2 to NULL — the placeholder row it writes carries the origin of
    /// the previous docsize row (`sqlite3_reset` keeps bindings); NULL until the first one.
    docsize_origin_binding: Option<i64>,
    content_hint: ShadowHint,
    /// `Fts5FullTable.pSortCsr`: the cursor whose sorter statement is being run.
    p_sort_csr: Option<main::SortCsr>,
    /// `Fts5Storage.pSavedRow`: the old row of the UPDATE being applied.
    saved_row: Option<SavedRow>,
    /// The next row lookup of a delete keeps its row in `saved_row` (`bSaveRow`).
    want_saved_row: bool,
    /// The number of cursors open on the table (`fts5NewTransaction`).
    n_open_csr: usize,
    sql_replace_docsize3: String,
    sql_replace_config: String,
    sql_insert_content: String,
    sql_replace_content: String,
}

fn index_detail(d: DetailMode) -> IndexDetail {
    match d {
        DetailMode::Full => IndexDetail::Full,
        DetailMode::Col => IndexDetail::Columns,
        DetailMode::None => IndexDetail::None,
    }
}

impl Fts5SharedState {
    fn new(db_name: &str, tbl_name: &str, cfg: &Fts5Config) -> Self {
        let mut icfg = IndexConfig::new(
            index_detail(cfg.detail),
            cfg.prefixes.clone(),
            cfg.contentless_delete,
            cfg.columns.len(),
        );
        icfg.b_tokendata = cfg.tokendata;
        let mut st = Self {
            shadows: Fts5ShadowNames::new(tbl_name),
            index: Fts5Index::new(db_name, tbl_name, icfg),
            b_totals_valid: false,
            n_total_row: 0,
            a_total_size: vec![0; cfg.columns.len()],
            i_savepoint: 0,
            docsize_hint: ShadowHint::NONE,
            docsize_origin_binding: None,
            content_hint: ShadowHint::NONE,
            p_sort_csr: None,
            saved_row: None,
            want_saved_row: false,
            n_open_csr: 0,
            sql_replace_docsize3: String::new(),
            sql_replace_config: String::new(),
            sql_insert_content: String::new(),
            sql_replace_content: String::new(),
        };
        st.set_names(db_name, tbl_name, cfg);
        st
    }

    /// The statement texts of `Fts5Storage` (`fts5StorageGetStmt`).
    fn set_names(&mut self, db_name: &str, tbl_name: &str, cfg: &Fts5Config) {
        self.shadows = Fts5ShadowNames::new(tbl_name);
        self.index.set_names(db_name, tbl_name);
        self.docsize_hint = ShadowHint::NONE;
        self.content_hint = ShadowHint::NONE;
        // (a renamed table is connected anew: its statements are prepared afresh)
        self.docsize_origin_binding = None;
        let db_q = quote_ident(db_name);
        let docsize_q = quote_ident(&self.shadows.docsize);
        let config_q = quote_ident(&self.shadows.config);
        let content_q = quote_ident(&self.shadows.content);
        self.sql_replace_docsize3 = format!("REPLACE INTO {db_q}.{docsize_q} VALUES(?,?,?)");
        self.sql_replace_config = format!("REPLACE INTO {db_q}.{config_q} VALUES(?,?)");
        let n_content = cfg
            .content_row(0, &vec![Value::Null; cfg.columns.len()], &[])
            .unwrap_or_default()
            .len();
        let binds: Vec<String> = (1..=n_content).map(|i| format!("?{i}")).collect();
        let binds = binds.join(",");
        self.sql_insert_content = format!("INSERT INTO {db_q}.{content_q} VALUES({binds})");
        self.sql_replace_content = format!("REPLACE INTO {db_q}.{content_q} VALUES({binds})");
    }

    /// `fts5StorageLoadTotals(p, 1)`.
    fn load_totals(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        if !self.b_totals_valid {
            let (n_row, sizes) = self.index.get_averages(ctx)?;
            self.n_total_row = n_row;
            self.a_total_size = sizes;
            self.b_totals_valid = true;
        }
        Ok(())
    }

    /// `fts5StorageSaveTotals`.
    fn save_totals(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut buf: Vec<u8> = Vec::with_capacity(16);
        fts5_put_varint(&mut buf, self.n_total_row as u64);
        for &sz in &self.a_total_size {
            fts5_put_varint(&mut buf, sz as u64);
        }
        self.index.set_averages(ctx, &buf)
    }

    /// `sqlite3Fts5StorageSync`.
    fn storage_sync(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let i_last_rowid = ctx.last_insert_rowid();
        let mut res = Ok(());
        if self.b_totals_valid {
            res = self.save_totals(ctx);
            if res.is_ok() {
                self.b_totals_valid = false;
            }
        }
        if res.is_ok() {
            res = self.index.sync(ctx);
        }
        ctx.set_last_insert_rowid(i_last_rowid);
        res
    }

    /// `sqlite3Fts5StorageRollback`.
    fn storage_rollback(&mut self) {
        self.b_totals_valid = false;
        self.index.rollback();
    }

    /// `sqlite3Fts5StorageConfigValue`.
    fn storage_config_value(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        key: &str,
        val: Option<&Value>,
        i_val: i64,
    ) -> Result<()> {
        let bound = match val {
            Some(v) => v.clone(),
            None => Value::Integer(i_val),
        };
        ctx.execute_sql_counted(
            &self.sql_replace_config,
            &[Value::Text(key.to_owned()), bound],
        )?;
        if val.is_some() {
            let i_new = self.index.config.i_cookie.wrapping_add(1);
            self.index.set_cookie(ctx, i_new)?;
            self.index.config.i_cookie = i_new;
        }
        Ok(())
    }
}

/// Built-in `fts5` full-text search virtual table module.
#[derive(Debug, Clone)]
pub struct Fts5Module {
    aux_registry: Arc<Fts5AuxRegistry>,
}

impl Default for Fts5Module {
    fn default() -> Self {
        Self::new(Arc::new(Fts5AuxRegistry::new()))
    }
}

impl Fts5Module {
    pub fn new(aux_registry: Arc<Fts5AuxRegistry>) -> Self {
        Self { aux_registry }
    }
}

impl VirtualTableModule for Fts5Module {
    fn name(&self) -> &str {
        "fts5"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn shadow_table_suffixes(&self) -> &[&str] {
        SHADOW_SUFFIXES
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        let tbl_name = ctx.table_name().to_owned();
        let db_name = ctx.db_name().to_owned();
        let mut cfg = Fts5Config::parse(&tbl_name, args)?;
        cfg.locale_hdr = self.aux_registry.locale_hdr();

        let db_q = quote_ident(&db_name);
        let data_sq = quote_single(&format!("{tbl_name}_data"));
        let idx_sq = quote_single(&format!("{tbl_name}_idx"));
        let config_sq = quote_single(&format!("{tbl_name}_config"));

        // `sqlite3Fts5IndexOpen(bCreate)`
        ctx.execute_sql(
            &format!("CREATE TABLE {db_q}.{data_sq}(id INTEGER PRIMARY KEY, block BLOB)"),
            &[],
        )?;
        ctx.execute_sql(
            &format!(
                "CREATE TABLE {db_q}.{idx_sq}(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID"
            ),
            &[],
        )?;
        let mut state = Fts5SharedState::new(&db_name, &tbl_name, &cfg);
        state.index.reinit(ctx)?;

        // `sqlite3Fts5StorageOpen(bCreate)`
        if cfg.has_content_table() {
            let content_sq = quote_single(&format!("{tbl_name}_content"));
            ctx.execute_sql(
                &format!(
                    "CREATE TABLE {db_q}.{content_sq}({})",
                    cfg.content_table_defn()
                ),
                &[],
            )?;
        }

        if cfg.columnsize {
            let docsize_sq = quote_single(&format!("{tbl_name}_docsize"));
            let docsize_cols = if cfg.contentless_delete {
                "id INTEGER PRIMARY KEY, sz BLOB, origin INTEGER"
            } else {
                "id INTEGER PRIMARY KEY, sz BLOB"
            };
            ctx.execute_sql(
                &format!("CREATE TABLE {db_q}.{docsize_sq}({docsize_cols})"),
                &[],
            )?;
        }

        ctx.execute_sql(
            &format!("CREATE TABLE {db_q}.{config_sq}(k PRIMARY KEY, v) WITHOUT ROWID"),
            &[],
        )?;
        state.storage_config_value(ctx, "version", None, i64::from(FTS5_CURRENT_VERSION))?;

        ctx.declare_vtab(&cfg.declare_sql(&tbl_name))?;
        if cfg.content_mode == ContentMode::Normal {
            let _ = ctx.set_vtab_config(VTabConfigOption::ConstraintSupport);
        }
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
        let _ = ctx.set_vtab_config(VTabConfigOption::FindFunction);
        let _ = ctx.set_vtab_config(VTabConfigOption::NochangeUnread);

        // "Load the initial configuration": `sqlite3Fts5ConfigLoad(pConfig, iCookie-1)`.
        state.index.config_load(ctx, -1)?;

        let cfg_arc = Arc::new(cfg.clone());
        let state = Arc::new(Mutex::new(state));
        self.aux_registry.register_table(&state, &cfg_arc);
        Ok(Box::new(Fts5Table {
            cfg_arc,
            cfg,
            aux_registry: Arc::clone(&self.aux_registry),
            state,
        }))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        let tbl_name = ctx.table_name().to_owned();
        let mut cfg = Fts5Config::parse(&tbl_name, args)?;
        cfg.locale_hdr = self.aux_registry.locale_hdr();
        ctx.declare_vtab(&cfg.declare_sql(&tbl_name))?;
        if cfg.content_mode == ContentMode::Normal {
            let _ = ctx.set_vtab_config(VTabConfigOption::ConstraintSupport);
        }
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
        let _ = ctx.set_vtab_config(VTabConfigOption::FindFunction);
        let _ = ctx.set_vtab_config(VTabConfigOption::NochangeUnread);
        // fts5InitVtab -> sqlite3Fts5ConfigLoad(pConfig, iCookie-1): the `version` row of
        // %_config must be 4 or 5 (5 = secure-delete has been used). A config table that
        // cannot be read at this point is left to the first statement that needs it.
        let db_name = ctx.db_name().to_owned();
        let mut state = Fts5SharedState::new(&db_name, &tbl_name, &cfg);
        if let Err(e) = state.index.config_load(ctx, -1) {
            if e.message.starts_with("invalid fts5 file format") {
                return Err(e);
            }
            state.index.config.pgsz = 0;
        }
        let cfg_arc = Arc::new(cfg.clone());
        let state = Arc::new(Mutex::new(state));
        self.aux_registry.register_table(&state, &cfg_arc);
        Ok(Box::new(Fts5Table {
            cfg_arc,
            cfg,
            aux_registry: Arc::clone(&self.aux_registry),
            state,
        }))
    }
}

#[derive(Debug)]
struct Fts5Table {
    cfg: Fts5Config,
    /// The configuration as the cursors share it.
    cfg_arc: Arc<Fts5Config>,
    aux_registry: Arc<Fts5AuxRegistry>,
    state: Arc<Mutex<Fts5SharedState>>,
}

// `FTS5_BI_ORDER_*`: the bits of the idxNum C reports.
const FTS5_BI_ORDER_RANK: i32 = 0x0020;
const FTS5_BI_ORDER_ROWID: i32 = 0x0040;
const FTS5_BI_ORDER_DESC: i32 = 0x0080;
const PLAN_HINT_NEEDS_CONTENT: u64 = 1;
const PLAN_HINT_NEEDS_RANK_COL: u64 = 2;
const PLAN_HINT_NEEDS_TBL_COL: u64 = 4;

impl Fts5Table {
    fn hidden_tbl_col(&self) -> i32 {
        self.cfg.columns.len() as i32
    }

    fn hidden_rank_col(&self) -> i32 {
        (self.cfg.columns.len() + 1) as i32
    }

    /// `FTS5_STMT_LOOKUP`: the stored values of row `rowid`, `None` if the content source has
    /// no such row. Contentless tables answer from `%_docsize` (all values NULL) or, with
    /// `contentless_unindexed=1`, from `%_content` (UNINDEXED columns only).
    fn read_content_row(
        &self,
        state: &Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<Vec<Value>>> {
        match &self.cfg.content_mode {
            ContentMode::Contentless if !self.cfg.unindexed_content => {
                if self.cfg.columnsize
                    && ctx
                        .shadow_get_rowid(&state.shadows.docsize, rowid)?
                        .is_some()
                {
                    return Ok(Some(vec![Value::Null; self.cfg.columns.len()]));
                }
                Ok(None)
            }
            ContentMode::Normal | ContentMode::Contentless => Ok(ctx
                .shadow_get_rowid(&state.shadows.content, rowid)?
                .map(|r| self.cfg.content_row_to_cols(r))),
            ContentMode::External { table, rowid_col } => {
                let db_q = quote_ident(ctx.db_name());
                let ext_q = quote_ident(table);
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let cols: Vec<String> = self.cfg.columns.iter().map(|c| quote_ident(c)).collect();
                let sql = format!(
                    "SELECT {} FROM {db_q}.{ext_q} WHERE {rid_q} = ?1",
                    cols.join(", ")
                );
                let mut rows = ctx.query_sql(&sql, &[Value::Integer(rowid)])?;
                Ok(rows.pop())
            }
        }
    }

    /// The row `FTS5_STMT_LOOKUP` / `LOOKUP2` finds for `rowid`: one value per user column
    /// and, for a locale=1 table with its own content, the `l<N>` value of each.
    fn read_saved_row(
        &self,
        state: &Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<Option<SavedRow>> {
        let n = self.cfg.columns.len();
        if self.cfg.content_mode == ContentMode::Normal && self.cfg.locale {
            let Some(raw) = ctx.shadow_get_rowid(&state.shadows.content, rowid)? else {
                return Ok(None);
            };
            let mut locs = vec![Value::Null; n];
            let mut k = 1 + n;
            for (i, l) in locs.iter_mut().enumerate() {
                if !self.cfg.unindexed[i] {
                    *l = raw.get(k).cloned().unwrap_or(Value::Null);
                    k += 1;
                }
            }
            return Ok(Some(SavedRow {
                cols: self.cfg.content_row_to_cols(raw),
                locs,
            }));
        }
        Ok(self.read_content_row(state, ctx, rowid)?.map(|cols| SavedRow {
            cols,
            locs: vec![Value::Null; n],
        }))
    }

    /// `fts5StorageNewRowid`: `REPLACE INTO %_docsize VALUES(NULL, NULL[, NULL])` and the
    /// rowid it was given. Without a %_docsize table the caller has to supply the rowid.
    fn storage_new_rowid(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<i64> {
        if !self.cfg.columnsize {
            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
        }
        if self.cfg.contentless_delete {
            let origin = state
                .docsize_origin_binding
                .map_or(Value::Null, Value::Integer);
            ctx.execute_sql_counted(
                &state.sql_replace_docsize3,
                &[Value::Null, Value::Null, origin],
            )
            .map_err(rc_only)?;
            Ok(ctx.last_insert_rowid())
        } else {
            let docsize = state.shadows.docsize.clone();
            ctx.shadow_replace_int(&docsize, &mut state.docsize_hint, None, None)
        }
    }

    /// `sqlite3Fts5StorageContentInsert`: stores the row in `%_content` (when the table has
    /// one) and returns its rowid, allocating one when `new_rowid` is not an integer.
    fn content_insert(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        replace: bool,
        new_rowid: Option<&Value>,
        col_vals: &[Value],
        locs: &[Value],
    ) -> Result<i64> {
        if !self.cfg.has_content_table() {
            if let Some(Value::Integer(rid)) = new_rowid {
                return Ok(*rid);
            }
            return self.storage_new_rowid(state, ctx);
        }
        if self.cfg.unindexed_content && self.cfg.locale {
            // C prepares `INSERT INTO %_content VALUES(...)` without the l<N> columns for a
            // contentless_unindexed=1 + locale=1 table and reports the failure as corruption;
            // no row can be written to such a table by either engine.
            return Err(SqliteError::with_extended(
                FTS5_CORRUPT,
                "database disk image is malformed",
            ));
        }
        // The rowid is bound as it was given (`sqlite3_bind_value(pInsert, 1, apVal[1])`):
        // NULL lets the INSERT choose one.
        let rid = content_rowid_from_value(new_rowid)?;
        let sql = if replace {
            &state.sql_replace_content
        } else {
            &state.sql_insert_content
        };
        sqliters_types::dtrace!("TR|f5|w|tbl=content replace={}", u8::from(replace));
        let content = state.shadows.content.clone();
        // the C module reports the bare result code of the %_content insert
        if self.cfg.content_mode == ContentMode::Normal
            && !self.cfg.locale
            && col_vals.len() == self.cfg.columns.len()
        {
            // (the row is the column values as they are)
            return ctx
                .shadow_insert_row(
                    &content,
                    &mut state.content_hint,
                    sql,
                    rid,
                    col_vals,
                    replace,
                )
                .map_err(rc_only);
        }
        let row = self.cfg.content_row(0, col_vals, locs)?;
        ctx.shadow_insert_row(
            &content,
            &mut state.content_hint,
            sql,
            rid,
            &row[1..],
            replace,
        )
        .map_err(rc_only)
    }

    /// `fts5StorageInsertDocsize`.
    fn insert_docsize(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        buf: &[u8],
    ) -> Result<()> {
        if self.cfg.columnsize {
            sqliters_types::dtrace!("TR|f5|w|tbl=docsize k={} n={}", rowid, buf.len());
            if self.cfg.contentless_delete {
                let origin = state.index.get_origin(ctx)?;
                state.docsize_origin_binding = Some(origin);
                ctx.execute_sql_counted(
                    &state.sql_replace_docsize3,
                    &[
                        Value::Integer(rowid),
                        Value::Blob(buf.to_vec()),
                        Value::Integer(origin),
                    ],
                )
                .map_err(rc_only)?;
            } else {
                let docsize = state.shadows.docsize.clone();
                ctx.shadow_replace_blob(&docsize, &mut state.docsize_hint, Some(rowid), buf)
                    .map_err(rc_only)?;
            }
        }
        Ok(())
    }

    /// The tokenizer loop shared by `sqlite3Fts5StorageIndexInsert`, `sqlite3Fts5StorageRebuild`
    /// (`delete == false`: tokens are added, the column sizes are appended to `buf` and added
    /// to the totals) and `fts5StorageDeleteFromIndex` (`delete == true`: delete markers; the
    /// totals shrink and may not become negative).
    fn index_tokens(
        &self,
        state: &mut Fts5SharedState,
        col_vals: &[Value],
        delete: bool,
        buf: &mut Vec<u8>,
        sizes_out: &mut Vec<u64>,
    ) -> Result<()> {
        let mut res = Ok(());
        for i_col in 0..self.cfg.columns.len() {
            let mut sz_col: i32 = 0;
            if !self.cfg.unindexed[i_col] {
                if delete && res.is_err() {
                    break;
                }
                let text = match col_vals.get(i_col) {
                    Some(v) => match self.cfg.doc_text(v) {
                        Ok(t) => t,
                        Err(e) => {
                            res = Err(e);
                            break;
                        }
                    },
                    None => None,
                };
                if let Some(text) = text {
                    let index = &mut state.index;
                    let col_arg = if delete { -1 } else { i_col as i32 };
                    self.cfg.tokenizer.for_each_doc_token(&text, |tok| {
                        sz_col += 1;
                        index.write(col_arg, sz_col - 1, tok);
                    });
                }
                if delete {
                    state.a_total_size[i_col] -= i64::from(sz_col);
                    if state.a_total_size[i_col] < 0 {
                        res = Err(SqliteError::with_extended(
                            FTS5_CORRUPT,
                            "database disk image is malformed",
                        ));
                    }
                }
            }
            if !delete {
                fts5_put_varint(buf, sz_col as u64);
                state.a_total_size[i_col] += i64::from(sz_col);
                sizes_out.push(sz_col as u64);
            }
        }
        res
    }

    /// `sqlite3Fts5StorageIndexInsert`: index entries, `%_docsize` row and totals for a row.
    fn index_insert(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        col_vals: &[Value],
    ) -> Result<()> {
        let mut res = state.load_totals(ctx);
        if res.is_ok() {
            res = state.index.begin_write(ctx, false, rowid);
        }
        let mut buf: Vec<u8> = Vec::with_capacity(self.cfg.columns.len() + 4);
        let mut sizes: Vec<u64> = Vec::with_capacity(self.cfg.columns.len());
        if res.is_ok() {
            res = self.index_tokens(state, col_vals, false, &mut buf, &mut sizes);
        }
        // `p->nTotalRow++` is not guarded by `rc`: the row is counted even when the write
        // could not begin (the statement fails, the in-memory totals keep the count).
        state.n_total_row += 1;
        res?;
        // Write the %_docsize record
        self.insert_docsize(state, ctx, rowid, &buf)?;
        Ok(())
    }

    /// `fts5StorageInsert`.
    fn storage_insert(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        new_rowid: Option<&Value>,
        col_vals: &[Value],
        locs: &[Value],
    ) -> Result<i64> {
        let rid = self.content_insert(state, ctx, false, new_rowid, col_vals, locs)?;
        self.index_insert(state, ctx, rid, col_vals)?;
        Ok(rid)
    }

    /// `fts5StorageDeleteFromIndex`.
    fn delete_from_index(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        vals: Option<&[Value]>,
    ) -> Result<()> {
        let looked_up;
        let old_vals: &[Value] = match vals {
            Some(v) => v,
            None => {
                // FTS5_STMT_LOOKUP (+bSaveRow): without a row there is nothing to remove.
                let save = std::mem::take(&mut state.want_saved_row);
                let row = self.read_saved_row(state, ctx, rowid)?;
                looked_up = row.as_ref().map(|r| r.cols.clone());
                if save {
                    state.saved_row = row;
                }
                match looked_up.as_deref() {
                    Some(v) => v,
                    None => return Ok(()),
                }
            }
        };
        let mut buf = Vec::new();
        let mut sizes = Vec::new();
        let mut res = self.index_tokens(state, old_vals, true, &mut buf, &mut sizes);
        if res.is_ok() && state.n_total_row < 1 {
            res = Err(SqliteError::with_extended(
                FTS5_CORRUPT,
                "database disk image is malformed",
            ));
        } else {
            state.n_total_row -= 1;
        }
        res
    }

    /// `sqlite3Fts5StorageDelete`: removes row `rowid` from the index, `%_docsize` and
    /// `%_content`. `vals` are the values to remove from the index (the `'delete'` command);
    /// without them they are looked up in the content source, and a row that is not found
    /// there leaves the index untouched.
    fn storage_delete(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
        vals: Option<&[Value]>,
    ) -> Result<()> {
        state.load_totals(ctx)?;
        // Delete the index records
        state.index.begin_write(ctx, true, rowid)?;
        if self.cfg.contentless_delete {
            let save = std::mem::take(&mut state.want_saved_row);
            self.contentless_delete_row(state, ctx, rowid)?;
            // "if( rc==SQLITE_OK && bSaveRow && eContent==FTS5_CONTENT_UNINDEXED )
            //    rc = sqlite3Fts5StorageFindDeleteRow(p, iDel);"
            if save && self.cfg.unindexed_content {
                state.saved_row = self.read_saved_row(state, ctx, rowid)?;
            }
        } else {
            self.delete_from_index(state, ctx, rowid, vals)?;
        }

        // Delete the %_docsize record
        if self.cfg.columnsize {
            sqliters_types::dtrace!("TR|f5|d|tbl=docsize k={}", rowid);
            let docsize = state.shadows.docsize.clone();
            ctx.shadow_delete_counted(&docsize, &mut state.docsize_hint, rowid)
                .map_err(rc_only)?;
        }
        // Delete the %_content record
        if self.cfg.has_content_table() {
            sqliters_types::dtrace!("TR|f5|d|tbl=content k={}", rowid);
            let content = state.shadows.content.clone();
            ctx.shadow_delete_counted(&content, &mut state.content_hint, rowid)
                .map_err(rc_only)?;
        }
        Ok(())
    }

    /// `fts5StorageContentlessDelete`: a row of a contentless_delete=1 table is known by its
    /// `%_docsize` record; without one there is nothing to delete.
    fn contentless_delete_row(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        rowid: i64,
    ) -> Result<()> {
        state.load_totals(ctx)?;
        // Look up the origin of the document in the %_docsize table. Store this in stack
        // variable iOrigin.
        let mut i_origin: i64 = 0;
        if let Some(row) = ctx.shadow_get_rowid(&state.shadows.docsize, rowid)? {
            let n_cols = self.cfg.columns.len();
            let blob: &[u8] = match row.get(1) {
                Some(Value::Blob(b)) => b,
                Some(Value::Text(s)) => s.as_bytes(),
                _ => &[],
            };
            // fts5StorageDecodeSizeArray
            let mut i_off = 0usize;
            let mut bad = false;
            let mut a_col: Vec<i64> = Vec::with_capacity(n_cols);
            for _ in 0..n_cols {
                if i_off >= blob.len() {
                    bad = true;
                    break;
                }
                let (v, nb) = fts5_get_varint32(blob, i_off);
                i_off += nb;
                a_col.push(i64::from(v));
            }
            if bad || i_off != blob.len() {
                return Err(SqliteError::with_extended(
                    FTS5_CORRUPT,
                    "database disk image is malformed",
                ));
            }
            for (ii, v) in a_col.iter().enumerate() {
                state.a_total_size[ii] -= *v;
            }
            i_origin = i64::from(row.get(2).map_or(0, Value::to_integer_lossy) as i32);
        }
        if i_origin != 0 {
            let res = state.index.contentless_delete(ctx, i_origin, rowid);
            // `p->nTotalRow--` follows the call whatever it returned
            state.n_total_row -= 1;
            res?;
        }
        Ok(())
    }

    /// `sqlite3Fts5FlushToDisk`.
    fn flush_to_disk(state: &mut Fts5SharedState, ctx: &mut dyn VTabHostContext) -> Result<()> {
        state.storage_sync(ctx)
    }

    /// `fts5SpecialInsert` / `fts5SpecialDelete`: `INSERT INTO ft(ft, ...) VALUES(cmd, ...)`.
    fn execute_special_command(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        cmd: &str,
        new_rowid: Option<Value>,
        values: &[Value],
    ) -> Result<()> {
        let n_cols = self.cfg.columns.len();
        let rank_val = values.get(n_cols + 1).cloned().unwrap_or(Value::Null);
        let contentless = self.cfg.content_mode == ContentMode::Contentless;

        if self.cfg.content_mode != ContentMode::Normal && cmd.eq_ignore_ascii_case("delete") {
            if self.cfg.contentless_delete {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "'delete' may not be used with a contentless_delete=1 table",
                ));
            }
            if let Some(Value::Integer(rid)) = new_rowid {
                self.storage_delete(state, ctx, rid, Some(&values[..n_cols]))?;
            }
            return Ok(());
        }

        let mut b_load_config = false;
        if cmd.eq_ignore_ascii_case("delete-all") {
            if self.cfg.content_mode == ContentMode::Normal {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "'delete-all' may only be used with a contentless or external content fts5 table",
                ));
            }
            self.storage_delete_all(state, ctx)?;
            b_load_config = true;
        } else if cmd.eq_ignore_ascii_case("rebuild") {
            if contentless {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "'rebuild' may not be used with a contentless fts5 table",
                ));
            }
            self.storage_rebuild(state, ctx)?;
            b_load_config = true;
        } else if cmd.eq_ignore_ascii_case("optimize") {
            state.index.optimize(ctx)?;
        } else if cmd.eq_ignore_ascii_case("merge") {
            let n_merge = rank_val.to_integer_lossy() as i32;
            state.index.merge(ctx, n_merge)?;
        } else if cmd.eq_ignore_ascii_case("integrity-check") {
            // `sqlite3Fts5StorageIntegrity(p, iArg)`: an external content table is only
            // compared with the index when the rank argument is non-zero.
            let check_external = rank_val.to_integer_lossy() as i32 != 0;
            return self.verify_integrity(state, ctx, check_external);
        } else if cmd.eq_ignore_ascii_case("flush") {
            Self::flush_to_disk(state, ctx)?;
        } else {
            // Everything else is a configuration setting (`sqlite3Fts5ConfigSetValue`); an
            // unknown key or an out-of-range value is SQLITE_ERROR without a message.
            Self::flush_to_disk(state, ctx)?;
            state.index.load_config(ctx)?;
            let mut b_error = !config_value_is_valid(cmd, &rank_val);
            if !b_error || cmd.eq_ignore_ascii_case("deletemerge") {
                // (C applies the value to the in-memory configuration first)
                let ok =
                    crate::fts5_index::config_set_value(&mut state.index.config, cmd, &rank_val)?;
                if !ok {
                    b_error = true;
                }
            }
            if b_error {
                return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
            }
            state.storage_config_value(ctx, cmd, Some(&rank_val), 0)?;
        }
        if b_load_config {
            state.index.config.i_cookie = state.index.config.i_cookie.wrapping_sub(1);
            state.index.load_config(ctx)?;
        }
        Ok(())
    }

    /// `sqlite3Fts5StorageDeleteAll`.
    fn storage_delete_all(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<()> {
        state.b_totals_valid = false;
        let db_q = quote_ident(ctx.db_name());
        let data_q = quote_ident(&state.shadows.data);
        let idx_q = quote_ident(&state.shadows.idx);
        // Delete the contents of the %_data and %_docsize tables.
        sqliters_types::dtrace!("TR|f5|deleteall|");
        ctx.execute_sql_counted(&format!("DELETE FROM {db_q}.{data_q}"), &[])
            .map_err(rc_only)?;
        ctx.execute_sql_counted(&format!("DELETE FROM {db_q}.{idx_q}"), &[])
            .map_err(rc_only)?;
        state.index.write_gen = state.index.write_gen.wrapping_add(1);
        if self.cfg.columnsize {
            let docsize_q = quote_ident(&state.shadows.docsize);
            ctx.execute_sql_counted(&format!("DELETE FROM {db_q}.{docsize_q}"), &[])
                .map_err(rc_only)?;
        }
        if self.cfg.unindexed_content {
            let content_q = quote_ident(&state.shadows.content);
            ctx.execute_sql_counted(&format!("DELETE FROM {db_q}.{content_q}"), &[])
                .map_err(rc_only)?;
        }
        // Reinitialize the %_data table. This call creates the initial structure and
        // averages records.
        state.index.reinit(ctx)?;
        state.storage_config_value(ctx, "version", None, i64::from(FTS5_CURRENT_VERSION))
    }

    /// `sqlite3Fts5StorageRebuild`.
    fn storage_rebuild(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
    ) -> Result<()> {
        self.storage_delete_all(state, ctx)?;
        state.load_totals(ctx)?;
        // FTS5_STMT_SCAN: `SELECT <exprlist> FROM <content> AS T`
        let db_q = quote_ident(ctx.db_name());
        let src_rows = match &self.cfg.content_mode {
            ContentMode::Normal => {
                let content_q = quote_ident(&state.shadows.content);
                let mut cols = vec!["T.id".to_owned()];
                for i in 0..self.cfg.columns.len() {
                    cols.push(format!("T.c{i}"));
                }
                ctx.query_sql(
                    &format!("SELECT {} FROM {db_q}.{content_q} AS T", cols.join(", ")),
                    &[],
                )?
            }
            ContentMode::External { table, rowid_col } => {
                let ext_q = quote_ident(table);
                let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                let mut cols = vec![format!("T.{rid_q}")];
                for c in &self.cfg.columns {
                    cols.push(format!("T.{}", quote_ident(c)));
                }
                ctx.query_sql(
                    &format!("SELECT {} FROM {db_q}.{ext_q} AS T", cols.join(", ")),
                    &[],
                )?
            }
            ContentMode::Contentless => Vec::new(),
        };

        for row in src_rows {
            let rid = row.first().map_or(0, Value::to_integer_lossy);
            let cvals = &row[1..];
            let mut res = state.index.begin_write(ctx, false, rid);
            let mut buf: Vec<u8> = Vec::with_capacity(self.cfg.columns.len() + 4);
            let mut sizes: Vec<u64> = Vec::with_capacity(self.cfg.columns.len());
            if res.is_ok() {
                res = self.index_tokens(state, cvals, false, &mut buf, &mut sizes);
            }
            // `p->nTotalRow++` is not guarded by `rc` (as in `index_insert`)
            state.n_total_row += 1;
            res?;
            self.insert_docsize(state, ctx, rid, &buf)?;
        }
        // Write the averages record
        state.save_totals(ctx)
    }

    /// `sqlite3Fts5StorageIntegrity(p, iArg)`.
    fn verify_integrity(
        &self,
        state: &mut Fts5SharedState,
        ctx: &mut dyn VTabHostContext,
        check_external: bool,
    ) -> Result<()> {
        let corrupt =
            || SqliteError::with_extended(FTS5_CORRUPT, "database disk image is malformed");
        let n_cols = self.cfg.columns.len();
        let mut a_total_size = vec![0i64; n_cols];
        let mut cksum: u64 = 0;
        let b_use_cksum = match self.cfg.content_mode {
            ContentMode::Normal => true,
            ContentMode::External { .. } => check_external,
            ContentMode::Contentless => false,
        };
        if b_use_cksum {
            // Generate the expected index checksum based on the contents of the %_content
            // table. This block also checks that the %_docsize records match the content.
            let db_q = quote_ident(ctx.db_name());
            let src_rows = match &self.cfg.content_mode {
                ContentMode::Normal => {
                    let content_q = quote_ident(&state.shadows.content);
                    let mut cols = vec!["T.id".to_owned()];
                    for i in 0..n_cols {
                        cols.push(format!("T.c{i}"));
                    }
                    ctx.query_sql(
                        &format!("SELECT {} FROM {db_q}.{content_q} AS T", cols.join(", ")),
                        &[],
                    )?
                }
                ContentMode::External { table, rowid_col } => {
                    let ext_q = quote_ident(table);
                    let rid_q = quote_ident(rowid_col.as_deref().unwrap_or("rowid"));
                    let mut cols = vec![format!("T.{rid_q}")];
                    for c in &self.cfg.columns {
                        cols.push(format!("T.{}", quote_ident(c)));
                    }
                    ctx.query_sql(
                        &format!("SELECT {} FROM {db_q}.{ext_q} AS T", cols.join(", ")),
                        &[],
                    )?
                }
                ContentMode::Contentless => Vec::new(),
            };
            let detail = self.cfg.detail;
            for row in src_rows {
                let i_rowid = row.first().map_or(0, Value::to_integer_lossy);
                let mut a_col_size: Vec<i64> = Vec::new();
                if self.cfg.columnsize {
                    // sqlite3Fts5StorageDocsize
                    let stored = ctx.shadow_get_rowid(&state.shadows.docsize, i_rowid)?;
                    let blob: Option<Vec<u8>> = stored.and_then(|r| match r.into_iter().nth(1) {
                        Some(Value::Blob(b)) => Some(b),
                        Some(Value::Text(t)) => Some(t.into_bytes()),
                        Some(_) => Some(Vec::new()),
                        None => None,
                    });
                    let mut ok = false;
                    if let Some(blob) = blob {
                        let mut i_off = 0usize;
                        let mut bad = false;
                        for _ in 0..n_cols {
                            if i_off >= blob.len() {
                                bad = true;
                                break;
                            }
                            let (v, nb) = fts5_get_varint32(&blob, i_off);
                            i_off += nb;
                            a_col_size.push(i64::from(v));
                        }
                        ok = !bad && i_off == blob.len();
                    }
                    if !ok {
                        return Err(corrupt());
                    }
                }
                // (detail=none: one termset per row; detail=column: one per column)
                let mut termset: std::collections::HashSet<(u8, Vec<u8>)> =
                    std::collections::HashSet::new();
                for i in 0..n_cols {
                    if self.cfg.unindexed[i] {
                        continue;
                    }
                    let mut sz_col: i64 = 0;
                    if detail == DetailMode::Col {
                        termset.clear();
                    }
                    if let Some(text) = row.get(i + 1).and_then(fts5_value_bytes) {
                        let prefixes = &self.cfg.prefixes;
                        self.cfg.tokenizer.for_each_doc_token(&text, |tok| {
                            sz_col += 1;
                            let (i_pos, i_col) = match detail {
                                DetailMode::Full => ((sz_col - 1) as i32, i as i32),
                                DetailMode::Col => (i as i32, 0),
                                DetailMode::None => (0, 0),
                            };
                            let present =
                                detail != DetailMode::Full && !termset.insert((0u8, tok.to_vec()));
                            if !present {
                                cksum ^=
                                    crate::fts5_index::entry_cksum(i_rowid, i_col, i_pos, 0, tok);
                            }
                            for (ii, &n_char) in prefixes.iter().enumerate() {
                                let n_byte = crate::fts5_index::charlen_to_bytelen(tok, n_char);
                                if n_byte != 0 {
                                    let present = detail != DetailMode::Full
                                        && !termset
                                            .insert(((ii + 1) as u8, tok[..n_byte].to_vec()));
                                    if !present {
                                        cksum ^= crate::fts5_index::entry_cksum(
                                            i_rowid,
                                            i_col,
                                            i_pos,
                                            (ii + 1) as i32,
                                            &tok[..n_byte],
                                        );
                                    }
                                }
                            }
                        });
                    }
                    // If this is not a columnsize=0 database, check that the number of
                    // tokens in the value matches the aColSize[] value read from the
                    // %_docsize table.
                    if self.cfg.columnsize && sz_col != a_col_size[i] {
                        return Err(corrupt());
                    }
                    a_total_size[i] += sz_col;
                }
            }

            // Test that the "totals" (sometimes called "averages") record looks Ok
            // (`fts5StorageLoadTotals(p, 0)`: the totals of the open transaction, if any)
            if !state.b_totals_valid {
                let (n_row, sizes) = state.index.get_averages(ctx)?;
                state.n_total_row = n_row;
                state.a_total_size = sizes;
            }
            for i in 0..n_cols {
                if state.a_total_size.get(i).copied().unwrap_or(0) != a_total_size[i] {
                    return Err(corrupt());
                }
            }
            let n_total_row = state.n_total_row;

            // Check that the %_docsize and %_content tables contain the expected number
            // of rows.
            if self.cfg.content_mode == ContentMode::Normal {
                let content_q = quote_ident(&state.shadows.content);
                let n_row = ctx
                    .query_sql(&format!("SELECT count(*) FROM {db_q}.{content_q}"), &[])?
                    .first()
                    .and_then(|r| r.first())
                    .map_or(0, Value::to_integer_lossy);
                if n_row != n_total_row {
                    return Err(corrupt());
                }
            }
            if self.cfg.columnsize {
                let docsize_q = quote_ident(&state.shadows.docsize);
                let n_row = ctx
                    .query_sql(&format!("SELECT count(*) FROM {db_q}.{docsize_q}"), &[])?
                    .first()
                    .and_then(|r| r.first())
                    .map_or(0, Value::to_integer_lossy);
                if n_row != n_total_row {
                    return Err(corrupt());
                }
            }
        }

        // Pass the expected checksum down to the FTS index module. It will verify, amongst
        // other things, that it matches the checksum generated by inspecting the index
        // itself.
        state.index.integrity_check(ctx, cksum, b_use_cksum)
    }
}

/// The text of a value as `sqlite3_value_text()` returns it: `None` for NULL, the bytes of a
/// BLOB, C's `"%!.17g"` rendering of a REAL.
fn fts5_value_bytes(v: &Value) -> Option<std::borrow::Cow<'_, [u8]>> {
    use std::borrow::Cow;
    match v {
        Value::Null => None,
        Value::Text(s) => Some(Cow::Borrowed(s.as_bytes())),
        Value::Blob(b) => Some(Cow::Borrowed(b.as_slice())),
        Value::Integer(i) => Some(Cow::Owned(i.to_string().into_bytes())),
        other => Some(Cow::Owned(real_to_text(other.to_real_lossy()).into_bytes())),
    }
}

/// `fts5_value_bytes` as (lossy) text.
fn fts5_value_text(v: &Value) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    match v {
        Value::Null => None,
        Value::Text(s) => Some(Cow::Borrowed(s.as_str())),
        Value::Blob(b) => Some(String::from_utf8_lossy(b)),
        Value::Integer(i) => Some(Cow::Owned(i.to_string())),
        other => Some(Cow::Owned(real_to_text(other.to_real_lossy()))),
    }
}

/// `sqlite3_value_numeric_type(v)==SQLITE_INTEGER` followed by `sqlite3_value_int64(v)`.
fn numeric_integer_value(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Integer(i) => Some(*i),
        Value::Text(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// The rowid SQLite stores when `v` is bound to the INTEGER PRIMARY KEY of `%_content`:
/// `None` = allocate one, an error for a value that is not an integer.
fn content_rowid_from_value(v: Option<&Value>) -> Result<Option<i64>> {
    let mismatch = || SqliteError::new(ErrorCode::Mismatch, "datatype mismatch");
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Integer(i)) => Ok(Some(*i)),
        Some(Value::Text(s)) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                return Ok(Some(i));
            }
            match t.parse::<f64>() {
                Ok(r) if r.fract() == 0.0 && r.abs() < 9.2e18 => Ok(Some(r as i64)),
                _ => Err(mismatch()),
            }
        }
        Some(Value::Blob(_)) => Err(mismatch()),
        Some(other) => {
            let r = other.to_real_lossy();
            if r.fract() == 0.0 && r.abs() < 9.2e18 {
                Ok(Some(r as i64))
            } else {
                Err(mismatch())
            }
        }
    }
}

/// `sqlite3_value_int()` of a value whose numeric type is INTEGER, else `None`.
fn config_int_arg(v: &Value) -> Option<i32> {
    numeric_integer_value(Some(v)).map(|i| i as i32)
}

/// `sqlite3Fts5ConfigParseRank`: `bareword ( literal, ... )`.
fn config_rank_is_valid(z: &[u8]) -> bool {
    let p = cfg_skip_ws(z);
    let Some(n) = cfg_bareword_len(p) else {
        return false;
    };
    let p = cfg_skip_ws(&p[n..]);
    if p.first() != Some(&b'(') {
        return false;
    }
    let mut p = cfg_skip_ws(&p[1..]);
    if p.first() == Some(&b')') {
        return true;
    }
    loop {
        p = cfg_skip_ws(p);
        let Some(n) = cfg_literal_len(p) else {
            return false;
        };
        p = cfg_skip_ws(&p[n..]);
        match p.first() {
            Some(b')') => return true,
            Some(b',') => p = &p[1..],
            _ => return false,
        }
    }
}

/// `sqlite3Fts5ConfigSetValue`: false for an unknown key or a value the key does not accept.
fn config_value_is_valid(key: &str, val: &Value) -> bool {
    let int = config_int_arg(val);
    if key.eq_ignore_ascii_case("pgsz") {
        int.is_some_and(|v| (32..=64 * 1024).contains(&v))
    } else if key.eq_ignore_ascii_case("hashsize") {
        int.is_some_and(|v| v > 0)
    } else if key.eq_ignore_ascii_case("automerge") {
        int.is_some_and(|v| (0..=64).contains(&v))
    } else if key.eq_ignore_ascii_case("usermerge") {
        int.is_some_and(|v| (2..=16).contains(&v))
    } else if key.eq_ignore_ascii_case("crisismerge") {
        int.is_some_and(|v| v >= 0)
    } else if key.eq_ignore_ascii_case("deletemerge") {
        int.is_some()
    } else if key.eq_ignore_ascii_case("rank") {
        fts5_value_bytes(val).is_some_and(|z| config_rank_is_valid(&z))
    } else if key.eq_ignore_ascii_case("secure-delete") || key.eq_ignore_ascii_case("insttoken") {
        int.is_some_and(|v| v >= 0)
    } else {
        false
    }
}

impl VirtualTable for Fts5Table {
    /// `fts5BestIndexMethod`. `idxNum` carries the ORDER BY flags (`FTS5_BI_ORDER_*`), `idxStr`
    /// one item per xFilter argument: `M<col>` (MATCH; col == number of columns for the table
    /// itself), `r` (rank), `L<col>` / `G<col>` (LIKE / GLOB served by a trigram tokenizer),
    /// `=`, `<`, `>` (rowid constraints).
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let n_col = self.hidden_tbl_col();
        let rank_col = self.hidden_rank_col();
        let mut idx_flags = 0i32;
        let mut idx_str = String::new();
        let mut i_cons = 0usize;
        let mut b_seen_eq = false;
        let mut b_seen_gt = false;
        let mut b_seen_lt = false;
        let mut n_seen_match = 0usize;
        let mut b_seen_rank = false;

        for i in 0..info.constraints.len() {
            let (op, i_col, usable) = {
                let c = &info.constraints[i];
                (c.op, c.i_column, c.usable)
            };
            if op == VTabConstraintOp::Match || (op == VTabConstraintOp::Eq && i_col >= n_col) {
                // A MATCH operator or equivalent
                if !usable || i_col < 0 {
                    // As there exists an unusable MATCH constraint this is an unusable plan.
                    return Err(SqliteError::new(
                        ErrorCode::Constraint,
                        "unusable fts5 match constraint",
                    ));
                }
                if i_col == rank_col {
                    if b_seen_rank {
                        continue;
                    }
                    idx_str.push('r');
                    b_seen_rank = true;
                } else {
                    n_seen_match += 1;
                    idx_str.push('M');
                    idx_str.push_str(&i_col.to_string());
                }
                i_cons += 1;
                info.constraint_usage[i].argv_index = i_cons;
                info.constraint_usage[i].omit = true;
            } else if usable {
                let pattern_ok = i_col >= 0
                    && i_col < n_col
                    && matches!(op, VTabConstraintOp::Like | VTabConstraintOp::Glob)
                    && match self.cfg.tokenizer.tok.pattern() {
                        // `fts5UsePatternMatch`: a case-insensitive trigram tokenizer without
                        // diacritic removal serves LIKE and GLOB, a case-sensitive one GLOB.
                        TokenizerPattern::Like => true,
                        TokenizerPattern::Glob => op == VTabConstraintOp::Glob,
                        TokenizerPattern::None => false,
                    };
                if pattern_ok {
                    idx_str.push(if op == VTabConstraintOp::Like {
                        'L'
                    } else {
                        'G'
                    });
                    idx_str.push_str(&i_col.to_string());
                    i_cons += 1;
                    info.constraint_usage[i].argv_index = i_cons;
                    n_seen_match += 1;
                } else if !b_seen_eq && op == VTabConstraintOp::Eq && i_col < 0 {
                    idx_str.push('=');
                    b_seen_eq = true;
                    i_cons += 1;
                    info.constraint_usage[i].argv_index = i_cons;
                    info.constraint_usage[i].omit = true;
                }
            }
        }

        if !b_seen_eq {
            for i in 0..info.constraints.len() {
                let (op, i_col, usable) = {
                    let c = &info.constraints[i];
                    (c.op, c.i_column, c.usable)
                };
                if i_col < 0 && usable {
                    if matches!(op, VTabConstraintOp::Lt | VTabConstraintOp::Le) {
                        if b_seen_lt {
                            continue;
                        }
                        idx_str.push('<');
                        i_cons += 1;
                        info.constraint_usage[i].argv_index = i_cons;
                        b_seen_lt = true;
                    } else if matches!(op, VTabConstraintOp::Gt | VTabConstraintOp::Ge) {
                        if b_seen_gt {
                            continue;
                        }
                        idx_str.push('>');
                        i_cons += 1;
                        info.constraint_usage[i].argv_index = i_cons;
                        b_seen_gt = true;
                    }
                }
            }
        }

        // Set idxFlags flags for the ORDER BY clause
        if info.order_by.len() == 1 {
            let ob = info.order_by[0];
            if ob.i_column == rank_col && n_seen_match > 0 {
                idx_flags |= FTS5_BI_ORDER_RANK;
            } else if ob.i_column == -1 && (!ob.desc || !self.cfg.tokendata) {
                // Note that tokendata=1 tables cannot currently handle "ORDER BY rowid DESC".
                idx_flags |= FTS5_BI_ORDER_ROWID;
            }
            if (idx_flags & (FTS5_BI_ORDER_RANK | FTS5_BI_ORDER_ROWID)) != 0 {
                info.order_by_consumed = true;
                if ob.desc {
                    idx_flags |= FTS5_BI_ORDER_DESC;
                }
            }
        }

        // Calculate the estimated cost based on the flags set in idxFlags.
        if b_seen_eq {
            info.estimated_cost = if n_seen_match > 0 { 25000.0 } else { 25.0 };
            info.estimated_rows = 1;
            info.idx_flags |= 1; // SQLITE_INDEX_SCAN_UNIQUE
        } else {
            let mut n_est_rows: i64;
            if n_seen_match > 0 {
                if b_seen_lt && b_seen_gt {
                    info.estimated_cost = 50000.0;
                } else if b_seen_lt || b_seen_gt {
                    info.estimated_cost = 37500.0;
                } else {
                    info.estimated_cost = 50000.0;
                }
                n_est_rows = (info.estimated_cost / 40.0) as i64;
                for _ in 1..n_seen_match {
                    info.estimated_cost *= 2.5;
                    n_est_rows /= 2;
                }
            } else {
                if b_seen_lt && b_seen_gt {
                    info.estimated_cost = 750_000.0;
                } else if b_seen_lt || b_seen_gt {
                    info.estimated_cost = 2_250_000.0;
                } else {
                    info.estimated_cost = 3_000_000.0;
                }
                n_est_rows = (info.estimated_cost / 4.0) as i64;
            }
            info.estimated_rows = n_est_rows;
        }

        // What C finds out lazily in xColumn, the cursor of this implementation has to
        // know when it is filtered: whether column values and the rank are read at all.
        let n_vis = self.cfg.columns.len();
        let vis_mask = if n_vis == 0 {
            0u64
        } else if n_vis >= 64 {
            u64::MAX
        } else {
            (1u64 << n_vis) - 1
        };
        let mut hint = 0u64;
        if (info.col_used & vis_mask) != 0 {
            hint |= PLAN_HINT_NEEDS_CONTENT;
        }
        let rank_bit = 1u64 << (rank_col as usize).min(63);
        // A `rank MATCH '...'` term alone does not make C call the ranking function: it is
        // looked up when the rank column is read or the sorter statement is prepared.
        if (info.col_used & rank_bit) != 0 {
            hint |= PLAN_HINT_NEEDS_RANK_COL;
        }
        if (info.col_used & (1u64 << n_vis.min(63))) != 0 {
            hint |= PLAN_HINT_NEEDS_TBL_COL;
        }
        info.plan_hint = hint;

        info.idx_num = idx_flags;
        info.idx_str = Some(idx_str);
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        // fts5OpenMethod
        Ok(Box::new(main::Fts5Cursor::open(
            &self.cfg_arc,
            &self.state,
            &self.aux_registry,
        )))
    }

    fn begin(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        // `fts5BeginMethod` -> `fts5NewTransaction`: unless a cursor is open on the table,
        // `sqlite3Fts5IndexReset` drops the cached structure when the database has changed
        // since it was read.
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.n_open_csr == 0 {
            st.index.reset(ctx)?;
        }
        Ok(())
    }

    fn sync(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::flush_to_disk(&mut st, ctx)
    }

    /// `fts5CommitMethod`.
    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.i_savepoint = 0;
        Ok(())
    }

    /// `fts5RollbackMethod`.
    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.storage_rollback();
        st.index.config.pgsz = 0;
        st.i_savepoint = 0;
        Ok(())
    }

    /// `fts5SavepointMethod`.
    fn savepoint(&mut self, ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        sqliters_types::dtrace!("TR|f5|hook|savepoint i={}", idx as i64);
        // `pConfig->pzErrmsg` is not set in xSavepoint / xRelease: only the code survives.
        Self::flush_to_disk(&mut st, ctx).map_err(rc_only)?;
        st.i_savepoint = idx as i64 + 1;
        Ok(())
    }

    /// `fts5ReleaseMethod`.
    fn release(&mut self, ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let flush = (idx as i64 + 1) < st.i_savepoint;
        sqliters_types::dtrace!(
            "TR|f5|hook|release i={} flush={}",
            idx as i64,
            u8::from(flush)
        );
        if flush {
            Self::flush_to_disk(&mut st, ctx).map_err(rc_only)?;
            st.i_savepoint = idx as i64;
        }
        Ok(())
    }

    /// `fts5RollbackToMethod`.
    fn rollback_to(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        // fts5TripCursors(pTab)
        self.aux_registry.trip_cursors(&self.state);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let discard = (idx as i64 + 1) <= st.i_savepoint;
        sqliters_types::dtrace!(
            "TR|f5|hook|rollbackto i={} discard={}",
            idx as i64,
            u8::from(discard)
        );
        if discard {
            st.index.config.pgsz = 0;
            st.storage_rollback();
        }
        // The shadow tables have gone back to the savepoint.
        Ok(())
    }

    /// `fts5IntegrityMethod`: `sqlite3Fts5StorageIntegrity(pStorage, 0)`; the text of the
    /// index's own error message if it left one, otherwise the generic message.
    fn integrity(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        schema: &str,
        table: &str,
        _is_quick: bool,
    ) -> Result<Option<String>> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match self.verify_integrity(&mut st, ctx, false) {
            Ok(()) => Ok(None),
            Err(e) if e.code == ErrorCode::Corrupt => {
                if e.message.starts_with("fts5: ") {
                    Ok(Some(e.message))
                } else {
                    Ok(Some(format!(
                        "malformed inverted index for FTS5 table {schema}.{table}"
                    )))
                }
            }
            Err(e) => Err(e),
        }
    }

    /// The pending data of this table is written out before an `fts5vocab` cursor on it is
    /// opened (`fts5VocabOpenMethod` -> `sqlite3Fts5FlushToDisk`).
    fn flush_before_shadow_access(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::flush_to_disk(&mut st, ctx)
    }

    fn invalidate_shadow_cache(&mut self) {
        // C keeps nothing that depends on what other statements of the connection write
        // to the shadow tables, except the structure record it caches until the data
        // version of the database changes (`sqlite3Fts5IndexReset`) - and so does this.
    }

    /// `fts5UpdateMethod`.
    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        // fts5TripCursors(pTab)
        self.aux_registry.trip_cursors(&self.state);
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.index.config.pgsz == 0 {
            let cookie = st.index.config.i_cookie;
            st.index.config_load(ctx, cookie)?;
        }
        let n_cols = self.cfg.columns.len();
        let name = st.shadows.table_name.clone();
        // fts5IsContentless(pTab, 1)
        let contentless = self.cfg.content_mode == ContentMode::Contentless;
        // The conflict mode only matters where a rowid conflict can be detected.
        let conflict =
            if self.cfg.content_mode == ContentMode::Normal || self.cfg.contentless_delete {
                ctx.vtab_on_conflict()
            } else {
                VTabConflictMode::Abort
            };
        // It is an error to write an fts5_locale() value to a table without the locale=1
        // option.
        // (a "special" INSERT — a value in the table-name column — is not checked)
        let special = matches!(&op, VTabUpdateOperation::Insert { values, .. }
            if values.get(n_cols).is_some_and(|v| !v.is_null()));
        if !self.cfg.locale
            && !special
            && let VTabUpdateOperation::Insert { values, .. }
            | VTabUpdateOperation::Update { values, .. } = &op
            && values.iter().take(n_cols).any(|v| self.cfg.is_locale_value(v))
        {
            return Err(SqliteError::new(ErrorCode::Mismatch, "fts5_locale() requires locale=1"));
        }
        match op {
            VTabUpdateOperation::Insert { new_rowid, values } => {
                // A "special" INSERT: any non-NULL value in the table-name column.
                if let Some(cmd_val) = values.get(n_cols)
                    && !cmd_val.is_null()
                {
                    let cmd = fts5_value_text(cmd_val).unwrap_or_default().into_owned();
                    self.execute_special_command(&mut st, ctx, &cmd, new_rowid, &values)?;
                    // (xUpdate leaves *pRowid at 0: OP_VUpdate stores it in db->lastRowid)
                    return Ok(Some(0));
                }
                let new_rowid = new_rowid.filter(|v| !v.is_null());
                if conflict == VTabConflictMode::Replace
                    && let Some(rid) = numeric_integer_value(new_rowid.as_ref())
                {
                    self.storage_delete(&mut st, ctx, rid, None)?;
                }
                let rid =
                    self.storage_insert(&mut st, ctx, new_rowid.as_ref(), &values[..n_cols], &[])?;
                Ok(Some(rid))
            }
            VTabUpdateOperation::Delete { old_rowid } => {
                // It is only possible to DELETE from a contentless table if the
                // contentless_delete=1 flag is set.
                if contentless && !self.cfg.contentless_delete {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot DELETE from contentless fts5 table: {name}"),
                    ));
                }
                let rid = old_rowid.to_integer_lossy();
                self.storage_delete(&mut st, ctx, rid, None)?;
                Ok(None)
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                let old_rid = old_rowid.to_integer_lossy();
                let new_rid_opt = match &new_rowid {
                    Value::Null => Some(old_rid),
                    other => numeric_integer_value(Some(other)),
                };
                let new_rid = new_rid_opt.unwrap_or(old_rid);
                let col_vals = &values[..n_cols];

                // `fts5ContentlessUpdate`
                let mut content_only = false;
                if contentless {
                    let mut seen_index = false;
                    let mut seen_index_nc = false;
                    for i in 0..n_cols {
                        if !self.cfg.unindexed[i] {
                            if ctx.vtab_nochange(i) {
                                seen_index_nc = true;
                            } else {
                                seen_index = true;
                            }
                        }
                    }
                    if !seen_index && old_rid == new_rid {
                        content_only = true;
                    } else if seen_index_nc || !self.cfg.contentless_delete {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            if self.cfg.contentless_delete {
                                format!(
                                    "cannot UPDATE a subset of columns on fts5 contentless-delete table: {name}"
                                )
                            } else {
                                format!("cannot UPDATE contentless fts5 table: {name}")
                            },
                        ));
                    }
                }

                if new_rid_opt.is_none() {
                    return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                }
                let new_val = Value::Integer(new_rid);
                // `Fts5Storage.pSavedRow`: once the old row was looked up with "save", the
                // columns the UPDATE does not assign (`sqlite3_value_nochange`) are taken
                // from it, with their `l<N>` values; without a saved row they are NULL.
                let nochange: Vec<bool> = (0..n_cols).map(|i| ctx.vtab_nochange(i)).collect();
                let resolve = |st: &Fts5SharedState| -> (Vec<Value>, Vec<Value>) {
                    let mut cols = Vec::with_capacity(n_cols);
                    let mut locs = vec![Value::Null; n_cols];
                    for i in 0..n_cols {
                        if nochange[i] {
                            match st.saved_row.as_ref() {
                                Some(saved) => {
                                    cols.push(saved.cols.get(i).cloned().unwrap_or(Value::Null));
                                    locs[i] = saved.locs.get(i).cloned().unwrap_or(Value::Null);
                                }
                                None => cols.push(Value::Null),
                            }
                        } else {
                            cols.push(col_vals[i].clone());
                        }
                    }
                    (cols, locs)
                };
                let res = (|| -> Result<()> {
                    if old_rid != new_rid {
                        if conflict == VTabConflictMode::Replace {
                            st.want_saved_row = true;
                            self.storage_delete(&mut st, ctx, old_rid, None)?;
                            self.storage_delete(&mut st, ctx, new_rid, None)?;
                            let (cols, locs) = resolve(&st);
                            self.storage_insert(&mut st, ctx, Some(&new_val), &cols, &locs)?;
                        } else {
                            // sqlite3Fts5StorageFindDeleteRow, then: a conflict on the new
                            // rowid is detected before the index is touched.
                            st.saved_row = self.read_saved_row(&st, ctx, old_rid)?;
                            let (cols, locs) = resolve(&st);
                            self.content_insert(&mut st, ctx, false, Some(&new_val), &cols, &locs)?;
                            self.storage_delete(&mut st, ctx, old_rid, None)?;
                            self.index_insert(&mut st, ctx, new_rid, &cols)?;
                        }
                    } else if content_only {
                        // Only UNINDEXED columns of a contentless table change: a write to
                        // %_content for contentless_unindexed=1 tables, a no-op otherwise.
                        if self.cfg.unindexed_content {
                            st.saved_row = self.read_saved_row(&st, ctx, old_rid)?;
                            if st.saved_row.is_some() {
                                let (cols, locs) = resolve(&st);
                                self.content_insert(&mut st, ctx, true, Some(&new_val), &cols, &locs)?;
                            }
                        }
                    } else {
                        st.want_saved_row = true;
                        self.storage_delete(&mut st, ctx, old_rid, None)?;
                        let (cols, locs) = resolve(&st);
                        self.storage_insert(&mut st, ctx, Some(&new_val), &cols, &locs)?;
                    }
                    Ok(())
                })();
                // sqlite3Fts5StorageReleaseDeleteRow
                st.saved_row = None;
                st.want_saved_row = false;
                res?;
                Ok(Some(new_rid))
            }
        }
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // `sqlite3Fts5StorageRename`
        st.storage_sync(ctx)?;
        let db_q = quote_ident(ctx.db_name());
        let old_name = st.shadows.table_name.clone();
        let mut suffixes = vec!["data", "idx", "config"];
        if self.cfg.columnsize {
            suffixes.push("docsize");
        }
        if self.cfg.has_content_table() {
            suffixes.push("content");
        }
        for suf in suffixes {
            let old_s = quote_ident(&format!("{old_name}_{suf}"));
            let new_s = quote_ident(&format!("{new_name}_{suf}"));
            ctx.execute_sql(
                &format!("ALTER TABLE {db_q}.{old_s} RENAME TO {new_s}"),
                &[],
            )?;
        }
        let db_name = ctx.db_name().to_owned();
        st.set_names(&db_name, new_name, &self.cfg);
        Ok(())
    }

    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.storage_rollback();
        let db_q = quote_ident(ctx.db_name());
        // `sqlite3Fts5DropAll`: data, idx, config, then docsize, then content.
        for suf in ["data", "idx", "config", "docsize", "content"] {
            let sq = quote_ident(&format!("{}_{suf}", st.shadows.table_name));
            ctx.execute_sql(&format!("DROP TABLE IF EXISTS {db_q}.{sq}"), &[])?;
        }
        Ok(())
    }
}

// ============================================================================
// `Fts5VocabModule` (`fts5vocab` Virtual Table: `'row'`, `'col'`, `'instance'`)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VocabType {
    Row,
    Col,
    Instance,
}

/// Built-in `fts5vocab` virtual table module for inspecting an `fts5` table's inverted index.
#[derive(Debug, Default, Clone)]
pub struct Fts5VocabModule {
    aux_registry: Arc<Fts5AuxRegistry>,
}

impl Fts5VocabModule {
    /// `sqlite3Fts5VocabInit(pGlobal, db)`.
    pub fn new(aux_registry: Arc<Fts5AuxRegistry>) -> Self {
        Self { aux_registry }
    }
}

impl VirtualTableModule for Fts5VocabModule {
    fn name(&self) -> &str {
        "fts5vocab"
    }

    /// (`fts5Vocab` has no `xUpdate`)
    fn is_read_only(&self) -> bool {
        true
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.connect(ctx, args)
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        // fts5VocabInitVtab: the (db, table, type) form only for a table in the temp schema
        let b_db = args.len() == 3 && ctx.db_name() == "temp";
        let (target_db, target_tbl, vtype) = match args.len() {
            _ if args.len() != 2 && !b_db => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "wrong number of vtable arguments",
                ));
            }
            2 => {
                let tbl = unquote_sql_token(args[0].trim())?;
                let ty = parse_vocab_type(&unquote_sql_token(args[1].trim())?)?;
                (None, tbl, ty)
            }
            3 => {
                let db = unquote_sql_token(args[0].trim())?;
                let tbl = unquote_sql_token(args[1].trim())?;
                let ty = parse_vocab_type(&unquote_sql_token(args[2].trim())?)?;
                (Some(db), tbl, ty)
            }
            _ => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "wrong number of vtable arguments",
                ));
            }
        };

        let decl = match vtype {
            VocabType::Row => "CREATE TABLE x(term, doc, cnt)",
            VocabType::Col => "CREATE TABLE x(term, col, doc, cnt)",
            VocabType::Instance => "CREATE TABLE x(term, doc, col, offset)",
        };
        ctx.declare_vtab(decl)?;
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
        Ok(Box::new(Fts5VocabTable {
            aux_registry: Arc::clone(&self.aux_registry),
            target_db,
            target_tbl,
            vtype,
        }))
    }
}

fn unquote_sql_token(s: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&cfg_dequote_word(s.as_bytes())).into_owned())
}

fn parse_vocab_type(s: &str) -> Result<VocabType> {
    match s {
        "row" => Ok(VocabType::Row),
        "col" => Ok(VocabType::Col),
        "instance" => Ok(VocabType::Instance),
        _ => Err(SqliteError::new(
            ErrorCode::Error,
            "fts5vocab: unknown table type",
        )),
    }
}

#[derive(Debug)]
struct Fts5VocabTable {
    aux_registry: Arc<Fts5AuxRegistry>,
    target_db: Option<String>,
    target_tbl: String,
    vtype: VocabType,
}

impl VirtualTable for Fts5VocabTable {
    /// fts5_vocab.c `fts5VocabBestIndexMethod`: `idxNum` = the `colUsed` mask plus
    /// `FTS5_VOCAB_TERM_EQ` (0x100) / `_GE` (0x200) / `_LE` (0x400) for the usable constraints
    /// on the `term` column (the last one of each kind wins); cost 100 with an equality,
    /// otherwise 1000000 halved per range end; `ORDER BY term [ASC]` alone is consumed. No
    /// constraint is omitted, so the core tests every term again.
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut i_term_eq = None;
        let mut i_term_ge = None;
        let mut i_term_le = None;
        let mut idx_num = (info.col_used & 0xFF) as i32;
        for (i, c) in info.constraints.iter().enumerate() {
            if !c.usable || c.i_column != 0 {
                continue;
            }
            match c.op {
                VTabConstraintOp::Eq => i_term_eq = Some(i),
                VTabConstraintOp::Le | VTabConstraintOp::Lt => i_term_le = Some(i),
                VTabConstraintOp::Ge | VTabConstraintOp::Gt => i_term_ge = Some(i),
                _ => {}
            }
        }
        let mut n_arg = 0usize;
        if let Some(i) = i_term_eq {
            idx_num |= 0x0100;
            n_arg += 1;
            info.constraint_usage[i].argv_index = n_arg;
            info.estimated_cost = 100.0;
        } else {
            info.estimated_cost = 1_000_000.0;
            if let Some(i) = i_term_ge {
                idx_num |= 0x0200;
                n_arg += 1;
                info.constraint_usage[i].argv_index = n_arg;
                info.estimated_cost /= 2.0;
            }
            if let Some(i) = i_term_le {
                idx_num |= 0x0400;
                n_arg += 1;
                info.constraint_usage[i].argv_index = n_arg;
                info.estimated_cost /= 2.0;
            }
        }
        if info.order_by.len() == 1 && info.order_by[0].i_column == 0 && !info.order_by[0].desc {
            info.order_by_consumed = true;
        }
        info.idx_num = idx_num;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(vocab::Fts5VocabCursor::new(
            Arc::clone(&self.aux_registry),
            self.target_db.clone(),
            self.target_tbl.clone(),
            self.vtype,
        )))
    }
}


mod auxfn;
mod expr;
mod main;
mod vocab;
pub use main::{Fts5AuxRegistry, Fts5AuxTarget};
