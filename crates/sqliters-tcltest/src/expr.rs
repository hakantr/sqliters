//! TCL `expr` expression parser and evaluator.

use crate::interp::{TclInterp, TclResult};
use crate::list::{parse_backslash_escape, parse_list};

#[derive(Debug, Clone)]
pub enum ExprVal {
    Int(i64),
    Float(f64),
    Str(String),
}

impl ExprVal {
    pub fn as_bool(&self) -> Result<bool, String> {
        match self {
            Self::Int(i) => Ok(*i != 0),
            Self::Float(f) => Ok(*f != 0.0),
            Self::Str(s) => parse_tcl_bool(s),
        }
    }

    pub fn as_int(&self) -> Result<i64, String> {
        match self {
            Self::Int(i) => Ok(*i),
            Self::Float(f) => Ok(*f as i64),
            Self::Str(s) => parse_tcl_int(s.trim())
                .ok_or_else(|| format!("expected integer but got \"{s}\"")),
        }
    }

    pub fn as_float(&self) -> Result<f64, String> {
        match self {
            Self::Int(i) => Ok(*i as f64),
            Self::Float(f) => Ok(*f),
            Self::Str(s) => parse_tcl_float(s.trim())
                .ok_or_else(|| format!("expected floating-point number but got \"{s}\"")),
        }
    }

    pub fn to_tcl_string(&self) -> String {
        match self {
            Self::Int(i) => i.to_string(),
            Self::Float(f) => {
                if f.is_nan() {
                    "NaN".to_string()
                } else if f.is_infinite() {
                    if f.is_sign_negative() {
                        "-Inf".to_string()
                    } else {
                        "Inf".to_string()
                    }
                } else {
                    let s = format!("{f}");
                    if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                        format!("{s}.0")
                    } else {
                        s
                    }
                }
            }
            Self::Str(s) => s.clone(),
        }
    }

    fn coerce_numeric(&self) -> Option<ExprVal> {
        match self {
            Self::Int(_) | Self::Float(_) => Some(self.clone()),
            Self::Str(s) => {
                let t = s.trim();
                if let Some(i) = parse_tcl_int(t) {
                    Some(ExprVal::Int(i))
                } else if let Some(f) = parse_tcl_float(t) {
                    Some(ExprVal::Float(f))
                } else if let Ok(b) = parse_tcl_bool_word(t) {
                    Some(ExprVal::Int(i64::from(b)))
                } else {
                    None
                }
            }
        }
    }
}

pub fn parse_tcl_bool(s: &str) -> Result<bool, String> {
    let t = s.trim();
    if let Some(i) = parse_tcl_int(t) {
        return Ok(i != 0);
    }
    if let Some(f) = parse_tcl_float(t) {
        return Ok(f != 0.0);
    }
    parse_tcl_bool_word(t)
}

fn parse_tcl_bool_word(t: &str) -> Result<bool, String> {
    if t.eq_ignore_ascii_case("true")
        || t.eq_ignore_ascii_case("yes")
        || t.eq_ignore_ascii_case("on")
    {
        Ok(true)
    } else if t.eq_ignore_ascii_case("false")
        || t.eq_ignore_ascii_case("no")
        || t.eq_ignore_ascii_case("off")
    {
        Ok(false)
    } else {
        Err(format!("expected boolean value but got \"{t}\""))
    }
}

pub fn parse_tcl_int(t: &str) -> Option<i64> {
    if t.is_empty() {
        return None;
    }
    let (neg, rest) = if let Some(r) = t.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = t.strip_prefix('+') {
        (false, r)
    } else {
        (false, t)
    };
    if rest.is_empty() {
        return None;
    }
    let abs_u: u64 = if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()?
    } else if let Some(oct) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
        u64::from_str_radix(oct, 8).ok()?
    } else if let Some(bin) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
        u64::from_str_radix(bin, 2).ok()?
    } else if rest.len() > 1
        && rest.starts_with('0')
        && rest.bytes().all(|b| (b'0'..=b'7').contains(&b))
    {
        u64::from_str_radix(rest, 8).ok()?
    } else if rest.bytes().all(|b| b.is_ascii_digit()) {
        rest.parse::<u64>().ok()?
    } else {
        return None;
    };

    if neg {
        if abs_u == (1u64 << 63) {
            Some(i64::MIN)
        } else if abs_u < (1u64 << 63) {
            Some(-(abs_u as i64))
        } else {
            None
        }
    } else {
        Some(abs_u as i64)
    }
}

pub fn parse_tcl_float(t: &str) -> Option<f64> {
    if t.is_empty() {
        return None;
    }
    if t.eq_ignore_ascii_case("inf")
        || t.eq_ignore_ascii_case("+inf")
        || t.eq_ignore_ascii_case("infinity")
    {
        return Some(f64::INFINITY);
    }
    if t.eq_ignore_ascii_case("-inf") || t.eq_ignore_ascii_case("-infinity") {
        return Some(f64::NEG_INFINITY);
    }
    if t.eq_ignore_ascii_case("nan") || t.eq_ignore_ascii_case("-nan") {
        return Some(f64::NAN);
    }
    t.parse::<f64>().ok()
}

pub fn eval_expr(interp: &mut TclInterp, src: &str) -> TclResult<ExprVal> {
    let mut p = ExprParser {
        interp,
        src,
        bytes: src.as_bytes(),
        pos: 0,
    };
    let val = p.parse_ternary(true)?;
    p.skip_ws();
    if p.pos < p.bytes.len() {
        return Err(crate::interp::TclControl::Error(format!(
            "syntax error in expression \"{src}\" near \"{}\"",
            &src[p.pos..]
        )));
    }
    Ok(val)
}

struct ExprParser<'a> {
    interp: &'a mut TclInterp,
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> ExprParser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if b == b'\\' && self.pos + 1 < self.bytes.len() && self.bytes[self.pos + 1] == b'\n' {
                self.pos += 2;
            } else {
                break;
            }
        }
    }

    fn err(&self, msg: impl Into<String>) -> crate::interp::TclControl {
        crate::interp::TclControl::Error(msg.into())
    }

    fn parse_ternary(&mut self, eval: bool) -> TclResult<ExprVal> {
        let cond = self.parse_or(eval)?;
        self.skip_ws();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'?' {
            self.pos += 1;
            let b = if eval {
                cond.as_bool().map_err(|e| self.err(e))?
            } else {
                false
            };
            let then_v = self.parse_ternary(eval && b)?;
            self.skip_ws();
            if self.pos >= self.bytes.len() || self.bytes[self.pos] != b':' {
                return Err(self.err("missing ':' in ternary expression"));
            }
            self.pos += 1;
            let else_v = self.parse_ternary(eval && !b)?;
            Ok(if b { then_v } else { else_v })
        } else {
            Ok(cond)
        }
    }

    fn parse_or(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_and(eval)?;
        loop {
            self.skip_ws();
            if self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos] == b'|'
                && self.bytes[self.pos + 1] == b'|'
            {
                self.pos += 2;
                let lb = if eval {
                    left.as_bool().map_err(|e| self.err(e))?
                } else {
                    false
                };
                let right = self.parse_and(eval && !lb)?;
                if eval {
                    let rb = if lb {
                        true
                    } else {
                        right.as_bool().map_err(|e| self.err(e))?
                    };
                    left = ExprVal::Int(i64::from(lb || rb));
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_and(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_bitor(eval)?;
        loop {
            self.skip_ws();
            if self.pos + 1 < self.bytes.len()
                && self.bytes[self.pos] == b'&'
                && self.bytes[self.pos + 1] == b'&'
            {
                self.pos += 2;
                let lb = if eval {
                    left.as_bool().map_err(|e| self.err(e))?
                } else {
                    false
                };
                let right = self.parse_bitor(eval && lb)?;
                if eval {
                    let rb = if lb {
                        right.as_bool().map_err(|e| self.err(e))?
                    } else {
                        false
                    };
                    left = ExprVal::Int(i64::from(lb && rb));
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_bitor(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_bitxor(eval)?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len()
                && self.bytes[self.pos] == b'|'
                && (self.pos + 1 >= self.bytes.len() || self.bytes[self.pos + 1] != b'|')
            {
                self.pos += 1;
                let right = self.parse_bitxor(eval)?;
                if eval {
                    let a = left.as_int().map_err(|e| self.err(e))?;
                    let b = right.as_int().map_err(|e| self.err(e))?;
                    left = ExprVal::Int(a | b);
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_bitxor(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_bitand(eval)?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len() && self.bytes[self.pos] == b'^' {
                self.pos += 1;
                let right = self.parse_bitand(eval)?;
                if eval {
                    let a = left.as_int().map_err(|e| self.err(e))?;
                    let b = right.as_int().map_err(|e| self.err(e))?;
                    left = ExprVal::Int(a ^ b);
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_bitand(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_eq(eval)?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len()
                && self.bytes[self.pos] == b'&'
                && (self.pos + 1 >= self.bytes.len() || self.bytes[self.pos + 1] != b'&')
            {
                self.pos += 1;
                let right = self.parse_eq(eval)?;
                if eval {
                    let a = left.as_int().map_err(|e| self.err(e))?;
                    let b = right.as_int().map_err(|e| self.err(e))?;
                    left = ExprVal::Int(a & b);
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_eq(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_rel(eval)?;
        loop {
            self.skip_ws();
            let op = if self.starts_with_bytes(b"==") {
                self.pos += 2;
                Some("==")
            } else if self.starts_with_bytes(b"!=") {
                self.pos += 2;
                Some("!=")
            } else if self.starts_with_kw("eq") {
                self.pos += 2;
                Some("eq")
            } else if self.starts_with_kw("ne") {
                self.pos += 2;
                Some("ne")
            } else if self.starts_with_kw("in") {
                self.pos += 2;
                Some("in")
            } else if self.starts_with_kw("ni") {
                self.pos += 2;
                Some("ni")
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.parse_rel(eval)?;
            if eval {
                let res = match op {
                    "eq" => left.to_tcl_string() == right.to_tcl_string(),
                    "ne" => left.to_tcl_string() != right.to_tcl_string(),
                    "in" | "ni" => {
                        let needle = left.to_tcl_string();
                        let haystack = parse_list(&right.to_tcl_string())
                            .map_err(|e| self.err(e))?;
                        let found = haystack.iter().any(|x| x == &needle);
                        if op == "in" { found } else { !found }
                    }
                    "==" => cmp_tcl_vals(&left, &right) == Some(std::cmp::Ordering::Equal),
                    "!=" => cmp_tcl_vals(&left, &right) != Some(std::cmp::Ordering::Equal),
                    _ => unreachable!(),
                };
                left = ExprVal::Int(i64::from(res));
            }
        }
        Ok(left)
    }

    fn parse_rel(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_shift(eval)?;
        loop {
            self.skip_ws();
            let op = if self.starts_with_bytes(b"<=") {
                self.pos += 2;
                Some("<=")
            } else if self.starts_with_bytes(b">=") {
                self.pos += 2;
                Some(">=")
            } else if self.starts_with_bytes(b"<") && !self.starts_with_bytes(b"<<") {
                self.pos += 1;
                Some("<")
            } else if self.starts_with_bytes(b">") && !self.starts_with_bytes(b">>") {
                self.pos += 1;
                Some(">")
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.parse_shift(eval)?;
            if eval {
                let ord = cmp_tcl_vals(&left, &right);
                let res = match (op, ord) {
                    ("<", Some(std::cmp::Ordering::Less)) => true,
                    ("<=", Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)) => true,
                    (">", Some(std::cmp::Ordering::Greater)) => true,
                    (">=", Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)) => true,
                    _ => false,
                };
                left = ExprVal::Int(i64::from(res));
            }
        }
        Ok(left)
    }

    fn parse_shift(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_add(eval)?;
        loop {
            self.skip_ws();
            let op = if self.starts_with_bytes(b"<<") {
                self.pos += 2;
                Some("<<")
            } else if self.starts_with_bytes(b">>") {
                self.pos += 2;
                Some(">>")
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.parse_add(eval)?;
            if eval {
                let a = left.as_int().map_err(|e| self.err(e))?;
                let b = right.as_int().map_err(|e| self.err(e))?;
                let sh = (b & 63) as u32;
                left = ExprVal::Int(if op == "<<" {
                    a.wrapping_shl(sh)
                } else {
                    a.wrapping_shr(sh)
                });
            }
        }
        Ok(left)
    }

    fn parse_add(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_mul(eval)?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len()
                && (self.bytes[self.pos] == b'+' || self.bytes[self.pos] == b'-')
            {
                let op = self.bytes[self.pos];
                self.pos += 1;
                let right = self.parse_mul(eval)?;
                if eval {
                    left = arith_binop(&left, &right, op).map_err(|e| self.err(e))?;
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_mul(&mut self, eval: bool) -> TclResult<ExprVal> {
        let mut left = self.parse_pow(eval)?;
        loop {
            self.skip_ws();
            if self.pos < self.bytes.len() {
                let b = self.bytes[self.pos];
                if b == b'*' && !self.starts_with_bytes(b"**") {
                    self.pos += 1;
                    let right = self.parse_pow(eval)?;
                    if eval {
                        left = arith_binop(&left, &right, b'*').map_err(|e| self.err(e))?;
                    }
                } else if b == b'/' || b == b'%' {
                    self.pos += 1;
                    let right = self.parse_pow(eval)?;
                    if eval {
                        left = arith_binop(&left, &right, b).map_err(|e| self.err(e))?;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_pow(&mut self, eval: bool) -> TclResult<ExprVal> {
        let base = self.parse_unary(eval)?;
        self.skip_ws();
        if self.starts_with_bytes(b"**") {
            self.pos += 2;
            let exp = self.parse_pow(eval)?;
            if eval {
                let b_num = base
                    .coerce_numeric()
                    .ok_or_else(|| self.err("non-numeric operand to **"))?;
                let e_num = exp
                    .coerce_numeric()
                    .ok_or_else(|| self.err("non-numeric operand to **"))?;
                match (b_num, e_num) {
                    (ExprVal::Int(bi), ExprVal::Int(ei)) if ei >= 0 && ei <= 62 => {
                        Ok(ExprVal::Int(bi.wrapping_pow(ei as u32)))
                    }
                    (bv, ev) => Ok(ExprVal::Float(
                        bv.as_float().unwrap().powf(ev.as_float().unwrap()),
                    )),
                }
            } else {
                Ok(ExprVal::Int(0))
            }
        } else {
            Ok(base)
        }
    }

    fn parse_unary(&mut self, eval: bool) -> TclResult<ExprVal> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return Err(self.err("unexpected end of expression"));
        }
        match self.bytes[self.pos] {
            b'-' => {
                self.pos += 1;
                let v = self.parse_unary(eval)?;
                if !eval {
                    return Ok(ExprVal::Int(0));
                }
                match v
                    .coerce_numeric()
                    .ok_or_else(|| self.err(format!("can't use non-numeric string as operand of \"-\"")))?
                {
                    ExprVal::Int(i) => Ok(ExprVal::Int(i.wrapping_neg())),
                    ExprVal::Float(f) => Ok(ExprVal::Float(-f)),
                    ExprVal::Str(_) => unreachable!(),
                }
            }
            b'+' => {
                self.pos += 1;
                let v = self.parse_unary(eval)?;
                if !eval {
                    return Ok(ExprVal::Int(0));
                }
                v.coerce_numeric()
                    .ok_or_else(|| self.err("can't use non-numeric string as operand of \"+\""))
            }
            b'!' => {
                self.pos += 1;
                let v = self.parse_unary(eval)?;
                if !eval {
                    return Ok(ExprVal::Int(0));
                }
                let b = v.as_bool().map_err(|e| self.err(e))?;
                Ok(ExprVal::Int(i64::from(!b)))
            }
            b'~' => {
                self.pos += 1;
                let v = self.parse_unary(eval)?;
                if !eval {
                    return Ok(ExprVal::Int(0));
                }
                let i = v.as_int().map_err(|e| self.err(e))?;
                Ok(ExprVal::Int(!i))
            }
            _ => self.parse_primary(eval),
        }
    }

    fn parse_primary(&mut self, eval: bool) -> TclResult<ExprVal> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return Err(self.err("unexpected end of expression"));
        }
        match self.bytes[self.pos] {
            b'(' => {
                self.pos += 1;
                let v = self.parse_ternary(eval)?;
                self.skip_ws();
                if self.pos >= self.bytes.len() || self.bytes[self.pos] != b')' {
                    return Err(self.err("unmatched '(' in expression"));
                }
                self.pos += 1;
                Ok(v)
            }
            b'{' => {
                self.pos += 1;
                let start = self.pos;
                let mut depth = 1usize;
                while self.pos < self.bytes.len() && depth > 0 {
                    if self.bytes[self.pos] == b'\\' && self.pos + 1 < self.bytes.len() {
                        self.pos += 2;
                        continue;
                    }
                    if self.bytes[self.pos] == b'{' {
                        depth += 1;
                    } else if self.bytes[self.pos] == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    self.pos += 1;
                }
                if depth != 0 {
                    return Err(self.err("unclosed '{' in expression"));
                }
                let s = self.src[start..self.pos].to_string();
                self.pos += 1;
                Ok(ExprVal::Str(s))
            }
            b'"' => {
                self.pos += 1;
                let mut out = String::new();
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'"' {
                    match self.bytes[self.pos] {
                        b'\\' => {
                            let (esc, nxt) = parse_backslash_escape(self.src, self.pos);
                            out.push_str(&esc);
                            self.pos = nxt;
                        }
                        b'$' => {
                            let (val, nxt) = self.interp.subst_dollar(self.src, self.pos, eval)?;
                            out.push_str(&val);
                            self.pos = nxt;
                        }
                        b'[' => {
                            let (val, nxt) = self.interp.subst_bracket(self.src, self.pos, eval)?;
                            out.push_str(&val);
                            self.pos = nxt;
                        }
                        _ => {
                            let ch = self.src[self.pos..].chars().next().unwrap();
                            out.push(ch);
                            self.pos += ch.len_utf8();
                        }
                    }
                }
                if self.pos >= self.bytes.len() {
                    return Err(self.err("unclosed '\"' in expression"));
                }
                self.pos += 1;
                Ok(ExprVal::Str(out))
            }
            b'$' => {
                let (val, nxt) = self.interp.subst_dollar(self.src, self.pos, eval)?;
                self.pos = nxt;
                Ok(str_to_expr_val(val))
            }
            b'[' => {
                let (val, nxt) = self.interp.subst_bracket(self.src, self.pos, eval)?;
                self.pos = nxt;
                Ok(str_to_expr_val(val))
            }
            b'0'..=b'9' | b'.' => {
                let start = self.pos;
                if self.bytes[self.pos] == b'0'
                    && self.pos + 1 < self.bytes.len()
                    && matches!(self.bytes[self.pos + 1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B')
                {
                    self.pos += 2;
                    while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_hexdigit() {
                        self.pos += 1;
                    }
                    let tok = &self.src[start..self.pos];
                    let iv = parse_tcl_int(tok)
                        .ok_or_else(|| self.err(format!("invalid integer \"{tok}\"")))?;
                    return Ok(ExprVal::Int(iv));
                }

                let mut is_float = false;
                while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
                if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' {
                    is_float = true;
                    self.pos += 1;
                    while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                }
                if self.pos < self.bytes.len()
                    && (self.bytes[self.pos] == b'e' || self.bytes[self.pos] == b'E')
                {
                    is_float = true;
                    self.pos += 1;
                    if self.pos < self.bytes.len()
                        && (self.bytes[self.pos] == b'+' || self.bytes[self.pos] == b'-')
                    {
                        self.pos += 1;
                    }
                    while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
                        self.pos += 1;
                    }
                }
                let tok = &self.src[start..self.pos];
                if is_float {
                    let fv = parse_tcl_float(tok)
                        .ok_or_else(|| self.err(format!("invalid float \"{tok}\"")))?;
                    Ok(ExprVal::Float(fv))
                } else if let Some(iv) = parse_tcl_int(tok) {
                    Ok(ExprVal::Int(iv))
                } else if let Some(fv) = parse_tcl_float(tok) {
                    Ok(ExprVal::Float(fv))
                } else {
                    Err(self.err(format!("invalid number \"{tok}\"")))
                }
            }
            _ if is_ident_start(self.bytes[self.pos]) => {
                let start = self.pos;
                while self.pos < self.bytes.len() && is_ident_cont(self.bytes[self.pos]) {
                    self.pos += 1;
                }
                let ident = &self.src[start..self.pos];
                self.skip_ws();
                if self.pos < self.bytes.len() && self.bytes[self.pos] == b'(' {
                    self.pos += 1;
                    let mut args = Vec::new();
                    self.skip_ws();
                    if self.pos < self.bytes.len() && self.bytes[self.pos] != b')' {
                        loop {
                            args.push(self.parse_ternary(eval)?);
                            self.skip_ws();
                            if self.pos < self.bytes.len() && self.bytes[self.pos] == b',' {
                                self.pos += 1;
                            } else {
                                break;
                            }
                        }
                    }
                    if self.pos >= self.bytes.len() || self.bytes[self.pos] != b')' {
                        return Err(self.err(format!("missing ')' after {ident}(")));
                    }
                    self.pos += 1;
                    if !eval {
                        return Ok(ExprVal::Int(0));
                    }
                    eval_math_func(ident, &args).map_err(|e| self.err(e))
                } else if let Ok(b) = parse_tcl_bool_word(ident) {
                    Ok(ExprVal::Int(i64::from(b)))
                } else if let Some(fv) = parse_tcl_float(ident) {
                    Ok(ExprVal::Float(fv))
                } else {
                    Err(self.err(format!("invalid bareword \"{ident}\" in expression")))
                }
            }
            other => Err(self.err(format!(
                "unexpected character '{}' in expression \"{}\"",
                other as char, self.src
            ))),
        }
    }

    fn starts_with_bytes(&self, pat: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(pat)
    }

    fn starts_with_kw(&self, kw: &str) -> bool {
        let kb = kw.as_bytes();
        if !self.bytes[self.pos..].starts_with(kb) {
            return false;
        }
        let after = self.pos + kb.len();
        after >= self.bytes.len() || !is_ident_cont(self.bytes[after])
    }
}

fn str_to_expr_val(s: String) -> ExprVal {
    let t = s.trim();
    if let Some(i) = parse_tcl_int(t) {
        if t == i.to_string() {
            return ExprVal::Int(i);
        }
    }
    ExprVal::Str(s)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_cont(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn cmp_tcl_vals(a: &ExprVal, b: &ExprVal) -> Option<std::cmp::Ordering> {
    if let (Some(na), Some(nb)) = (a.coerce_numeric(), b.coerce_numeric()) {
        match (na, nb) {
            (ExprVal::Int(ia), ExprVal::Int(ib)) => Some(ia.cmp(&ib)),
            (va, vb) => va.as_float().ok()?.partial_cmp(&vb.as_float().ok()?),
        }
    } else {
        Some(a.to_tcl_string().cmp(&b.to_tcl_string()))
    }
}

fn arith_binop(a: &ExprVal, b: &ExprVal, op: u8) -> Result<ExprVal, String> {
    let na = a
        .coerce_numeric()
        .ok_or_else(|| format!("can't use non-numeric string \"{}\" as operand", a.to_tcl_string()))?;
    let nb = b
        .coerce_numeric()
        .ok_or_else(|| format!("can't use non-numeric string \"{}\" as operand", b.to_tcl_string()))?;

    match (na, nb) {
        (ExprVal::Int(ia), ExprVal::Int(ib)) => match op {
            b'+' => Ok(ExprVal::Int(ia.wrapping_add(ib))),
            b'-' => Ok(ExprVal::Int(ia.wrapping_sub(ib))),
            b'*' => Ok(ExprVal::Int(ia.wrapping_mul(ib))),
            b'/' => {
                if ib == 0 {
                    return Err("divide by zero".to_string());
                }
                // TCL integer division floors toward -infinity!
                let d = ia.wrapping_div(ib);
                let r = ia.wrapping_rem(ib);
                if r != 0 && ((r < 0) != (ib < 0)) {
                    Ok(ExprVal::Int(d - 1))
                } else {
                    Ok(ExprVal::Int(d))
                }
            }
            b'%' => {
                if ib == 0 {
                    return Err("divide by zero".to_string());
                }
                let r = ia.wrapping_rem(ib);
                if r != 0 && ((r < 0) != (ib < 0)) {
                    Ok(ExprVal::Int(r + ib))
                } else {
                    Ok(ExprVal::Int(r))
                }
            }
            _ => unreachable!(),
        },
        (va, vb) => {
            let fa = va.as_float()?;
            let fb = vb.as_float()?;
            match op {
                b'+' => Ok(ExprVal::Float(fa + fb)),
                b'-' => Ok(ExprVal::Float(fa - fb)),
                b'*' => Ok(ExprVal::Float(fa * fb)),
                b'/' => {
                    if fb == 0.0 {
                        return Err("divide by zero".to_string());
                    }
                    Ok(ExprVal::Float(fa / fb))
                }
                b'%' => Err("can't use floating-point value as operand of \"%\"".to_string()),
                _ => unreachable!(),
            }
        }
    }
}

fn eval_math_func(name: &str, args: &[ExprVal]) -> Result<ExprVal, String> {
    match name {
        "abs" => {
            if args.len() != 1 {
                return Err("wrong # args to mathfunc abs".to_string());
            }
            match args[0].coerce_numeric().ok_or("non-numeric arg to abs")? {
                ExprVal::Int(i) => Ok(ExprVal::Int(i.wrapping_abs())),
                ExprVal::Float(f) => Ok(ExprVal::Float(f.abs())),
                ExprVal::Str(_) => unreachable!(),
            }
        }
        "int" | "wide" | "entier" => {
            if args.len() != 1 {
                return Err(format!("wrong # args to mathfunc {name}"));
            }
            Ok(ExprVal::Int(args[0].as_int()?))
        }
        "double" => {
            if args.len() != 1 {
                return Err("wrong # args to mathfunc double".to_string());
            }
            Ok(ExprVal::Float(args[0].as_float()?))
        }
        "round" => {
            if args.len() != 1 {
                return Err("wrong # args to mathfunc round".to_string());
            }
            Ok(ExprVal::Int(args[0].as_float()?.round() as i64))
        }
        "floor" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.floor())),
        "ceil" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.ceil())),
        "sqrt" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.sqrt())),
        "log" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.ln())),
        "log10" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.log10())),
        "exp" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.exp())),
        "sin" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.sin())),
        "cos" => Ok(ExprVal::Float(args.first().ok_or("arg")?.as_float()?.cos())),
        "pow" => {
            if args.len() != 2 {
                return Err("wrong # args to mathfunc pow".to_string());
            }
            Ok(ExprVal::Float(args[0].as_float()?.powf(args[1].as_float()?)))
        }
        "fmod" => {
            if args.len() != 2 {
                return Err("wrong # args to mathfunc fmod".to_string());
            }
            Ok(ExprVal::Float(args[0].as_float()? % args[1].as_float()?))
        }
        "min" => {
            if args.is_empty() {
                return Err("not enough arguments for math function \"min\"".to_string());
            }
            let mut best = args[0].clone();
            for a in &args[1..] {
                if cmp_tcl_vals(a, &best) == Some(std::cmp::Ordering::Less) {
                    best = a.clone();
                }
            }
            Ok(best)
        }
        "max" => {
            if args.is_empty() {
                return Err("not enough arguments for math function \"max\"".to_string());
            }
            let mut best = args[0].clone();
            for a in &args[1..] {
                if cmp_tcl_vals(a, &best) == Some(std::cmp::Ordering::Greater) {
                    best = a.clone();
                }
            }
            Ok(best)
        }
        "isnan" => Ok(ExprVal::Int(i64::from(args.first().ok_or("arg")?.as_float()?.is_nan()))),
        "isinf" => Ok(ExprVal::Int(i64::from(
            args.first().ok_or("arg")?.as_float()?.is_infinite(),
        ))),
        "bool" => Ok(ExprVal::Int(i64::from(args.first().ok_or("arg")?.as_bool()?))),
        "srand" => {
            static SRAND: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0x9e37_79b9_7f4a_7c15);
            let seed = args.first().map(|v| v.as_int()).transpose()?.unwrap_or(1) as u64;
            SRAND.store(seed | 1, std::sync::atomic::Ordering::Relaxed);
            Ok(ExprVal::Float(0.5))
        }
        "rand" => {
            static STATE: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0x9e37_79b9_7f4a_7c15);
            let mut x = STATE.load(std::sync::atomic::Ordering::Relaxed);
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            STATE.store(x, std::sync::atomic::Ordering::Relaxed);
            let f = ((x >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
            Ok(ExprVal::Float(f))
        }
        other => Err(format!("unknown math function \"{other}\"")),
    }
}
