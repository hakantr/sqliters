//! Pure safe Rust zero-dependency regular expression and glob matcher
//! supporting TCL `regexp`, `regsub`, and `string match` semantics.

/// Matches TCL `string match` glob patterns (`*`, `?`, `[abc]`, `\x`).
pub fn glob_match(pattern: &str, text: &str, nocase: bool) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    glob_match_slice(&pat, &txt, nocase)
}

fn glob_match_slice(pat: &[char], txt: &[char], nocase: bool) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < txt.len() {
        if pi < pat.len() && pat[pi] == '*' {
            while pi < pat.len() && pat[pi] == '*' {
                pi += 1;
            }
            if pi == pat.len() {
                return true;
            }
            star_pi = Some(pi);
            star_ti = ti;
        } else if pi < pat.len() && pat[pi] == '?' {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '[' {
            if let Some((matched, next_pi)) = match_glob_bracket(&pat[pi..], txt[ti], nocase) {
                if matched {
                    pi += next_pi;
                    ti += 1;
                    continue;
                }
            }
            if let Some(sp) = star_pi {
                pi = sp;
                star_ti += 1;
                ti = star_ti;
            } else {
                return false;
            }
        } else if pi < pat.len() {
            let pc = if pat[pi] == '\\' && pi + 1 < pat.len() {
                pi += 1;
                pat[pi]
            } else {
                pat[pi]
            };
            if chars_eq(pc, txt[ti], nocase) {
                pi += 1;
                ti += 1;
            } else if let Some(sp) = star_pi {
                pi = sp;
                star_ti += 1;
                ti = star_ti;
            } else {
                return false;
            }
        } else if let Some(sp) = star_pi {
            pi = sp;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

fn match_glob_bracket(pat: &[char], ch: char, nocase: bool) -> Option<(bool, usize)> {
    // pat[0] == '['
    let mut i = 1;
    let mut negate = false;
    if i < pat.len() && (pat[i] == '!' || pat[i] == '^') {
        negate = true;
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pat.len() {
        if pat[i] == ']' && !first {
            let res = if negate { !matched } else { matched };
            return Some((res, i + 1));
        }
        first = false;
        let c1 = if pat[i] == '\\' && i + 1 < pat.len() {
            i += 1;
            pat[i]
        } else {
            pat[i]
        };
        if i + 2 < pat.len() && pat[i + 1] == '-' && pat[i + 2] != ']' {
            let c2 = if pat[i + 2] == '\\' && i + 3 < pat.len() {
                i += 3;
                pat[i]
            } else {
                i += 2;
                pat[i]
            };
            if char_in_range(ch, c1, c2, nocase) {
                matched = true;
            }
        } else if chars_eq(c1, ch, nocase) {
            matched = true;
        }
        i += 1;
    }
    None
}

#[inline]
fn chars_eq(a: char, b: char, nocase: bool) -> bool {
    if nocase {
        a.to_ascii_lowercase() == b.to_ascii_lowercase()
    } else {
        a == b
    }
}

#[inline]
fn char_in_range(ch: char, lo: char, hi: char, nocase: bool) -> bool {
    if nocase {
        let c = ch.to_ascii_lowercase();
        let l = lo.to_ascii_lowercase();
        let h = hi.to_ascii_lowercase();
        c >= l && c <= h
    } else {
        ch >= lo && ch <= hi
    }
}

#[derive(Debug, Clone)]
enum Node {
    Seq(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        inner: Box<Node>,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    },
    Group {
        index: Option<usize>,
        inner: Box<Node>,
    },
    Literal(char),
    Any,
    AnchorStart,
    AnchorEnd,
    WordBoundary,
    Class {
        negate: bool,
        items: Vec<ClassItem>,
    },
}

#[derive(Debug, Clone)]
enum ClassItem {
    Single(char),
    Range(char, char),
    Digit,
    Word,
    Space,
    Alpha,
    Alnum,
    Xdigit,
}

#[derive(Debug, Clone)]
pub struct Regex {
    root: Node,
    num_groups: usize,
    nocase: bool,
}

#[derive(Debug, Clone)]
pub struct Captures {
    /// `(start_char_idx, end_char_idx)` for group 0..=num_groups
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Regex {
    pub fn compile(pattern: &str, mut nocase: bool) -> Result<Self, String> {
        let mut pat = pattern;
        if let Some(rest) = pat.strip_prefix("(?i)") {
            nocase = true;
            pat = rest;
        }
        let chars: Vec<char> = pat.chars().collect();
        let mut parser = Parser {
            chars: &chars,
            pos: 0,
            next_group: 1,
        };
        let root = parser.parse_alt()?;
        if parser.pos < parser.chars.len() {
            return Err(format!("unexpected ')' in regex: {pattern}"));
        }
        Ok(Self {
            root,
            num_groups: parser.next_group - 1,
            nocase,
        })
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.find_captures(text).is_some()
    }

    pub fn find_captures(&self, text: &str) -> Option<Captures> {
        let chars: Vec<char> = text.chars().collect();
        for start in 0..=chars.len() {
            let mut groups = vec![None; self.num_groups + 1];
            if let Some(end) = self.match_node(&self.root, &chars, start, &mut groups) {
                groups[0] = Some((start, end));
                return Some(Captures { groups });
            }
        }
        None
    }

    pub fn replace(&self, text: &str, replacement: &str, all: bool) -> (String, usize) {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::new();
        let mut pos = 0usize;
        let mut count = 0usize;

        while pos <= chars.len() {
            let mut matched = None;
            for start in pos..=chars.len() {
                let mut groups = vec![None; self.num_groups + 1];
                if let Some(end) = self.match_node(&self.root, &chars, start, &mut groups) {
                    groups[0] = Some((start, end));
                    matched = Some((start, end, groups));
                    break;
                }
            }

            let Some((start, end, groups)) = matched else {
                for &c in &chars[pos..] {
                    out.push(c);
                }
                break;
            };

            for &c in &chars[pos..start] {
                out.push(c);
            }
            expand_replacement(&chars, &groups, replacement, &mut out);
            count += 1;

            if !all {
                for &c in &chars[end..] {
                    out.push(c);
                }
                break;
            }

            if end == start {
                if end < chars.len() {
                    out.push(chars[end]);
                }
                pos = end + 1;
            } else {
                pos = end;
            }
        }

        (out, count)
    }

    fn match_node(
        &self,
        node: &Node,
        chars: &[char],
        pos: usize,
        groups: &mut Vec<Option<(usize, usize)>>,
    ) -> Option<usize> {
        self.match_seq(std::slice::from_ref(node), chars, pos, groups)
    }

    fn match_seq(
        &self,
        seq: &[Node],
        chars: &[char],
        pos: usize,
        groups: &mut Vec<Option<(usize, usize)>>,
    ) -> Option<usize> {
        if seq.is_empty() {
            return Some(pos);
        }
        let head = &seq[0];
        let tail = &seq[1..];

        match head {
            Node::Seq(inner) => {
                let mut combined = Vec::with_capacity(inner.len() + tail.len());
                combined.extend_from_slice(inner);
                combined.extend_from_slice(tail);
                self.match_seq(&combined, chars, pos, groups)
            }
            Node::Alt(branches) => {
                let saved = groups.clone();
                for branch in branches {
                    let mut combined = Vec::with_capacity(1 + tail.len());
                    combined.push(branch.clone());
                    combined.extend_from_slice(tail);
                    if let Some(end) = self.match_seq(&combined, chars, pos, groups) {
                        return Some(end);
                    }
                    *groups = saved.clone();
                }
                None
            }
            Node::Group { index, inner } => {
                // Match inner, record group span, then match tail (with backtracking if inner has choices)
                let saved = groups.clone();
                let mut candidates = Vec::new();
                self.collect_ends(inner, chars, pos, groups, &mut candidates);
                for (end_inner, grp_state) in candidates {
                    *groups = grp_state;
                    if let Some(idx) = *index {
                        if idx < groups.len() {
                            groups[idx] = Some((pos, end_inner));
                        }
                    }
                    if let Some(final_end) = self.match_seq(tail, chars, end_inner, groups) {
                        return Some(final_end);
                    }
                }
                *groups = saved;
                None
            }
            Node::Repeat {
                inner,
                min,
                max,
                greedy,
            } => self.match_repeat(inner, *min, *max, *greedy, tail, chars, pos, groups, 0),
            Node::Literal(ch) => {
                if pos < chars.len() && chars_eq(*ch, chars[pos], self.nocase) {
                    self.match_seq(tail, chars, pos + 1, groups)
                } else {
                    None
                }
            }
            Node::Any => {
                if pos < chars.len() {
                    self.match_seq(tail, chars, pos + 1, groups)
                } else {
                    None
                }
            }
            Node::AnchorStart => {
                if pos == 0 {
                    self.match_seq(tail, chars, pos, groups)
                } else {
                    None
                }
            }
            Node::AnchorEnd => {
                if pos == chars.len() {
                    self.match_seq(tail, chars, pos, groups)
                } else {
                    None
                }
            }
            Node::WordBoundary => {
                let left_word = pos > 0 && is_word_char(chars[pos - 1]);
                let right_word = pos < chars.len() && is_word_char(chars[pos]);
                if left_word != right_word {
                    self.match_seq(tail, chars, pos, groups)
                } else {
                    None
                }
            }
            Node::Class { negate, items } => {
                if pos >= chars.len() {
                    return None;
                }
                let ch = chars[pos];
                let hit = items.iter().any(|item| match_class_item(item, ch, self.nocase));
                let ok = if *negate { !hit } else { hit };
                if ok {
                    self.match_seq(tail, chars, pos + 1, groups)
                } else {
                    None
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn match_repeat(
        &self,
        inner: &Node,
        min: usize,
        max: Option<usize>,
        greedy: bool,
        tail: &[Node],
        chars: &[char],
        pos: usize,
        groups: &mut Vec<Option<(usize, usize)>>,
        count: usize,
    ) -> Option<usize> {
        if let Some(m) = max {
            if count == m {
                return self.match_seq(tail, chars, pos, groups);
            }
        }

        if count < min {
            let mut candidates = Vec::new();
            self.collect_ends(inner, chars, pos, groups, &mut candidates);
            let saved = groups.clone();
            for (next_pos, grp_state) in candidates {
                if next_pos == pos && count + 1 >= min {
                    *groups = grp_state;
                    if let Some(end) = self.match_seq(tail, chars, next_pos, groups) {
                        return Some(end);
                    }
                    continue;
                }
                *groups = grp_state;
                if let Some(end) = self.match_repeat(
                    inner,
                    min,
                    max,
                    greedy,
                    tail,
                    chars,
                    next_pos,
                    groups,
                    count + 1,
                ) {
                    return Some(end);
                }
            }
            *groups = saved;
            return None;
        }

        let saved = groups.clone();
        if greedy {
            let mut candidates = Vec::new();
            self.collect_ends(inner, chars, pos, groups, &mut candidates);
            for (next_pos, grp_state) in candidates {
                if next_pos == pos {
                    continue;
                }
                *groups = grp_state;
                if let Some(end) = self.match_repeat(
                    inner,
                    min,
                    max,
                    greedy,
                    tail,
                    chars,
                    next_pos,
                    groups,
                    count + 1,
                ) {
                    return Some(end);
                }
            }
            *groups = saved;
            self.match_seq(tail, chars, pos, groups)
        } else {
            if let Some(end) = self.match_seq(tail, chars, pos, groups) {
                return Some(end);
            }
            *groups = saved.clone();
            let mut candidates = Vec::new();
            self.collect_ends(inner, chars, pos, groups, &mut candidates);
            for (next_pos, grp_state) in candidates {
                if next_pos == pos {
                    continue;
                }
                *groups = grp_state;
                if let Some(end) = self.match_repeat(
                    inner,
                    min,
                    max,
                    greedy,
                    tail,
                    chars,
                    next_pos,
                    groups,
                    count + 1,
                ) {
                    return Some(end);
                }
            }
            *groups = saved;
            None
        }
    }

    fn collect_ends(
        &self,
        node: &Node,
        chars: &[char],
        pos: usize,
        groups: &mut Vec<Option<(usize, usize)>>,
        out: &mut Vec<(usize, Vec<Option<(usize, usize)>>)>,
    ) {
        match node {
            Node::Alt(branches) => {
                let saved = groups.clone();
                for b in branches {
                    *groups = saved.clone();
                    self.collect_ends(b, chars, pos, groups, out);
                }
                *groups = saved;
            }
            _ => {
                let saved = groups.clone();
                if let Some(end) = self.match_node(node, chars, pos, groups) {
                    out.push((end, groups.clone()));
                }
                *groups = saved;
            }
        }
    }
}

fn expand_replacement(
    chars: &[char],
    groups: &[Option<(usize, usize)>],
    rep: &str,
    out: &mut String,
) {
    let rchars: Vec<char> = rep.chars().collect();
    let mut i = 0;
    while i < rchars.len() {
        if rchars[i] == '&' {
            if let Some((s, e)) = groups.first().copied().flatten() {
                for &c in &chars[s..e] {
                    out.push(c);
                }
            }
            i += 1;
        } else if rchars[i] == '\\' && i + 1 < rchars.len() {
            let nxt = rchars[i + 1];
            if nxt.is_ascii_digit() {
                let idx = (nxt as usize) - ('0' as usize);
                if let Some((s, e)) = groups.get(idx).copied().flatten() {
                    for &c in &chars[s..e] {
                        out.push(c);
                    }
                }
                i += 2;
            } else {
                out.push(nxt);
                i += 2;
            }
        } else {
            out.push(rchars[i]);
            i += 1;
        }
    }
}

#[inline]
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn match_class_item(item: &ClassItem, ch: char, nocase: bool) -> bool {
    match item {
        ClassItem::Single(c) => chars_eq(*c, ch, nocase),
        ClassItem::Range(lo, hi) => char_in_range(ch, *lo, *hi, nocase),
        ClassItem::Digit => ch.is_ascii_digit(),
        ClassItem::Word => is_word_char(ch),
        ClassItem::Space => ch.is_whitespace(),
        ClassItem::Alpha => ch.is_alphabetic(),
        ClassItem::Alnum => ch.is_alphanumeric(),
        ClassItem::Xdigit => ch.is_ascii_hexdigit(),
    }
}

struct Parser<'a> {
    chars: &'a [char],
    pos: usize,
    next_group: usize,
}

impl<'a> Parser<'a> {
    fn parse_alt(&mut self) -> Result<Node, String> {
        let mut branches = vec![self.parse_seq()?];
        while self.pos < self.chars.len() && self.chars[self.pos] == '|' {
            self.pos += 1;
            branches.push(self.parse_seq()?);
        }
        if branches.len() == 1 {
            Ok(branches.pop().unwrap())
        } else {
            Ok(Node::Alt(branches))
        }
    }

    fn parse_seq(&mut self) -> Result<Node, String> {
        let mut items = Vec::new();
        while self.pos < self.chars.len() {
            let ch = self.chars[self.pos];
            if ch == ')' || ch == '|' {
                break;
            }
            let atom = self.parse_atom()?;
            let node = self.parse_quantifier(atom)?;
            items.push(node);
        }
        if items.len() == 1 {
            Ok(items.pop().unwrap())
        } else {
            Ok(Node::Seq(items))
        }
    }

    fn parse_atom(&mut self) -> Result<Node, String> {
        let ch = self.chars[self.pos];
        self.pos += 1;
        match ch {
            '^' => Ok(Node::AnchorStart),
            '$' => Ok(Node::AnchorEnd),
            '.' => Ok(Node::Any),
            '(' => {
                let index = if self.pos + 1 < self.chars.len()
                    && self.chars[self.pos] == '?'
                    && self.chars[self.pos + 1] == ':'
                {
                    self.pos += 2;
                    None
                } else {
                    let idx = self.next_group;
                    self.next_group += 1;
                    Some(idx)
                };
                let inner = self.parse_alt()?;
                if self.pos >= self.chars.len() || self.chars[self.pos] != ')' {
                    return Err("unclosed '(' in regex".to_string());
                }
                self.pos += 1;
                Ok(Node::Group {
                    index,
                    inner: Box::new(inner),
                })
            }
            '[' => self.parse_class(),
            '\\' => {
                if self.pos >= self.chars.len() {
                    return Ok(Node::Literal('\\'));
                }
                let esc = self.chars[self.pos];
                self.pos += 1;
                match esc {
                    'd' => Ok(Node::Class {
                        negate: false,
                        items: vec![ClassItem::Digit],
                    }),
                    'D' => Ok(Node::Class {
                        negate: true,
                        items: vec![ClassItem::Digit],
                    }),
                    'w' => Ok(Node::Class {
                        negate: false,
                        items: vec![ClassItem::Word],
                    }),
                    'W' => Ok(Node::Class {
                        negate: true,
                        items: vec![ClassItem::Word],
                    }),
                    's' => Ok(Node::Class {
                        negate: false,
                        items: vec![ClassItem::Space],
                    }),
                    'S' => Ok(Node::Class {
                        negate: true,
                        items: vec![ClassItem::Space],
                    }),
                    'y' | 'b' => Ok(Node::WordBoundary),
                    'A' => Ok(Node::AnchorStart),
                    'Z' | 'z' => Ok(Node::AnchorEnd),
                    'n' => Ok(Node::Literal('\n')),
                    'r' => Ok(Node::Literal('\r')),
                    't' => Ok(Node::Literal('\t')),
                    other => Ok(Node::Literal(other)),
                }
            }
            other => Ok(Node::Literal(other)),
        }
    }

    fn parse_class(&mut self) -> Result<Node, String> {
        let mut negate = false;
        if self.pos < self.chars.len() && self.chars[self.pos] == '^' {
            negate = true;
            self.pos += 1;
        }
        let mut items = Vec::new();
        let mut first = true;
        while self.pos < self.chars.len() {
            if self.chars[self.pos] == ']' && !first {
                self.pos += 1;
                return Ok(Node::Class { negate, items });
            }
            first = false;
            // POSIX [:alpha:], [:digit:], [:space:], [:alnum:], [:xdigit:]
            if self.pos + 2 < self.chars.len()
                && self.chars[self.pos] == '['
                && self.chars[self.pos + 1] == ':'
            {
                if let Some(end_off) = self.find_posix_class_end() {
                    let name: String = self.chars[self.pos + 2..end_off].iter().collect();
                    self.pos = end_off + 2;
                    match name.as_str() {
                        "digit" => items.push(ClassItem::Digit),
                        "space" => items.push(ClassItem::Space),
                        "alpha" => items.push(ClassItem::Alpha),
                        "alnum" => items.push(ClassItem::Alnum),
                        "word" => items.push(ClassItem::Word),
                        "xdigit" => items.push(ClassItem::Xdigit),
                        _ => {}
                    }
                    continue;
                }
            }
            let c1 = if self.chars[self.pos] == '\\' && self.pos + 1 < self.chars.len() {
                self.pos += 1;
                let esc = self.chars[self.pos];
                self.pos += 1;
                match esc {
                    'd' => {
                        items.push(ClassItem::Digit);
                        continue;
                    }
                    'w' => {
                        items.push(ClassItem::Word);
                        continue;
                    }
                    's' => {
                        items.push(ClassItem::Space);
                        continue;
                    }
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                }
            } else {
                let c = self.chars[self.pos];
                self.pos += 1;
                c
            };

            if self.pos + 1 < self.chars.len()
                && self.chars[self.pos] == '-'
                && self.chars[self.pos + 1] != ']'
            {
                self.pos += 1;
                let c2 = if self.chars[self.pos] == '\\' && self.pos + 1 < self.chars.len() {
                    self.pos += 1;
                    let esc = self.chars[self.pos];
                    self.pos += 1;
                    match esc {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    }
                } else {
                    let c = self.chars[self.pos];
                    self.pos += 1;
                    c
                };
                items.push(ClassItem::Range(c1, c2));
            } else {
                items.push(ClassItem::Single(c1));
            }
        }
        Err("unclosed '[' in regex".to_string())
    }

    fn find_posix_class_end(&self) -> Option<usize> {
        let mut i = self.pos + 2;
        while i + 1 < self.chars.len() {
            if self.chars[i] == ':' && self.chars[i + 1] == ']' {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn parse_quantifier(&mut self, atom: Node) -> Result<Node, String> {
        if self.pos >= self.chars.len() {
            return Ok(atom);
        }
        let (min, max) = match self.chars[self.pos] {
            '*' => {
                self.pos += 1;
                (0, None)
            }
            '+' => {
                self.pos += 1;
                (1, None)
            }
            '?' => {
                self.pos += 1;
                (0, Some(1))
            }
            '{' => {
                if let Some((mn, mx, next_pos)) = self.try_parse_brace_quant() {
                    self.pos = next_pos;
                    (mn, mx)
                } else {
                    return Ok(atom);
                }
            }
            _ => return Ok(atom),
        };
        let greedy = if self.pos < self.chars.len() && self.chars[self.pos] == '?' {
            self.pos += 1;
            false
        } else {
            true
        };
        Ok(Node::Repeat {
            inner: Box::new(atom),
            min,
            max,
            greedy,
        })
    }

    fn try_parse_brace_quant(&self) -> Option<(usize, Option<usize>, usize)> {
        let mut i = self.pos + 1;
        let start_m = i;
        while i < self.chars.len() && self.chars[i].is_ascii_digit() {
            i += 1;
        }
        if i == start_m {
            return None;
        }
        let min: usize = self.chars[start_m..i].iter().collect::<String>().parse().ok()?;
        if i < self.chars.len() && self.chars[i] == '}' {
            return Some((min, Some(min), i + 1));
        }
        if i < self.chars.len() && self.chars[i] == ',' {
            i += 1;
            if i < self.chars.len() && self.chars[i] == '}' {
                return Some((min, None, i + 1));
            }
            let start_n = i;
            while i < self.chars.len() && self.chars[i].is_ascii_digit() {
                i += 1;
            }
            if i > start_n && i < self.chars.len() && self.chars[i] == '}' {
                let max: usize = self.chars[start_n..i].iter().collect::<String>().parse().ok()?;
                return Some((min, Some(max), i + 1));
            }
        }
        None
    }
}
