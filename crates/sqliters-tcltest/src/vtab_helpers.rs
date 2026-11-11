//! Pure safe Rust test virtual table modules and static test extensions for `sqliters-tcltest`
//! (`wholenumber`, `strdup`, `randomjson`, `eval`, `schema`, `tclvar`, `tcl`, `echo`).
#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

use sqliters_core::{Connection, FunctionFlags, RuntimeValue};
use sqliters_types::{
    ErrorCode, Result, SqliteError, VTabConfigOption, VTabConstraintOp, VTabConstraintUsage,
    VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor,
    VirtualTable, VirtualTableModule, format_sqlite_real,
};

use crate::expr::{parse_tcl_bool, parse_tcl_float, parse_tcl_int};
use crate::harness::{eval_global_in_active_interp, sqlite_err_text, with_active_interp};
use crate::interp::TclControl;
use crate::list::{format_list, parse_list};

fn dequote_vtab_arg(raw: &str) -> String {
    let s = raw.trim();
    if s.len() < 2 {
        return s.to_string();
    }
    let bytes = s.as_bytes();
    let close = match bytes[0] {
        b'\'' => b'\'',
        b'"' => b'"',
        b'`' => b'`',
        b'[' => b']',
        _ => return s.to_string(),
    };
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s[1..].chars().collect();
    let close_ch = close as char;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == close_ch {
            if i + 1 < chars.len() && chars[i + 1] == close_ch {
                out.push(close_ch);
                i += 2;
            } else {
                break;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn quote_sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn quote_sql_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn value_as_tcl_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(n) => n.to_string(),
        Value::Real(r) => format_sqlite_real(*r),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

// ============================================================================
// 1. `wholenumber` (`sqlite/ext/misc/wholenumber.c`)
// ============================================================================

pub struct WholenumberModule;

struct WholenumberTable;

#[derive(Default)]
struct WholenumberCursor {
    cur_val: i64,
    max_val: i64,
}

impl VirtualTableModule for WholenumberModule {
    fn name(&self) -> &str {
        "wholenumber"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        ctx.declare_vtab("CREATE TABLE x(value)")?;
        ctx.set_vtab_config(VTabConfigOption::Innocuous)?;
        Ok(Box::new(WholenumberTable))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.create(ctx, args)
    }
}

impl VirtualTable for WholenumberTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut idx_num = 0i32;
        let mut argv_idx = 1usize;
        let mut lower_idx: Option<usize> = None;
        let mut upper_idx: Option<usize> = None;

        for (i, c) in info.constraints.iter().enumerate() {
            if !c.usable {
                continue;
            }
            if (idx_num & 3) == 0 && c.op == VTabConstraintOp::Gt {
                idx_num |= 1;
                lower_idx = Some(i);
            }
            if (idx_num & 3) == 0 && c.op == VTabConstraintOp::Ge {
                idx_num |= 2;
                lower_idx = Some(i);
            }
            if (idx_num & 12) == 0 && c.op == VTabConstraintOp::Lt {
                idx_num |= 4;
                upper_idx = Some(i);
            }
            if (idx_num & 12) == 0 && c.op == VTabConstraintOp::Le {
                idx_num |= 8;
                upper_idx = Some(i);
            }
        }

        info.idx_num = idx_num;
        if let Some(li) = lower_idx {
            info.constraint_usage[li] = VTabConstraintUsage {
                argv_index: argv_idx,
                omit: true,
            };
            argv_idx += 1;
        }
        if let Some(ui) = upper_idx {
            info.constraint_usage[ui] = VTabConstraintUsage {
                argv_index: argv_idx,
                omit: true,
            };
        }
        if info.order_by.len() == 1 && !info.order_by[0].desc {
            info.order_by_consumed = true;
        }
        info.estimated_cost = if (idx_num & 12) == 0 {
            1e99
        } else if (idx_num & 3) == 0 {
            5.0
        } else {
            1.0
        };
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(WholenumberCursor::default()))
    }
}

impl VirtualCursor for WholenumberCursor {
    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.cur_val = 1;
        self.max_val = 0xffff_ffff;
        let mut arg_idx = 0usize;
        if (idx_num & 3) != 0 {
            let mut v = args.get(arg_idx).map_or(0, Value::to_integer_lossy);
            if v <= self.max_val && (idx_num & 1) != 0 {
                v = v.saturating_add(1);
            }
            if v > self.cur_val && v <= self.max_val {
                self.cur_val = v;
            }
            arg_idx += 1;
        }
        if (idx_num & 12) != 0 {
            let mut v = args.get(arg_idx).map_or(0, Value::to_integer_lossy);
            if v > 0 && ((idx_num >> 2) & 1) != 0 {
                v -= 1;
            }
            if v >= self.cur_val && v < self.max_val {
                self.max_val = v;
            }
            if v < self.cur_val {
                // Empty range: set max_val < cur_val so eof() is true immediately.
                self.max_val = self.cur_val - 1;
            }
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.cur_val = self.cur_val.saturating_add(1);
        Ok(())
    }

    fn eof(&self) -> bool {
        self.cur_val > self.max_val || self.cur_val <= 0
    }

    fn column(&self, _col_idx: usize) -> Result<Value> {
        Ok(Value::Integer(self.cur_val))
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.cur_val)
    }
}

// ============================================================================
// 2. Static Test Extensions (`strdup`, `randomjson`, `eval`)
// ============================================================================

pub fn register_strdup_extension(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "strdup",
        1,
        FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
        |args| {
            let v = match &args[0].value {
                Value::Null => Value::Null,
                Value::Blob(b) => Value::Blob(b.clone()),
                other => Value::Text(value_as_tcl_text(other)),
            };
            Ok(RuntimeValue::plain(v))
        },
    )
}

#[derive(Clone, Copy)]
struct RandomJsonPrng {
    x: u32,
    y: u32,
}

impl RandomJsonPrng {
    fn new(seed: u32) -> Self {
        Self {
            x: seed | 1,
            y: seed,
        }
    }

    fn next_u32(&mut self) -> u32 {
        let lsb_mask = (1u32.wrapping_add(!(self.x & 1))) & 0xd000_0001;
        self.x = (self.x >> 1) ^ lsb_mask;
        self.y = self.y.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        self.x ^ self.y
    }
}

const JSON_ATOMS: &[&str] = &[
    "0",
    "0",
    "1",
    "1",
    "-1",
    "-1",
    "2",
    "+2",
    "3DDDD",
    "3DDDD",
    "2.5DD",
    "2.5DD",
    "0.75",
    ".75",
    "-4.0e2",
    "-4.e2",
    "5.0e-3",
    "+5e-3",
    "6.DDe+0DD",
    "6.DDe+0DD",
    "0",
    "0x0",
    "512",
    "0x200",
    "256",
    "+0x100",
    "-2748",
    "-0xabc",
    "true",
    "true",
    "false",
    "false",
    "null",
    "null",
    "9.0e999",
    "Infinity",
    "-9.0e999",
    "-Infinity",
    "9.0e999",
    "+Infinity",
    "null",
    "NaN",
    "-0.0005DD",
    "-0.0005DD",
    "4.35e-3",
    "+4.35e-3",
    "\"gem\\\"hay\"",
    "\"gem\\\"hay\"",
    "\"icy'joy\"",
    "'icy\\'joy'",
    "\"keylog\"",
    "\"key\\\nlog\"",
    "\"mix\\\\\\tnet\"",
    "\"mix\\\\\\tnet\"",
    "\"oat\\r\\n\"",
    "\"oat\\r\\n\"",
    "\"\\fpan\\b\"",
    "\"\\fpan\\b\"",
    "{}",
    "{}",
    "[]",
    "[]",
    "[]",
    "[/*empty*/]",
    "{}",
    "{//empty\n}",
    "\"ask\"",
    "\"ask\"",
    "\"bag\"",
    "\"bag\"",
    "\"can\"",
    "\"can\"",
    "\"day\"",
    "\"day\"",
    "\"end\"",
    "'end'",
    "\"fly\"",
    "\"fly\"",
    "\"\\u00XX\\u00XX\"",
    "\"\\xXX\\xXX\"",
    "\"y\\uXXXXz\"",
    "\"y\\uXXXXz\"",
    "\"\"",
    "\"\"",
];

const JSON_TEMPLATES: &[&str] = &[
    "{\"a\":%,\"b\":%,\"cDD\":%}",
    "{a:%,b:%,cDD:%}",
    "{\"a\":%,\"b\":%,\"c\":%,\"d\":%,\"e\":%}",
    "{a:%,b:%,c:%,d:%,e:%}",
    "{\"a\":%,\"b\":%,\"c\":%,\"d\":%,\"\":%}",
    "{a:%,b:%,c:%,d:%,'':%}",
    "{\"d\":%}",
    "{d:%}",
    "{\"eeee\":%, \"ffff\":%}",
    "{eeee:% /*and*/, ffff:%}",
    "{\"$g\":%,\"_h_\":%,\"a b c d\":%}",
    "{$g:%,_h_:%,\"a b c d\":%}",
    "{\"x\":%,\n  \"y\":%}",
    "{\"x\":%,\n  \"y\":%}",
    "{\"\\u00XX\":%,\"\\uXXXX\":%}",
    "{\"\\xXX\":%,\"\\uXXXX\":%}",
    "{\"Z\":%}",
    "{Z:%,}",
    "[%]",
    "[%,]",
    "[%,%]",
    "[%,%]",
    "[%,%,%]",
    "[%,%,%,]",
    "[%,%,%,%]",
    "[%,%,%,%]",
    "[%,%,%,%,%]",
    "[%,%,%,%,%]",
];

fn random_json_expand(src: &str, prng: &mut RandomJsonPrng, e_type: usize, mut r: u32) -> String {
    const STRSZ: usize = 10000;
    let mut dest: Vec<u8> = Vec::with_capacity(1024);
    if src.len() >= STRSZ / 10 {
        r = 0;
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    const DEC: &[u8; 10] = b"0123456789";

    for &b in src.as_bytes() {
        if b != b'%' {
            if dest.len() < STRSZ {
                dest.push(b);
            }
            continue;
        }
        let tpl = if r == 0 || (r < 1000 && (prng.next_u32() % 1000) <= r) {
            let k = (prng.next_u32() as usize) % (JSON_ATOMS.len() / 2);
            JSON_ATOMS[k * 2 + e_type]
        } else {
            let k = (prng.next_u32() as usize) % (JSON_TEMPLATES.len() / 2);
            JSON_TEMPLATES[k * 2 + e_type]
        };
        let mut buf = tpl.as_bytes().to_vec();
        if tpl.contains("XX") {
            let mut y = prng.next_u32();
            if (y & 0xff) == ((y >> 8) & 0xff) {
                y = y.wrapping_add(0x100);
            }
            while (y & 0xff) == ((y >> 16) & 0xff) || ((y >> 8) & 0xff) == ((y >> 16) & 0xff) {
                y = y.wrapping_add(0x1_0000);
            }
            let mut idx = 0;
            while idx + 1 < buf.len() {
                if buf[idx] == b'X' && buf[idx + 1] == b'X' {
                    buf[idx] = HEX[(y % 16) as usize];
                    y /= 16;
                    buf[idx + 1] = HEX[(y % 16) as usize];
                    y /= 16;
                } else {
                    idx += 1;
                }
            }
        } else if tpl.contains("DD") {
            let mut y = prng.next_u32();
            let mut idx = 0;
            while idx + 1 < buf.len() {
                if buf[idx] == b'D' && buf[idx + 1] == b'D' {
                    buf[idx] = DEC[(y % 10) as usize];
                    y /= 10;
                    buf[idx + 1] = DEC[(y % 10) as usize];
                    y /= 10;
                } else {
                    idx += 1;
                }
            }
        }
        if dest.len() + buf.len() < STRSZ {
            dest.extend_from_slice(&buf);
        }
    }
    if dest.len() >= STRSZ {
        dest.truncate(STRSZ - 1);
    }
    String::from_utf8_lossy(&dest).into_owned()
}

fn generate_random_json(seed_val: &Value, e_type: usize) -> String {
    let seed = seed_val.to_integer_lossy() as i32 as u32;
    let mut prng = RandomJsonPrng::new(seed);
    let z2 = random_json_expand("%", &mut prng, e_type, 1000);
    let z1 = random_json_expand(&z2, &mut prng, e_type, 1000);
    let z2 = random_json_expand(&z1, &mut prng, e_type, 100);
    random_json_expand(&z2, &mut prng, e_type, 0)
}

pub fn register_randomjson_extension(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "random_json",
        1,
        FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
        |args| {
            Ok(RuntimeValue::plain(Value::Text(generate_random_json(
                &args[0].value,
                0,
            ))))
        },
    )?;
    conn.create_scalar_function(
        "random_json5",
        1,
        FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
        |args| {
            Ok(RuntimeValue::plain(Value::Text(generate_random_json(
                &args[0].value,
                1,
            ))))
        },
    )
}

pub fn register_eval_extension(conn: &Connection, db_cmd_name: String) -> Result<()> {
    for arity in [1i32, 2i32] {
        let db_name = db_cmd_name.clone();
        conn.create_scalar_function("eval", arity, FunctionFlags::DIRECTONLY, move |args| {
            if args[0].value.is_null() {
                return Ok(RuntimeValue::plain(Value::Null));
            }
            let sql = value_as_tcl_text(&args[0].value);
            let sep = if args.len() > 1 {
                if args[1].value.is_null() {
                    return Ok(RuntimeValue::plain(Value::Null));
                }
                value_as_tcl_text(&args[1].value)
            } else {
                " ".to_string()
            };
            let script = format!(
                "{} eval {}",
                format_list(std::slice::from_ref(&db_name)),
                format_list(&[sql])
            );
            match eval_global_in_active_interp(&script) {
                Some((Ok(res_list), _)) => {
                    let items = parse_list(&res_list).unwrap_or_default();
                    Ok(RuntimeValue::plain(Value::Text(items.join(&sep))))
                }
                Some((Err(ctrl), _)) => Err(SqliteError::new(ErrorCode::Error, ctrl.err_msg())),
                None => Err(SqliteError::misuse("no active Tcl interpreter for eval()")),
            }
        })?;
    }
    Ok(())
}

// ============================================================================
// 3. `schema` Test Module (`sqlite/src/test_schema.c`)
// ============================================================================

pub struct SchemaTestModule;

struct SchemaTestTable;

#[derive(Default)]
struct SchemaTestCursor {
    rows: Vec<Vec<Value>>,
    pos: usize,
}

impl VirtualTableModule for SchemaTestModule {
    fn name(&self) -> &str {
        "schema"
    }

    fn is_eponymous(&self) -> bool {
        true
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        ctx.declare_vtab(
            "CREATE TABLE x(database, tablename, cid, name, type, not_null, dflt_value, pk)",
        )?;
        Ok(Box::new(SchemaTestTable))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.create(ctx, args)
    }
}

impl VirtualTable for SchemaTestTable {
    fn best_index(&self, _info: &mut VTabIndexInfo) -> Result<()> {
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(SchemaTestCursor::default()))
    }
}

impl VirtualCursor for SchemaTestCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        _idx_num: i32,
        _idx_str: Option<&str>,
        _args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        let dbs = ctx.query_sql("PRAGMA database_list", &[])?;
        for db_row in dbs {
            let seq = db_row.first().map_or(0, Value::to_integer_lossy);
            let db_name = db_row.get(1).map_or_else(String::new, value_as_tcl_text);
            let tbl_sql = if seq == 1 {
                "SELECT name FROM sqlite_temp_schema WHERE type='table'".to_string()
            } else {
                format!(
                    "SELECT name FROM {}.sqlite_schema WHERE type='table'",
                    quote_sql_ident(&db_name)
                )
            };
            let tbls = ctx.query_sql(&tbl_sql, &[])?;
            for tbl_row in tbls {
                let tbl_name = tbl_row.first().map_or_else(String::new, value_as_tcl_text);
                let info_sql = format!(
                    "PRAGMA {}.table_info({})",
                    quote_sql_ident(&db_name),
                    quote_sql_ident(&tbl_name)
                );
                let cols = ctx.query_sql(&info_sql, &[])?;
                for col in cols {
                    let mut row = Vec::with_capacity(8);
                    row.push(Value::Text(db_name.clone()));
                    row.push(Value::Text(tbl_name.clone()));
                    for j in 0..6 {
                        row.push(col.get(j).cloned().unwrap_or(Value::Null));
                    }
                    self.rows.push(row);
                }
            }
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        Ok(self
            .rows
            .get(self.pos)
            .and_then(|r| r.get(col_idx))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        Ok((self.pos + 1) as i64)
    }
}

// ============================================================================
// 4. `tclvar` Test Module (`sqlite/src/test_tclvar.c`)
// ============================================================================

pub const TCLVAR_HELPER_PROCS: &str = r#"
proc like {pattern str} {
  set p [string map {% * _ ?} $pattern]
  string match $p $str
}
proc tclvar_filter_cmd {eq match glob regexp like} {
  set res {}
  set pattern $eq
  if {$pattern=={}} { set pattern $match }
  if {$pattern=={}} { set pattern * }
  foreach v [uplevel #0 info vars $pattern] {
    if {![uplevel #0 array exists $v]} {
      if {($glob=={} || [string match $glob [uplevel #0 set $v]])
       && ($like=={} || [like $like [uplevel #0 set $v]])
       && ($regexp=={} || [regexp $regexp [uplevel #0 set $v]])
      } {
        lappend res $v
      }
    } else {
      lappend res $v
    }
  }
  set res
}
"#;

pub struct TclvarTestModule;

struct TclvarTestTable;

#[derive(Default)]
struct TclvarTestCursor {
    rows: Vec<[String; 4]>,
    pos: usize,
}

fn tclvar_set_omit() -> bool {
    with_active_interp(|interp| {
        interp
            .get_var("::tclvar_set_omit")
            .ok()
            .and_then(|v| parse_tcl_bool(&v).ok())
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

impl VirtualTableModule for TclvarTestModule {
    fn name(&self) -> &str {
        "tclvar"
    }

    fn is_eponymous(&self) -> bool {
        true
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        ctx.declare_vtab(
            "CREATE TABLE x(name TEXT, arrayname TEXT, value TEXT, fullname TEXT PRIMARY KEY) WITHOUT ROWID",
        )?;
        Ok(Box::new(TclvarTestTable))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.create(ctx, args)
    }
}

impl VirtualTable for TclvarTestTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut z_str = String::new();
        let mut argv_idx = 0usize;

        for (i, c) in info.constraints.iter().enumerate() {
            if !c.usable {
                continue;
            }
            if c.op == VTabConstraintOp::Eq && c.i_column == 0 && !z_str.contains('e') {
                z_str.push('e');
                argv_idx += 1;
                info.constraint_usage[i] = VTabConstraintUsage {
                    argv_index: argv_idx,
                    omit: false,
                };
            } else if c.op == VTabConstraintOp::Match && c.i_column == 0 && !z_str.contains('m') {
                z_str.push('m');
                argv_idx += 1;
                info.constraint_usage[i] = VTabConstraintUsage {
                    argv_index: argv_idx,
                    omit: true,
                };
            } else if c.op == VTabConstraintOp::Glob && c.i_column == 2 && !z_str.contains('g') {
                z_str.push('g');
                argv_idx += 1;
                info.constraint_usage[i] = VTabConstraintUsage {
                    argv_index: argv_idx,
                    omit: tclvar_set_omit(),
                };
            } else if c.op == VTabConstraintOp::Regexp && c.i_column == 2 && !z_str.contains('r') {
                z_str.push('r');
                argv_idx += 1;
                info.constraint_usage[i] = VTabConstraintUsage {
                    argv_index: argv_idx,
                    omit: tclvar_set_omit(),
                };
            } else if c.op == VTabConstraintOp::Like && c.i_column == 2 && !z_str.contains('l') {
                z_str.push('l');
                argv_idx += 1;
                info.constraint_usage[i] = VTabConstraintUsage {
                    argv_index: argv_idx,
                    omit: tclvar_set_omit(),
                };
            }
        }
        info.idx_str = Some(z_str);
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(TclvarTestCursor::default()))
    }

    fn update(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        match op {
            VTabUpdateOperation::Delete { old_rowid } => {
                let var_name = value_as_tcl_text(&old_rowid);
                with_active_interp(|interp| {
                    let full = format!("::{}", var_name.trim_start_matches("::"));
                    let _ = interp.unset_var(&full, true);
                });
                Ok(None)
            }
            VTabUpdateOperation::Insert { values, .. } => {
                let fullname_val = values.get(3).cloned().unwrap_or(Value::Null);
                let Value::Text(z_name) = fullname_val else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "the 'fullname' column must be TEXT",
                    ));
                };
                let full = format!("::{}", z_name.trim_start_matches("::"));
                let val_opt = values.get(2).cloned().unwrap_or(Value::Null);
                with_active_interp(|interp| {
                    if val_opt.is_null() {
                        let _ = interp.unset_var(&full, true);
                    } else {
                        let _ = interp.set_var(&full, value_as_tcl_text(&val_opt));
                    }
                });
                Ok(None)
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                let (Value::Text(old_name), Value::Text(new_name)) = (old_rowid, new_rowid) else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "prohibited TCL variable change",
                    ));
                };
                let val_opt = values.get(2).cloned().unwrap_or(Value::Null);
                with_active_interp(|interp| {
                    let old_full = format!("::{}", old_name.trim_start_matches("::"));
                    let new_full = format!("::{}", new_name.trim_start_matches("::"));
                    if old_name != new_name || val_opt.is_null() {
                        let _ = interp.unset_var(&old_full, true);
                    }
                    if !val_opt.is_null() {
                        let _ = interp.set_var(&new_full, value_as_tcl_text(&val_opt));
                    }
                });
                Ok(None)
            }
        }
    }
}

impl VirtualCursor for TclvarTestCursor {
    fn filter(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        _idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;

        let mut z_eq = String::new();
        let mut z_match = String::new();
        let mut z_glob = String::new();
        let mut z_regexp = String::new();
        let mut z_like = String::new();

        if let Some(s) = idx_str {
            for (i, ch) in s.chars().enumerate() {
                let txt = args.get(i).map_or_else(String::new, value_as_tcl_text);
                match ch {
                    'e' => z_eq = txt,
                    'm' => z_match = txt,
                    'g' => z_glob = txt,
                    'r' => z_regexp = txt,
                    'l' => z_like = txt,
                    _ => {}
                }
            }
        }

        let cmd = format!(
            "tclvar_filter_cmd {}",
            format_list(&[z_eq, z_match, z_glob, z_regexp, z_like])
        );
        let var_names = match eval_global_in_active_interp(&cmd) {
            Some((Ok(list_str), _)) => parse_list(&list_str).unwrap_or_default(),
            _ => Vec::new(),
        };

        for v in var_names {
            let clean = v.trim_start_matches("::").to_string();
            let arr_cmd = format!("array names {}", format_list(&[format!("::{clean}")]));
            let arr_keys = match eval_global_in_active_interp(&arr_cmd) {
                Some((Ok(k_str), _)) if !k_str.is_empty() => parse_list(&k_str).unwrap_or_default(),
                _ => Vec::new(),
            };
            if arr_keys.is_empty() {
                if let Some(Some(val)) =
                    with_active_interp(|interp| interp.get_var(&format!("::{clean}")).ok())
                {
                    self.rows.push([clean.clone(), String::new(), val, clean]);
                }
            } else {
                for key in arr_keys {
                    let full_lookup = format!("::{clean}({key})");
                    if let Some(Some(val)) =
                        with_active_interp(|interp| interp.get_var(&full_lookup).ok())
                    {
                        let fullname = format!("{clean}({key})");
                        self.rows.push([clean.clone(), key, val, fullname]);
                    }
                }
            }
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        Ok(self
            .rows
            .get(self.pos)
            .and_then(|r| r.get(col_idx))
            .map(|s| Value::Text(s.clone()))
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        Ok(0)
    }
}

// ============================================================================
// 5. `tcl` Test Module (`sqlite/src/test_bestindex.c`)
// ============================================================================

static NEXT_BESTINDEX_HDL: AtomicUsize = AtomicUsize::new(43);

pub struct TclVtabTestModule {
    pub default_cmd: Option<String>,
    pub allow_update: bool,
}

struct TclVtabTestTable {
    cmd: String,
    allow_update: bool,
}

#[derive(Default)]
struct TclVtabTestCursor {
    rows: Vec<Vec<Value>>,
    pos: usize,
}

fn constraint_op_tcl_name(op: VTabConstraintOp) -> String {
    match op {
        VTabConstraintOp::Eq => "eq".to_string(),
        VTabConstraintOp::Gt => "gt".to_string(),
        VTabConstraintOp::Le => "le".to_string(),
        VTabConstraintOp::Lt => "lt".to_string(),
        VTabConstraintOp::Ge => "ge".to_string(),
        VTabConstraintOp::Match => "match".to_string(),
        VTabConstraintOp::Like => "like".to_string(),
        VTabConstraintOp::Glob => "glob".to_string(),
        VTabConstraintOp::Regexp => "regexp".to_string(),
        VTabConstraintOp::Ne => "ne".to_string(),
        VTabConstraintOp::IsNot => "isnot".to_string(),
        VTabConstraintOp::IsNotNull => "isnotnull".to_string(),
        VTabConstraintOp::IsNull => "isnull".to_string(),
        VTabConstraintOp::Is => "is".to_string(),
        VTabConstraintOp::Limit => "limit".to_string(),
        VTabConstraintOp::Offset => "offset".to_string(),
        VTabConstraintOp::Function(n) => (150u16 + u16::from(n)).to_string(),
    }
}

impl VirtualTableModule for TclVtabTestModule {
    fn name(&self) -> &str {
        "tcl"
    }

    fn is_eponymous(&self) -> bool {
        self.default_cmd.is_some()
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        let cmd = match args.len() {
            1 => dequote_vtab_arg(&args[0]),
            0 if self.default_cmd.is_some() => self.default_cmd.clone().unwrap(),
            _ => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "wrong number of arguments",
                ));
            }
        };
        let script = format!("{cmd} xConnect");
        let schema_sql = match eval_global_in_active_interp(&script) {
            Some((Ok(s), _)) => s,
            Some((Err(ctrl), _)) => {
                let msg = ctrl.err_msg();
                let code = if msg.eq_ignore_ascii_case("database schema has changed") {
                    ErrorCode::Schema
                } else {
                    ErrorCode::Error
                };
                return Err(SqliteError::new(code, msg));
            }
            None => return Err(SqliteError::misuse("no active Tcl interpreter")),
        };
        ctx.declare_vtab(&schema_sql).map_err(|e| {
            SqliteError::new(e.code, format!("declare_vtab: {}", sqlite_err_text(&e)))
        })?;
        Ok(Box::new(TclVtabTestTable {
            cmd,
            allow_update: self.allow_update,
        }))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.create(ctx, args)
    }
}

impl VirtualTable for TclVtabTestTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let hdl_num = NEXT_BESTINDEX_HDL.fetch_add(1, Ordering::Relaxed);
        let hdl_name = format!("bestindex{hdl_num}");

        let constraints_tcl: Vec<String> = info
            .constraints
            .iter()
            .map(|c| {
                format_list(&[
                    "op".to_string(),
                    constraint_op_tcl_name(c.op),
                    "column".to_string(),
                    c.i_column.to_string(),
                    "usable".to_string(),
                    i32::from(c.usable).to_string(),
                ])
            })
            .collect();
        let constraints_str = format_list(&constraints_tcl);

        let orderby_tcl: Vec<String> = info
            .order_by
            .iter()
            .map(|o| {
                format_list(&[
                    "column".to_string(),
                    o.i_column.to_string(),
                    "desc".to_string(),
                    i32::from(o.desc).to_string(),
                ])
            })
            .collect();
        let orderby_str = format_list(&orderby_tcl);
        let mask_str = (info.col_used as i64).to_string();

        let in_flags: Vec<String> = info
            .constraints
            .iter()
            .map(|c| i32::from(c.in_values).to_string())
            .collect();
        let in_flags_str = format_list(&in_flags);

        let proc_def = format!(
            "proc {hdl_name} {{sub args}} {{\n\
               switch -- $sub {{\n\
                 constraints {{ return {} }}\n\
                 orderby {{ return {} }}\n\
                 mask {{ return {mask_str} }}\n\
                 distinct {{ return 0 }}\n\
                 in {{\n\
                   set idx [lindex $args 0]\n\
                   return [lindex {} $idx]\n\
                 }}\n\
                 rhs_value {{\n\
                   if {{[llength $args] >= 2}} {{ return [lindex $args 1] }}\n\
                   return \"\"\n\
                 }}\n\
                 collation {{ return \"BINARY\" }}\n\
                 default {{ error \"bad sub-command $sub\" }}\n\
               }}\n\
             }}",
            format_list(&[constraints_str]),
            format_list(&[orderby_str]),
            format_list(&[in_flags_str]),
        );

        let call_script = format!(
            "{proc_def}\n\
             set __bi_rc [catch {{{} xBestIndex {hdl_name}}} __bi_res]\n\
             catch {{rename {hdl_name} \"\"}}\n\
             if {{$__bi_rc}} {{ error $__bi_res }}\n\
             return $__bi_res",
            self.cmd
        );

        let res_str = match eval_global_in_active_interp(&call_script) {
            Some((Ok(s), _)) => s,
            Some((Err(TclControl::Return { value, .. }), _)) => value,
            Some((Err(ctrl), _)) => return Err(SqliteError::new(ErrorCode::Error, ctrl.err_msg())),
            None => return Err(SqliteError::misuse("no active Tcl interpreter")),
        };

        let elems = parse_list(&res_str).map_err(|e| SqliteError::new(ErrorCode::Error, e))?;
        if elems.len() % 2 != 0 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("unexpected bestindex return list: {res_str}"),
            ));
        }

        let mut i_argv = 1usize;
        let mut i = 0usize;
        while i + 1 < elems.len() {
            let key = elems[i].as_str();
            let val = elems[i + 1].as_str();
            if key.eq_ignore_ascii_case("cost") {
                info.estimated_cost = parse_tcl_float(val).ok_or_else(|| {
                    SqliteError::new(ErrorCode::Error, format!("bad cost: {val}"))
                })?;
            } else if key.eq_ignore_ascii_case("orderby") {
                let v = parse_tcl_int(val).ok_or_else(|| {
                    SqliteError::new(ErrorCode::Error, format!("bad orderby: {val}"))
                })?;
                info.order_by_consumed = v != 0;
            } else if key.eq_ignore_ascii_case("idxnum") {
                let v = parse_tcl_int(val).ok_or_else(|| {
                    SqliteError::new(ErrorCode::Error, format!("bad idxnum: {val}"))
                })?;
                info.idx_num = v as i32;
            } else if key.eq_ignore_ascii_case("idxstr") {
                info.idx_str = Some(val.to_string());
            } else if key.eq_ignore_ascii_case("rows") {
                info.estimated_rows = parse_tcl_int(val).ok_or_else(|| {
                    SqliteError::new(ErrorCode::Error, format!("bad rows: {val}"))
                })?;
            } else if key.eq_ignore_ascii_case("use") || key.eq_ignore_ascii_case("omit") {
                let idx = parse_tcl_int(val).ok_or_else(|| {
                    SqliteError::new(ErrorCode::Error, format!("bad constraint index: {val}"))
                })?;
                if idx < 0 || (idx as usize) >= info.constraint_usage.len() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("unexpected: {idx}"),
                    ));
                }
                info.constraint_usage[idx as usize] = VTabConstraintUsage {
                    argv_index: i_argv,
                    omit: key.eq_ignore_ascii_case("omit"),
                };
                i_argv += 1;
            } else if key.eq_ignore_ascii_case("constraint") {
                return Err(SqliteError::new(ErrorCode::Constraint, val.to_string()));
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unexpected: {key}"),
                ));
            }
            i += 2;
        }
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        let cmd = self.cmd.clone();
        Ok(Box::new(TclVtabBoundCursor {
            cmd,
            inner: TclVtabTestCursor::default(),
        }))
    }

    fn update(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        _op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        if !self.allow_update {
            return Err(SqliteError::new(
                ErrorCode::ReadOnly,
                "table may not be modified",
            ));
        }
        let script = format!("{} xUpdate", self.cmd);
        match eval_global_in_active_interp(&script) {
            Some((Ok(s), _)) => {
                let rid = parse_tcl_int(s.trim()).ok_or_else(|| {
                    SqliteError::new(
                        ErrorCode::Error,
                        format!("expected integer but got \"{s}\""),
                    )
                })?;
                Ok(Some(rid))
            }
            Some((Err(ctrl), _)) => Err(SqliteError::new(ErrorCode::Error, ctrl.err_msg())),
            None => Err(SqliteError::misuse("no active Tcl interpreter")),
        }
    }
}

struct TclVtabBoundCursor {
    cmd: String,
    inner: TclVtabTestCursor,
}

impl VirtualCursor for TclVtabBoundCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.inner.rows.clear();
        self.inner.pos = 0;

        let arg_strs: Vec<String> = args.iter().map(value_as_tcl_text).collect();
        let call = format!(
            "{} xFilter {} {} {}",
            self.cmd,
            idx_num,
            format_list(&[idx_str.unwrap_or("").to_string()]),
            format_list(&[format_list(&arg_strs)])
        );
        let res_str = match eval_global_in_active_interp(&call) {
            Some((Ok(s), _)) => s,
            Some((Err(ctrl), _)) => return Err(SqliteError::new(ErrorCode::Error, ctrl.err_msg())),
            None => return Err(SqliteError::misuse("no active Tcl interpreter")),
        };
        let elems = parse_list(&res_str).map_err(|e| SqliteError::new(ErrorCode::Error, e))?;
        let mut i = 0usize;
        while i + 1 < elems.len() {
            let key = elems[i].as_str();
            let val = elems[i + 1].as_str();
            if key.eq_ignore_ascii_case("sql") {
                let rows = ctx.query_sql(val, &[]).map_err(|e| {
                    SqliteError::new(e.code, format!("unexpected: {}", sqlite_err_text(&e)))
                })?;
                self.inner.rows = rows;
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unexpected: {key}"),
                ));
            }
            i += 2;
        }
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.inner.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.inner.pos >= self.inner.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        Ok(self
            .inner
            .rows
            .get(self.inner.pos)
            .and_then(|r| r.get(col_idx + 1))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self
            .inner
            .rows
            .get(self.inner.pos)
            .and_then(|r| r.first())
            .map_or(0, Value::to_integer_lossy))
    }
}

// ============================================================================
// 6. `echo` / `echo_v2` Test Module (`sqlite/src/test8.c`)
// ============================================================================

pub struct EchoTestModule {
    pub module_name: &'static str,
}

impl EchoTestModule {
    pub const fn new(module_name: &'static str) -> Self {
        Self { module_name }
    }
}

struct EchoSharedMeta {
    this_name: std::sync::Mutex<String>,
    backing_table: std::sync::Mutex<Option<String>>,
    log_table: Option<String>,
    is_pattern: bool,
    cols: Vec<String>,
    has_index: Vec<bool>,
    row_count: AtomicI64,
    in_tx: AtomicBool,
}

struct EchoTestTable {
    meta: Arc<EchoSharedMeta>,
}

struct EchoTestCursor {
    meta: Arc<EchoSharedMeta>,
    rows: Vec<Vec<Value>>,
    pos: usize,
}

fn append_to_echo_module(elem: &str) {
    with_active_interp(|interp| {
        let cur = interp.get_var("::echo_module").unwrap_or_default();
        let mut items = if cur.is_empty() {
            Vec::new()
        } else {
            parse_list(&cur).unwrap_or_default()
        };
        items.push(elem.to_string());
        let _ = interp.set_var("::echo_module", format_list(&items));
    });
}

fn check_echo_simulated_error(method: &str, backing: Option<&str>) -> Result<()> {
    let Some(tbl) = backing else {
        return Ok(());
    };
    let key = format!("::echo_module_fail({method},{tbl})");
    if let Some(Some(err_msg)) = with_active_interp(|interp| interp.get_var(&key).ok()) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("echo-vtab-error: {err_msg}"),
        ));
    }
    Ok(())
}

fn echo_hash_string(s: &str) -> i32 {
    let mut val = 0u32;
    for &b in s.as_bytes() {
        val = val.wrapping_shl(3).wrapping_add(u32::from(b));
    }
    (val & 0x7fff_ffff) as i32
}

impl EchoTestModule {
    fn construct(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
        is_create: bool,
    ) -> Result<Box<dyn VirtualTable>> {
        append_to_echo_module(if is_create { "xCreate" } else { "xConnect" });

        let this_name = ctx.table_name().to_string();
        let mut is_pattern = false;
        let backing_table = if let Some(first) = args.first() {
            let deq = dequote_vtab_arg(first);
            if let Some(rest) = deq.strip_prefix('*') {
                is_pattern = true;
                Some(format!("{this_name}{rest}"))
            } else {
                Some(deq)
            }
        } else {
            None
        };

        append_to_echo_module(self.module_name);
        append_to_echo_module(ctx.db_name());
        append_to_echo_module(&this_name);
        for a in args {
            append_to_echo_module(a);
        }

        let mut cols = Vec::new();
        let mut has_index = Vec::new();
        let mut initial_rows = 0i64;

        if let Some(ref tbl) = backing_table {
            let mut schema_rows = ctx.query_sql(
                "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?",
                &[Value::Text(tbl.clone())],
            )?;
            if schema_rows.is_empty() {
                schema_rows = ctx.query_sql(
                    "SELECT sql FROM sqlite_temp_schema WHERE type = 'table' AND name = ?",
                    &[Value::Text(tbl.clone())],
                )?;
            }
            let Some(first_row) = schema_rows.first() else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {tbl}"),
                ));
            };
            let create_sql = first_row
                .first()
                .map_or_else(String::new, value_as_tcl_text);
            ctx.declare_vtab(&create_sql)?;

            let ti_rows =
                ctx.query_sql(&format!("PRAGMA table_info({})", quote_sql_ident(tbl)), &[])?;
            for r in ti_rows {
                cols.push(r.get(1).map_or_else(String::new, value_as_tcl_text));
            }
            has_index.resize(cols.len(), false);

            let il_rows =
                ctx.query_sql(&format!("PRAGMA index_list({})", quote_sql_ident(tbl)), &[])?;
            for r in il_rows {
                let idx_name = r.get(1).map_or_else(String::new, value_as_tcl_text);
                if idx_name.is_empty() {
                    continue;
                }
                let ii_rows = ctx.query_sql(
                    &format!("PRAGMA index_info({})", quote_sql_ident(&idx_name)),
                    &[],
                )?;
                if let Some(first_col) = ii_rows.first() {
                    let cid = first_col.get(1).map_or(-1, Value::to_integer_lossy);
                    if cid >= 0 && (cid as usize) < has_index.len() {
                        has_index[cid as usize] = true;
                    }
                }
            }

            if let Ok(cnt_rows) =
                ctx.query_sql(&format!("SELECT count(*) FROM {}", quote_sql_str(tbl)), &[])
            {
                initial_rows = cnt_rows
                    .first()
                    .and_then(|r| r.first())
                    .map_or(0, Value::to_integer_lossy);
            }
        }

        let mut log_table = None;
        if is_create && args.len() == 2 {
            let log_name = args[1].clone();
            ctx.execute_sql(
                &format!("CREATE TABLE {}(logmsg)", quote_sql_str(&log_name)),
                &[],
            )?;
            log_table = Some(log_name);
        }

        let meta = Arc::new(EchoSharedMeta {
            this_name: std::sync::Mutex::new(this_name),
            backing_table: std::sync::Mutex::new(backing_table),
            log_table,
            is_pattern,
            cols,
            has_index,
            row_count: AtomicI64::new(initial_rows),
            in_tx: AtomicBool::new(is_create),
        });
        Ok(Box::new(EchoTestTable { meta }))
    }
}

impl VirtualTableModule for EchoTestModule {
    fn name(&self) -> &str {
        self.module_name
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.construct(ctx, args, true)
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.construct(ctx, args, false)
    }
}

impl VirtualTable for EchoTestTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xBestIndex", backing.as_deref())?;

        let (ignore_usable, custom_cost) = with_active_interp(|interp| {
            let ign = interp.get_var("::echo_module_ignore_usable").is_ok();
            let cost_opt = interp
                .get_var("::echo_module_cost")
                .ok()
                .and_then(|s| parse_tcl_float(&s));
            (ign, cost_opt)
        })
        .unwrap_or((false, None));

        let tbl = backing.unwrap_or_default();
        let n_row = self.meta.row_count.load(Ordering::Relaxed).max(0);

        let mut z_col = String::new();
        for (i, col_name) in self.meta.cols.iter().enumerate() {
            let bit = 1u64 << i.min(63);
            if (info.col_used & bit) != 0 {
                z_col.push_str(", ");
                z_col.push_str(col_name);
            } else {
                z_col.push_str(", NULL");
            }
        }
        let mut z_query = format!("SELECT rowid{z_col} FROM {}", quote_sql_str(&tbl));
        let mut z_sep = "WHERE";
        let mut n_arg = 0usize;
        let mut use_idx = false;

        for (i, c) in info.constraints.iter().enumerate() {
            if !ignore_usable && !c.usable {
                continue;
            }
            let i_col = c.i_column;
            let indexed = i_col < 0
                || (i_col >= 0
                    && self
                        .meta
                        .has_index
                        .get(i_col as usize)
                        .copied()
                        .unwrap_or(false));
            if !indexed {
                continue;
            }
            let col_expr = if i_col >= 0 {
                self.meta
                    .cols
                    .get(i_col as usize)
                    .map_or("rowid", String::as_str)
            } else {
                "rowid"
            };
            let op_opt = match c.op {
                VTabConstraintOp::Eq => Some("="),
                VTabConstraintOp::Lt => Some("<"),
                VTabConstraintOp::Gt => Some(">"),
                VTabConstraintOp::Le => Some("<="),
                VTabConstraintOp::Ge => Some(">="),
                VTabConstraintOp::Match => Some("LIKE"),
                VTabConstraintOp::Like => Some("like"),
                VTabConstraintOp::Glob => Some("glob"),
                VTabConstraintOp::Regexp => Some("regexp"),
                _ => None,
            };
            if let Some(z_op) = op_opt {
                use_idx = true;
                if z_op == "LIKE" {
                    z_query.push_str(&format!(" {z_sep} {col_expr} LIKE (SELECT '%'||?||'%')"));
                } else {
                    z_query.push_str(&format!(" {z_sep} {col_expr} {z_op} ?"));
                }
                z_sep = "AND";
                n_arg += 1;
                info.constraint_usage[i] = VTabConstraintUsage {
                    argv_index: n_arg,
                    omit: true,
                };
            }
        }

        if info.order_by.len() == 1 {
            let ob = info.order_by[0];
            let indexed = ob.i_column < 0
                || (ob.i_column >= 0
                    && self
                        .meta
                        .has_index
                        .get(ob.i_column as usize)
                        .copied()
                        .unwrap_or(false));
            if indexed {
                let col_expr = if ob.i_column >= 0 {
                    self.meta
                        .cols
                        .get(ob.i_column as usize)
                        .map_or("rowid", String::as_str)
                } else {
                    "rowid"
                };
                let dir = if ob.desc { "DESC" } else { "ASC" };
                z_query.push_str(&format!(" ORDER BY {col_expr} {dir}"));
                info.order_by_consumed = true;
            }
        }

        append_to_echo_module("xBestIndex");
        append_to_echo_module(&z_query);

        info.idx_num = echo_hash_string(&z_query);
        info.idx_str = Some(z_query);
        info.estimated_cost = if let Some(cost) = custom_cost {
            cost
        } else if use_idx {
            let mut log2_est = 0.0;
            for bit in 0..31 {
                if (n_row & (1i64 << bit)) != 0 {
                    log2_est = f64::from(bit);
                }
            }
            log2_est
        } else {
            n_row as f64
        };
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xOpen", backing.as_deref())?;
        Ok(Box::new(EchoTestCursor {
            meta: Arc::clone(&self.meta),
            rows: Vec::new(),
            pos: 0,
        }))
    }

    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xUpdate", backing.as_deref())?;
        let Some(tbl) = backing else {
            return Err(SqliteError::new(ErrorCode::Error, "no backing table"));
        };

        let mut returned_rowid = None;
        let dml_res: Result<()> = match op {
            VTabUpdateOperation::Delete { old_rowid } => {
                let sql = format!("DELETE FROM {} WHERE rowid = ?1", quote_sql_str(&tbl));
                ctx.execute_sql(&sql, &[old_rowid]).map(|_| ())
            }
            VTabUpdateOperation::Insert { new_rowid, values } => {
                let mut col_list = Vec::new();
                let mut placeholders = Vec::new();
                let mut params = Vec::new();
                if let Some(rid) = new_rowid.filter(|v| matches!(v, Value::Integer(_))) {
                    col_list.push("rowid".to_string());
                    params.push(rid);
                    placeholders.push(format!("?{}", params.len()));
                }
                for (i, v) in values.into_iter().enumerate() {
                    if let Some(cname) = self.meta.cols.get(i) {
                        col_list.push(quote_sql_str(cname));
                        params.push(v);
                        placeholders.push(format!("?{}", params.len()));
                    }
                }
                let sql = format!(
                    "INSERT INTO {} ({}) VALUES({})",
                    quote_sql_str(&tbl),
                    col_list.join(", "),
                    placeholders.join(", ")
                );
                ctx.execute_sql(&sql, &params)?;
                let rid_rows = ctx.query_sql("SELECT last_insert_rowid()", &[])?;
                returned_rowid = Some(
                    rid_rows
                        .first()
                        .and_then(|r| r.first())
                        .map_or(0, Value::to_integer_lossy),
                );
                Ok(())
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                let mut sets = Vec::new();
                let mut params = Vec::new();
                if matches!(new_rowid, Value::Integer(_)) {
                    params.push(new_rowid);
                    sets.push(format!("rowid=?{}", params.len()));
                }
                for (i, v) in values.into_iter().enumerate() {
                    if let Some(cname) = self.meta.cols.get(i) {
                        params.push(v);
                        sets.push(format!("{} = ?{}", quote_sql_str(cname), params.len()));
                    }
                }
                params.push(old_rowid);
                let sql = format!(
                    "UPDATE {} SET {} WHERE rowid=?{}",
                    quote_sql_str(&tbl),
                    sets.join(", "),
                    params.len()
                );
                ctx.execute_sql(&sql, &params).map(|_| ())
            }
        };

        if let Err(e) = dml_res {
            return Err(SqliteError::new(
                e.code,
                format!("echo-vtab-error: {}", sqlite_err_text(&e)),
            ));
        }

        if let Ok(cnt_rows) = ctx.query_sql(
            &format!("SELECT count(*) FROM {}", quote_sql_str(&tbl)),
            &[],
        ) {
            let cnt = cnt_rows
                .first()
                .and_then(|r| r.first())
                .map_or(0, Value::to_integer_lossy);
            self.meta.row_count.store(cnt, Ordering::Relaxed);
        }

        Ok(returned_rowid)
    }

    fn begin(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xBegin", backing.as_deref())?;
        let tbl = backing.unwrap_or_default();
        append_to_echo_module("xBegin");
        append_to_echo_module(&format!("echo({tbl})"));
        let begin_fail = with_active_interp(|interp| {
            interp
                .get_var("::echo_module_begin_fail")
                .ok()
                .is_some_and(|v| v == tbl)
        })
        .unwrap_or(false);
        if begin_fail {
            return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
        }
        self.meta.in_tx.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn sync(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xSync", backing.as_deref())?;
        let tbl = backing.unwrap_or_default();
        append_to_echo_module("xSync");
        append_to_echo_module(&format!("echo({tbl})"));
        let sync_fail = with_active_interp(|interp| {
            interp
                .get_var("::echo_module_sync_fail")
                .ok()
                .is_some_and(|v| v == tbl)
        })
        .unwrap_or(false);
        if sync_fail {
            return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
        }
        Ok(())
    }

    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xCommit", backing.as_deref())?;
        let tbl = backing.unwrap_or_default();
        append_to_echo_module("xCommit");
        append_to_echo_module(&format!("echo({tbl})"));
        self.meta.in_tx.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        append_to_echo_module("xRollback");
        append_to_echo_module(&format!("echo({backing})"));
        self.meta.in_tx.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn rename(&mut self, ctx: &mut dyn VTabHostContext, new_name: &str) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xRename", backing.as_deref())?;
        if self.meta.is_pattern
            && let Some(ref cur_backing) = backing
        {
            let cur_this = self
                .meta
                .this_name
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let suffix = cur_backing.strip_prefix(&cur_this).unwrap_or("");
            let new_backing = format!("{new_name}{suffix}");
            let sql = format!("ALTER TABLE {cur_backing} RENAME TO {new_backing}");
            ctx.execute_sql(&sql, &[])
                .map_err(|_| SqliteError::new(ErrorCode::Error, "SQL logic error"))?;
            *self
                .meta
                .this_name
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = new_name.to_string();
            *self
                .meta
                .backing_table
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(new_backing);
        }
        Ok(())
    }

    fn destroy(&mut self, ctx: &mut dyn VTabHostContext) -> Result<()> {
        append_to_echo_module("xDestroy");
        if let Some(ref log_tbl) = self.meta.log_table {
            ctx.execute_sql(&format!("DROP TABLE {}", quote_sql_str(log_tbl)), &[])?;
        }
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        append_to_echo_module("xDisconnect");
        Ok(())
    }
}

impl VirtualCursor for EchoTestCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xFilter", backing.as_deref())?;

        let z_sql = idx_str.unwrap_or("");
        debug_assert_eq!(idx_num, echo_hash_string(z_sql));

        append_to_echo_module("xFilter");
        append_to_echo_module(z_sql);
        for a in args {
            append_to_echo_module(&value_as_tcl_text(a));
        }

        self.rows = ctx.query_sql(z_sql, args)?;
        self.pos = 0;

        if let Some(ref tbl) = backing
            && let Ok(cnt_rows) =
                ctx.query_sql(&format!("SELECT count(*) FROM {}", quote_sql_str(tbl)), &[])
        {
            let cnt = cnt_rows
                .first()
                .and_then(|r| r.first())
                .map_or(0, Value::to_integer_lossy);
            self.meta.row_count.store(cnt, Ordering::Relaxed);
        }

        check_echo_simulated_error("xNext", backing.as_deref())?;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xNext", backing.as_deref())?;
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xColumn", backing.as_deref())?;
        Ok(self
            .rows
            .get(self.pos)
            .and_then(|r| r.get(col_idx + 1))
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn rowid(&self) -> Result<i64> {
        let backing = self
            .meta
            .backing_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        check_echo_simulated_error("xRowid", backing.as_deref())?;
        Ok(self
            .rows
            .get(self.pos)
            .and_then(|r| r.first())
            .map_or(0, Value::to_integer_lossy))
    }
}
