//! Port of `ext/fts5/fts5_expr.c`: the expression tree of a MATCH query and its evaluation
//! against the index (`fts5ExprNodeFirst` / `fts5ExprNodeNext` and the node tests). The
//! query text is parsed by `QueryParser` (the grammar of `fts5parse.y`); `Fts5Expr::build`
//! turns its result into the node / nearset / phrase / term objects of C, applying the
//! rules of `sqlite3Fts5ParseNode`. Function names are the C names in snake case.
#![forbid(unsafe_code)]

use super::*;
use crate::fts5_index::{
    FTS5INDEX_QUERY_DESC, FTS5INDEX_QUERY_PREFIX, MultiIter, PoslistReader, poslist_next64,
    poslist_safe_append,
};

/// `Fts5ExprNode.eType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NodeType {
    /// `FTS5_EOF` (0): matches no rows.
    Eof,
    Or,
    And,
    Not,
    Term,
    String,
}

/// `Fts5ExprTerm`. `syn` is the `pSynonym` chain of the term, in chain order.
#[derive(Debug, Default)]
pub(super) struct ExprTerm {
    pub(super) b_prefix: bool,
    pub(super) b_first: bool,
    /// `pTerm` (`nFullTerm` bytes).
    pub(super) term: Vec<u8>,
    /// `nQueryTerm`: the effective size of the term (up to a 0x00 for tokendata=1).
    pub(super) n_query_term: usize,
    pub(super) iter: Option<MultiIter>,
    pub(super) syn: Vec<ExprTerm>,
}

/// `Fts5ExprPhrase`. `poslist.n` is `n_poslist`; the bytes are `poslist`, or (`alias`) the
/// output of the iterator of term 0 (`fts5ExprNodeTest_TERM` points `poslist.p` at it).
#[derive(Debug, Default)]
pub(super) struct ExprPhrase {
    pub(super) p_node: usize,
    poslist: Vec<u8>,
    pub(super) n_poslist: i32,
    alias: bool,
    pub(super) a_term: Vec<ExprTerm>,
}

/// `Fts5ExprNearset`.
#[derive(Debug, Default)]
pub(super) struct ExprNearset {
    pub(super) n_near: i32,
    pub(super) colset: Option<Vec<i32>>,
    /// Indexes into `Fts5Expr::phrases`.
    pub(super) ap_phrase: Vec<usize>,
}

/// `Fts5ExprNode`.
#[derive(Debug)]
pub(super) struct ExprNode {
    pub(super) e_type: NodeType,
    pub(super) b_eof: bool,
    pub(super) b_nomatch: bool,
    pub(super) i_height: i32,
    /// `xNext != 0`.
    has_next: bool,
    pub(super) i_rowid: i64,
    pub(super) near: Option<ExprNearset>,
    pub(super) children: Vec<usize>,
}

/// `Fts5Expr`. Nodes and phrases live in arenas; `ap_expr_phrase` is `apExprPhrase[]`.
#[derive(Debug)]
pub(super) struct Fts5Expr {
    pub(super) nodes: Vec<ExprNode>,
    pub(super) root: usize,
    pub(super) phrases: Vec<ExprPhrase>,
    pub(super) ap_expr_phrase: Vec<usize>,
    pub(super) b_desc: bool,
    detail: DetailMode,
    b_tokendata: bool,
    /// Buffers of `fts5ExprNearIsMatch` kept for their allocation.
    near_pool: Vec<Vec<u8>>,
}

/// `Fts5PoslistPopulator`.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct PoslistPopulator {
    i_prev: i64,
    b_ok: bool,
    b_miss: bool,
}

/// `Fts5LookaheadReader`.
#[derive(Debug, Clone, Copy, Default)]
struct LookaheadReader {
    n: i32,
    i: i32,
    i_pos: i64,
    i_lookahead: i64,
}

const FTS5_LOOKAHEAD_EOF: i64 = 1 << 62;

impl LookaheadReader {
    /// `fts5LookaheadReaderNext`.
    fn next(&mut self, a: &[u8]) -> bool {
        self.i_pos = self.i_lookahead;
        if poslist_next64(a, self.n, &mut self.i, &mut self.i_lookahead) {
            self.i_lookahead = FTS5_LOOKAHEAD_EOF;
        }
        self.i_pos == FTS5_LOOKAHEAD_EOF
    }
    /// `fts5LookaheadReaderInit`.
    fn init(a: &[u8]) -> Self {
        let mut r = Self {
            n: a.len() as i32,
            ..Self::default()
        };
        r.next(a);
        r.next(a);
        r
    }
}

/// The phrases of a nearset, copied out of the node (on the stack when there are few).
enum PhraseList {
    Inline([usize; 8], usize),
    Heap(Vec<usize>),
}

impl PhraseList {
    fn of(ap: &[usize]) -> Self {
        if ap.len() <= 8 {
            let mut a = [0usize; 8];
            a[..ap.len()].copy_from_slice(ap);
            Self::Inline(a, ap.len())
        } else {
            Self::Heap(ap.to_vec())
        }
    }
    fn as_slice(&self) -> &[usize] {
        match self {
            Self::Inline(a, n) => &a[..*n],
            Self::Heap(v) => v,
        }
    }
}

fn new_node(e_type: NodeType) -> ExprNode {
    ExprNode {
        e_type,
        b_eof: false,
        b_nomatch: false,
        i_height: 0,
        has_next: true,
        i_rowid: 0,
        near: None,
        children: Vec::new(),
    }
}

impl ExprTerm {
    /// The term itself (0) or a synonym (`s-1`).
    fn syn_get(&self, s: usize) -> &ExprTerm {
        if s == 0 { self } else { &self.syn[s - 1] }
    }
    fn syn_get_mut(&mut self, s: usize) -> &mut ExprTerm {
        if s == 0 { self } else { &mut self.syn[s - 1] }
    }
    fn syn_count(&self) -> usize {
        1 + self.syn.len()
    }
    fn iter_eof(&self) -> bool {
        self.iter.as_ref().is_none_or(MultiIter::eof)
    }
    fn iter_rowid(&self) -> i64 {
        self.iter.as_ref().map_or(0, |i| i.i_rowid)
    }
    fn iter_n_data(&self) -> i32 {
        self.iter.as_ref().map_or(0, |i| i.n_data)
    }
    fn iter_data(&self) -> &[u8] {
        self.iter.as_ref().map_or(&[], MultiIter::data)
    }
}

/// `fts5ExprSynonymRowid`: (rowid, all synonym iterators at EOF).
fn expr_synonym_rowid(term: &ExprTerm, b_desc: bool) -> (i64, bool) {
    let mut i_ret: i64 = 0;
    let mut b_ret_valid = false;
    for s in 0..term.syn_count() {
        let p = term.syn_get(s);
        if !p.iter_eof() {
            let i_rowid = p.iter_rowid();
            if !b_ret_valid || (b_desc != (i_rowid < i_ret)) {
                i_ret = i_rowid;
                b_ret_valid = true;
            }
        }
    }
    (i_ret, !b_ret_valid)
}

/// `fts5ExprSynonymList`: the merged position list of the synonyms of `term` at `i_rowid`.
fn expr_synonym_list(term: &ExprTerm, i_rowid: i64) -> Vec<u8> {
    let mut srcs: Vec<&[u8]> = Vec::new();
    let mut readers: Vec<PoslistReader> = Vec::new();
    for s in 0..term.syn_count() {
        let p = term.syn_get(s);
        if !p.iter_eof() && p.iter_rowid() == i_rowid {
            if p.iter_n_data() == 0 {
                continue;
            }
            let d = p.iter_data();
            readers.push(PoslistReader::init(d, d.len() as i32));
            srcs.push(d);
        }
    }
    if readers.len() == 1 {
        return srcs[0].to_vec();
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut writer_prev: i64 = 0;
    let mut i_prev: i64 = -1;
    loop {
        let mut i_min = i64::MAX;
        for (i, r) in readers.iter_mut().enumerate() {
            if !r.b_eof {
                if r.i_pos == i_prev && r.next(srcs[i]) {
                    continue;
                }
                if r.i_pos < i_min {
                    i_min = r.i_pos;
                }
            }
        }
        if i_min == i64::MAX {
            break;
        }
        poslist_safe_append(&mut buf, &mut writer_prev, i_min);
        i_prev = i_min;
    }
    buf
}

impl Fts5Expr {
    // -----------------------------------------------------------------------------------
    // Construction (`sqlite3Fts5ExprNew`, `sqlite3Fts5ParseNode`, ...)
    // -----------------------------------------------------------------------------------

    /// `sqlite3Fts5ExprNew`: parses `text`; `i_col < nCol` applies the implicit column
    /// filter of `col MATCH ...`.
    pub(super) fn new(
        cfg: &Fts5Config,
        b_phrase_to_and: bool,
        i_col: usize,
        text: &str,
    ) -> Result<Self> {
        let (mut parsed, _) = QueryParser::parse_query_opt(cfg, text, b_phrase_to_and)?;
        // If the LHS of the MATCH expression was a user column, apply the implicit
        // column-filter.
        if i_col < cfg.columns.len() {
            if cfg.detail == DetailMode::None {
                return Err(fts5_query_err(
                    "fts5: column queries are not supported (detail=none)",
                ));
            }
            parsed.set_colset(&[i_col]);
        }
        let mut e = Self {
            nodes: Vec::new(),
            root: 0,
            phrases: Vec::new(),
            ap_expr_phrase: Vec::new(),
            b_desc: false,
            detail: cfg.detail,
            b_tokendata: cfg.tokendata,
            near_pool: Vec::new(),
        };
        e.root = e.build(cfg, &parsed, b_phrase_to_and);
        Ok(e)
    }

    fn new_term(&self, bytes: &[u8]) -> ExprTerm {
        // fts5ParseTokenize
        let n = bytes.len().min(FTS5_MAX_TOKEN_SIZE);
        let term = bytes[..n].to_vec();
        let n_query_term = if self.b_tokendata {
            term.iter().position(|&b| b == 0).unwrap_or(term.len())
        } else {
            term.len()
        };
        ExprTerm {
            term,
            n_query_term,
            ..ExprTerm::default()
        }
    }

    /// `fts5ExprAssignXNext` for a string node, then the FTS5_EOF rule of
    /// `sqlite3Fts5ParseNode`.
    fn finish_string_node(&mut self, i_node: usize) {
        let mut e_type = NodeType::String;
        let mut has_next = true;
        if let Some(near) = self.nodes[i_node].near.as_ref() {
            if near.ap_phrase.len() == 1 {
                let ph = &self.phrases[near.ap_phrase[0]];
                if ph.a_term.len() == 1 && ph.a_term[0].syn.is_empty() && !ph.a_term[0].b_first {
                    e_type = NodeType::Term;
                }
            }
            for &k in &near.ap_phrase {
                if self.phrases[k].a_term.is_empty() {
                    has_next = false;
                    e_type = NodeType::Eof;
                }
            }
        }
        let n = &mut self.nodes[i_node];
        n.e_type = e_type;
        n.has_next = has_next;
    }

    /// `fts5ExprAddChildren`.
    fn add_children(&mut self, p: usize, sub: usize) {
        let ii = self.nodes[p].children.len();
        if self.nodes[p].e_type != NodeType::Not && self.nodes[sub].e_type == self.nodes[p].e_type {
            let ch = std::mem::take(&mut self.nodes[sub].children);
            self.nodes[p].children.extend(ch);
        } else {
            self.nodes[p].children.push(sub);
        }
        for k in ii..self.nodes[p].children.len() {
            let h = self.nodes[self.nodes[p].children[k]].i_height + 1;
            if h > self.nodes[p].i_height {
                self.nodes[p].i_height = h;
            }
        }
    }

    /// Builds the node for `q` (the calls of `sqlite3Fts5ParseNode` the parser makes).
    fn build(&mut self, cfg: &Fts5Config, q: &FtsQueryExpr, b_phrase_to_and: bool) -> usize {
        match q {
            FtsQueryExpr::Near(n) => {
                if b_phrase_to_and && n.phrases.first().is_some_and(|p| p.tokens.len() > 1) {
                    // fts5ParsePhraseToAnd
                    let i_and = self.nodes.len();
                    let mut and = new_node(NodeType::And);
                    and.i_height = 1;
                    self.nodes.push(and);
                    for tok in &n.phrases[0].tokens {
                        let term = self.new_term(&tok.raw);
                        let k = self.phrases.len();
                        let i_node = self.nodes.len();
                        self.phrases.push(ExprPhrase {
                            p_node: i_node,
                            a_term: vec![term],
                            ..ExprPhrase::default()
                        });
                        self.ap_expr_phrase.push(k);
                        let mut node = new_node(NodeType::String);
                        node.near = Some(ExprNearset {
                            n_near: 0,
                            colset: n.colset.as_ref().map(|c| c.iter().map(|&x| x as i32).collect()),
                            ap_phrase: vec![k],
                        });
                        self.nodes.push(node);
                        self.finish_string_node(i_node);
                        if n.colset.as_ref().is_some_and(Vec::is_empty) {
                            self.nodes[i_node].e_type = NodeType::Eof;
                            self.nodes[i_node].has_next = false;
                        }
                        self.nodes[i_and].children.push(i_node);
                    }
                    return i_and;
                }
                let i_node = self.nodes.len();
                let mut ap = Vec::with_capacity(n.phrases.len());
                for ph in &n.phrases {
                    let mut a_term: Vec<ExprTerm> = Vec::with_capacity(ph.tokens.len());
                    for tok in &ph.tokens {
                        let mut t = self.new_term(&tok.raw);
                        t.b_prefix = tok.is_prefix;
                        a_term.push(t);
                    }
                    if ph.anchored_start && let Some(t0) = a_term.first_mut() {
                        t0.b_first = true;
                    }
                    let k = self.phrases.len();
                    self.phrases.push(ExprPhrase {
                        p_node: i_node,
                        a_term,
                        ..ExprPhrase::default()
                    });
                    self.ap_expr_phrase.push(k);
                    ap.push(k);
                }
                let mut node = new_node(NodeType::String);
                node.near = Some(ExprNearset {
                    n_near: n.distance as i32,
                    colset: n
                        .colset
                        .as_ref()
                        .map(|c| c.iter().map(|&x| x as i32).collect()),
                    ap_phrase: ap,
                });
                self.nodes.push(node);
                self.finish_string_node(i_node);
                if n.eof {
                    // (a phrase without terms, or a column set that became empty:
                    // fts5ParseSetColset)
                    self.nodes[i_node].e_type = NodeType::Eof;
                    self.nodes[i_node].has_next = false;
                }
                let _ = cfg;
                i_node
            }
            FtsQueryExpr::And(ch) | FtsQueryExpr::Or(ch) => {
                let e_type = if matches!(q, FtsQueryExpr::And(_)) {
                    NodeType::And
                } else {
                    NodeType::Or
                };
                let subs: Vec<usize> = ch
                    .iter()
                    .map(|c| self.build(cfg, c, b_phrase_to_and))
                    .collect();
                let i_node = self.nodes.len();
                self.nodes.push(new_node(e_type));
                for s in subs {
                    self.add_children(i_node, s);
                }
                i_node
            }
            FtsQueryExpr::Not(l, r) => {
                let sl = self.build(cfg, l, b_phrase_to_and);
                let sr = self.build(cfg, r, b_phrase_to_and);
                let i_node = self.nodes.len();
                self.nodes.push(new_node(NodeType::Not));
                self.add_children(i_node, sl);
                self.add_children(i_node, sr);
                i_node
            }
        }
    }

    /// `sqlite3Fts5ExprAnd`: `*pp1 = p1 AND p2`; the phrases of `p2` come first.
    pub(super) fn and(p1: Option<Self>, p2: Option<Self>) -> Option<Self> {
        let (mut p1, mut p2) = match (p1, p2) {
            (Some(a), Some(b)) => (a, b),
            (None, b) => return b,
            (a, None) => return a,
        };
        let node_off = p1.nodes.len();
        let phrase_off = p1.phrases.len();
        for n in &mut p2.nodes {
            for c in &mut n.children {
                *c += node_off;
            }
            if let Some(near) = n.near.as_mut() {
                for k in &mut near.ap_phrase {
                    *k += phrase_off;
                }
            }
        }
        for ph in &mut p2.phrases {
            ph.p_node += node_off;
        }
        let p2_root = p2.root + node_off;
        p1.nodes.append(&mut p2.nodes);
        p1.phrases.append(&mut p2.phrases);
        // sqlite3Fts5ParseNode(FTS5_AND, p1->pRoot, p2->pRoot)
        let i_node = p1.nodes.len();
        p1.nodes.push(new_node(NodeType::And));
        let r1 = p1.root;
        p1.add_children(i_node, r1);
        p1.add_children(i_node, p2_root);
        p1.root = i_node;
        let mut ap: Vec<usize> = p2.ap_expr_phrase.iter().map(|k| k + phrase_off).collect();
        ap.extend(p1.ap_expr_phrase.iter().copied());
        p1.ap_expr_phrase = ap;
        Some(p1)
    }

    /// `sqlite3Fts5ExprPattern` (trigram tokenizer): an expression that matches a superset
    /// of the rows the LIKE / GLOB pattern `text` matches; `None` when the pattern has no
    /// run of three characters.
    pub(super) fn pattern(
        cfg: &Fts5Config,
        b_glob: bool,
        i_col: usize,
        text: &[u8],
    ) -> Result<Option<Self>> {
        let n_text = text.len();
        let at = |i: usize| -> u8 { text.get(i).copied().unwrap_or(0) };
        let a_spec: [u8; 3] = if b_glob { [b'*', b'?', b'['] } else { [b'_', b'%', 0] };
        let mut expr: Vec<u8> = Vec::with_capacity(n_text * 4 + 1);
        let mut i = 0usize;
        let mut i_first = 0usize;
        let mut i_col = i_col;
        while i <= n_text {
            if i == n_text || at(i) == a_spec[0] || at(i) == a_spec[1] || at(i) == a_spec[2] {
                // fts5ExprCountChar
                let n_char = text
                    .get(i_first..i)
                    .unwrap_or(&[])
                    .iter()
                    .filter(|&&b| (b & 0xC0) != 0x80)
                    .count();
                if n_char >= 3 {
                    expr.push(b'"');
                    for jj in i_first..i {
                        expr.push(text[jj]);
                        if text[jj] == b'"' {
                            expr.push(b'"');
                        }
                    }
                    expr.push(b'"');
                    expr.push(b' ');
                }
                if at(i) == a_spec[2] {
                    i += 2;
                    if at(i - 1) == b'^' {
                        i += 1;
                    }
                    while i < n_text && at(i) != b']' {
                        i += 1;
                    }
                }
                i_first = i + 1;
            }
            i += 1;
        }
        if expr.is_empty() {
            return Ok(None);
        }
        let mut b_and = false;
        if cfg.detail != DetailMode::Full {
            b_and = true;
            if cfg.detail == DetailMode::None {
                i_col = cfg.columns.len();
            }
        }
        let z = String::from_utf8_lossy(&expr).into_owned();
        Self::new(cfg, b_and, i_col, &z).map(Some)
    }

    /// `sqlite3Fts5ExprClonePhrase`: a new expression made of phrase `i_phrase` alone.
    pub(super) fn clone_phrase(&self, i_phrase: usize) -> Result<Self> {
        let Some(&k) = self.ap_expr_phrase.get(i_phrase) else {
            return Err(SqliteError::new(ErrorCode::Range, "column index out of range"));
        };
        let orig = &self.phrases[k];
        let colset = self.nodes[orig.p_node]
            .near
            .as_ref()
            .and_then(|n| n.colset.clone());
        let mut a_term: Vec<ExprTerm> = Vec::with_capacity(orig.a_term.len());
        for t in &orig.a_term {
            let mut nt = self.new_term(&t.term);
            for s in &t.syn {
                // (fts5ParseTokenize with FTS5_TOKEN_COLOCATED: newest first)
                nt.syn.insert(0, self.new_term(&s.term));
            }
            nt.b_prefix = t.b_prefix;
            nt.b_first = t.b_first;
            a_term.push(nt);
        }
        let mut node = new_node(NodeType::String);
        if orig.a_term.len() == 1 && orig.a_term[0].syn.is_empty() && !orig.a_term[0].b_first {
            node.e_type = NodeType::Term;
        }
        node.near = Some(ExprNearset {
            n_near: 0,
            colset,
            ap_phrase: vec![0],
        });
        Ok(Self {
            nodes: vec![node],
            root: 0,
            phrases: vec![ExprPhrase {
                p_node: 0,
                a_term,
                ..ExprPhrase::default()
            }],
            ap_expr_phrase: vec![0],
            b_desc: false,
            detail: self.detail,
            b_tokendata: self.b_tokendata,
            near_pool: Vec::new(),
        })
    }

    // -----------------------------------------------------------------------------------
    // Phrase position lists
    // -----------------------------------------------------------------------------------

    /// `pPhrase->poslist.p` / `.n`.
    fn phrase_poslist(&self, k: usize) -> &[u8] {
        let ph = &self.phrases[k];
        let n = ph.n_poslist.max(0) as usize;
        if ph.alias {
            let d = ph.a_term.first().map_or(&[][..], ExprTerm::iter_data);
            &d[..n.min(d.len())]
        } else {
            &ph.poslist[..n.min(ph.poslist.len())]
        }
    }

    fn set_phrase_poslist(&mut self, k: usize, buf: Vec<u8>) {
        let ph = &mut self.phrases[k];
        ph.n_poslist = buf.len() as i32;
        ph.poslist = buf;
        ph.alias = false;
    }

    /// `fts5ExprPhraseIsMatch`: the term iterators of the phrase all point to the row of
    /// the node; decides whether the row really matches and fills the phrase's poslist.
    fn expr_phrase_is_match(&mut self, i_node: usize, k: usize) -> bool {
        let i_rowid = self.nodes[i_node].i_rowid;
        let mut out = std::mem::take(&mut self.phrases[k].poslist);
        out.clear();
        let mut writer_prev: i64 = 0;
        if self.phrases[k].a_term.len() <= 8 && self.phrases[k].a_term.iter().all(|t| t.syn.is_empty())
        {
            // (the usual phrase: no synonyms, a few terms — the readers live on the stack)
            let ph = &self.phrases[k];
            let b_first = ph.a_term.first().is_some_and(|t| t.b_first);
            let n_term = ph.a_term.len();
            let mut srcs: [&[u8]; 8] = [&[]; 8];
            let mut a_iter: [PoslistReader; 8] = [PoslistReader::default(); 8];
            let mut done = false;
            for (i, t) in ph.a_term.iter().enumerate() {
                let a = t.iter_data();
                a_iter[i] = PoslistReader::init(a, a.len() as i32);
                srcs[i] = a;
                if a_iter[i].b_eof {
                    done = true;
                    break;
                }
            }
            'ismatch: while !done {
                let mut i_pos = a_iter[0].i_pos;
                loop {
                    let mut b_match = true;
                    for i in 0..n_term {
                        let i_adj = i_pos + i as i64;
                        if a_iter[i].i_pos != i_adj {
                            b_match = false;
                            while a_iter[i].i_pos < i_adj {
                                if a_iter[i].next(srcs[i]) {
                                    break 'ismatch;
                                }
                            }
                            if a_iter[i].i_pos > i_adj {
                                i_pos = a_iter[i].i_pos - i as i64;
                            }
                        }
                    }
                    if b_match {
                        break;
                    }
                }
                // Append position iPos to the output
                if !b_first || (i_pos & 0x7FFF_FFFF) == 0 {
                    poslist_safe_append(&mut out, &mut writer_prev, i_pos);
                }
                for i in 0..n_term {
                    if a_iter[i].next(srcs[i]) {
                        break 'ismatch;
                    }
                }
            }
        } else {
            let ph = &self.phrases[k];
            let b_first = ph.a_term.first().is_some_and(|t| t.b_first);
            let n_term = ph.a_term.len();
            // Initialize a term iterator for each term in the phrase
            let mut srcs: Vec<std::borrow::Cow<'_, [u8]>> = Vec::with_capacity(n_term);
            let mut a_iter: Vec<PoslistReader> = Vec::with_capacity(n_term);
            let mut done = false;
            for t in &ph.a_term {
                let a: std::borrow::Cow<'_, [u8]> = if !t.syn.is_empty() {
                    std::borrow::Cow::Owned(expr_synonym_list(t, i_rowid))
                } else {
                    std::borrow::Cow::Borrowed(t.iter_data())
                };
                let r = PoslistReader::init(&a, a.len() as i32);
                srcs.push(a);
                a_iter.push(r);
                if r.b_eof {
                    done = true;
                    break;
                }
            }
            'ismatch: while !done {
                let mut i_pos = a_iter[0].i_pos;
                loop {
                    let mut b_match = true;
                    for i in 0..n_term {
                        let i_adj = i_pos + i as i64;
                        if a_iter[i].i_pos != i_adj {
                            b_match = false;
                            while a_iter[i].i_pos < i_adj {
                                if a_iter[i].next(&srcs[i]) {
                                    break 'ismatch;
                                }
                            }
                            if a_iter[i].i_pos > i_adj {
                                i_pos = a_iter[i].i_pos - i as i64;
                            }
                        }
                    }
                    if b_match {
                        break;
                    }
                }
                // Append position iPos to the output
                if !b_first || (i_pos & 0x7FFF_FFFF) == 0 {
                    poslist_safe_append(&mut out, &mut writer_prev, i_pos);
                }
                for i in 0..n_term {
                    if a_iter[i].next(&srcs[i]) {
                        break 'ismatch;
                    }
                }
            }
        }
        let b_match = !out.is_empty();
        sqliters_types::dtrace!(
            "TR|f5q|phrase|nterm={} match={}",
            self.phrases[k].a_term.len(),
            i32::from(b_match)
        );
        self.set_phrase_poslist(k, out);
        b_match
    }

    /// `fts5ExprNearIsMatch`: all phrases of the nearset point to the same row and their
    /// poslists are populated; true if the row has instances close enough together. The
    /// poslists are trimmed to the entries that meet the constraint.
    fn expr_near_is_match(&mut self, i_node: usize) -> bool {
        let (ap_list, n_near) = match self.nodes[i_node].near.as_ref() {
            Some(n) => (PhraseList::of(&n.ap_phrase), i64::from(n.n_near)),
            None => return false,
        };
        let ap = ap_list.as_slice();
        let n_phrase = ap.len();
        // (the output lists: buffers kept from earlier rows)
        let mut outs_inline: [Vec<u8>; 4] = std::array::from_fn(|_| Vec::new());
        let mut outs_heap: Vec<Vec<u8>> = Vec::new();
        let outs: &mut [Vec<u8>] = if n_phrase <= 4 {
            &mut outs_inline[..n_phrase]
        } else {
            outs_heap.resize(n_phrase, Vec::new());
            &mut outs_heap
        };
        for o in outs.iter_mut() {
            if let Some(mut b) = self.near_pool.pop() {
                b.clear();
                *o = b;
            }
        }
        let b_ret = {
            // Initialize a lookahead iterator for each phrase (the lists are read where they
            // are; the trimmed lists are written to `outs` and replace them afterwards).
            let mut inputs_inline: [&[u8]; 4] = [&[]; 4];
            let mut readers_inline = [LookaheadReader::default(); 4];
            let mut writers_inline = [0i64; 4];
            let mut n_terms_inline = [0i64; 4];
            let (inputs_heap, mut readers_heap, mut writers_heap, n_terms_heap);
            let (inputs, readers, writers, n_terms): (
                &[&[u8]],
                &mut [LookaheadReader],
                &mut [i64],
                &[i64],
            ) = if n_phrase <= 4 {
                for (i, &k) in ap.iter().enumerate() {
                    let a = self.phrase_poslist(k);
                    inputs_inline[i] = a;
                    readers_inline[i] = LookaheadReader::init(a);
                    n_terms_inline[i] = self.phrases[k].a_term.len() as i64;
                }
                (
                    &inputs_inline[..n_phrase],
                    &mut readers_inline[..n_phrase],
                    &mut writers_inline[..n_phrase],
                    &n_terms_inline[..n_phrase],
                )
            } else {
                inputs_heap = ap.iter().map(|&k| self.phrase_poslist(k)).collect::<Vec<&[u8]>>();
                readers_heap =
                    inputs_heap.iter().map(|a| LookaheadReader::init(a)).collect::<Vec<_>>();
                writers_heap = vec![0i64; n_phrase];
                n_terms_heap =
                    ap.iter().map(|&k| self.phrases[k].a_term.len() as i64).collect::<Vec<_>>();
                (&inputs_heap[..], &mut readers_heap[..], &mut writers_heap[..], &n_terms_heap[..])
            };
            'ismatch: loop {
                // This block advances the phrase iterators until they point to a set of
                // entries that together comprise a match.
                let mut i_max = readers[0].i_pos;
                loop {
                    let mut b_match = true;
                    for i in 0..n_phrase {
                        let i_min = i_max - n_terms[i] - n_near;
                        if readers[i].i_pos < i_min || readers[i].i_pos > i_max {
                            b_match = false;
                            while readers[i].i_pos < i_min {
                                if readers[i].next(inputs[i]) {
                                    break 'ismatch;
                                }
                            }
                            if readers[i].i_pos > i_max {
                                i_max = readers[i].i_pos;
                            }
                        }
                    }
                    if b_match {
                        break;
                    }
                }
                // Add an entry to each output position list
                for i in 0..n_phrase {
                    let i_pos = readers[i].i_pos;
                    if outs[i].is_empty() || i_pos != writers[i] {
                        poslist_safe_append(&mut outs[i], &mut writers[i], i_pos);
                    }
                }
                let mut i_adv = 0usize;
                let mut i_min = readers[0].i_lookahead;
                for i in 0..n_phrase {
                    if readers[i].i_lookahead < i_min {
                        i_min = readers[i].i_lookahead;
                        i_adv = i;
                    }
                }
                if readers[i_adv].next(inputs[i_adv]) {
                    break 'ismatch;
                }
            }
            !outs[0].is_empty()
        };
        sqliters_types::dtrace!("TR|f5q|near|n={} match={}", n_phrase, i32::from(b_ret));
        for (i, &k) in ap.iter().enumerate() {
            let o = std::mem::take(&mut outs[i]);
            let old = std::mem::take(&mut self.phrases[k].poslist);
            self.set_phrase_poslist(k, o);
            if old.capacity() > 0 && self.near_pool.len() < 16 {
                self.near_pool.push(old);
            }
        }
        b_ret
    }

    /// `fts5ExprNearTest`.
    fn expr_near_test(&mut self, i_node: usize) -> bool {
        let i_rowid = self.nodes[i_node].i_rowid;
        let Some(near) = self.nodes[i_node].near.as_ref() else {
            return false;
        };
        let ap_list = PhraseList::of(&near.ap_phrase);
        let ap = ap_list.as_slice();
        let has_colset = near.colset.is_some();
        if self.detail != DetailMode::Full {
            let k = ap[0];
            let ph = &mut self.phrases[k];
            let mut hit = false;
            if let Some(t0) = ph.a_term.first() {
                for s in 0..t0.syn_count() {
                    let t = t0.syn_get(s);
                    if !t.iter_eof() && t.iter_rowid() == i_rowid && t.iter_n_data() > 0 {
                        hit = true;
                    }
                }
            }
            ph.alias = false;
            ph.poslist.clear();
            if hit {
                // (`pPhrase->poslist.n = 1`: a marker, the bytes are never read)
                ph.poslist.push(0);
            }
            ph.n_poslist = i32::from(hit);
            hit
        } else {
            // Check that each phrase in the nearset matches the current row. Populate the
            // pPhrase->poslist buffers at the same time. If any phrase is not a match,
            // break out of the loop early.
            let mut i = 0usize;
            while i < ap.len() {
                let k = ap[i];
                let ph = &self.phrases[k];
                if ph.a_term.len() > 1
                    || ph.a_term.first().is_some_and(|t| !t.syn.is_empty())
                    || has_colset
                    || ph.a_term.first().is_some_and(|t| t.b_first)
                {
                    if !self.expr_phrase_is_match(i_node, k) {
                        break;
                    }
                } else {
                    // fts5BufferSet(&rc, &pPhrase->poslist, pIter->nData, pIter->pData)
                    let ph = &mut self.phrases[k];
                    let mut buf = std::mem::take(&mut ph.poslist);
                    buf.clear();
                    buf.extend_from_slice(ph.a_term.first().map_or(&[][..], ExprTerm::iter_data));
                    self.set_phrase_poslist(k, buf);
                }
                i += 1;
            }
            i == ap.len() && (i == 1 || self.expr_near_is_match(i_node))
        }
    }

    /// `fts5ExprNearInitAll`: opens the iterators of every term of the nearset.
    fn expr_near_init_all(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
    ) -> Result<()> {
        let b_desc = self.b_desc;
        let Some(near) = self.nodes[i_node].near.as_ref() else {
            return Ok(());
        };
        let ap = near.ap_phrase.clone();
        let colset = near.colset.clone();
        for &k in &ap {
            if self.phrases[k].a_term.is_empty() {
                self.nodes[i_node].b_eof = true;
                return Ok(());
            }
            for j in 0..self.phrases[k].a_term.len() {
                let b_prefix = self.phrases[k].a_term[j].b_prefix;
                let mut b_hit = false;
                for s in 0..self.phrases[k].a_term[j].syn_count() {
                    let p = self.phrases[k].a_term[j].syn_get_mut(s);
                    p.iter = None;
                    let flags = (if b_prefix { FTS5INDEX_QUERY_PREFIX } else { 0 })
                        | (if b_desc { FTS5INDEX_QUERY_DESC } else { 0 });
                    let it = ix.query(ctx, &p.term[..p.n_query_term], flags, colset.as_deref())?;
                    if !it.eof() {
                        b_hit = true;
                    }
                    p.iter = Some(it);
                }
                if !b_hit {
                    self.nodes[i_node].b_eof = true;
                    return Ok(());
                }
            }
        }
        self.nodes[i_node].b_eof = false;
        Ok(())
    }

    /// `fts5RowidCmp`.
    #[inline]
    fn rowid_cmp(&self, i_lhs: i64, i_rhs: i64) -> i32 {
        if !self.b_desc {
            if i_lhs < i_rhs {
                return -1;
            }
            i32::from(i_lhs > i_rhs)
        } else {
            if i_lhs > i_rhs {
                return -1;
            }
            i32::from(i_lhs < i_rhs)
        }
    }

    /// `fts5ExprSetEof`.
    fn expr_set_eof(&mut self, i_node: usize) {
        self.nodes[i_node].b_eof = true;
        self.nodes[i_node].b_nomatch = false;
        for c in self.nodes[i_node].children.clone() {
            self.expr_set_eof(c);
        }
    }

    /// `fts5ExprNodeZeroPoslist`.
    fn expr_node_zero_poslist(&mut self, i_node: usize) {
        if matches!(self.nodes[i_node].e_type, NodeType::String | NodeType::Term) {
            let ap = self.nodes[i_node]
                .near
                .as_ref()
                .map(|n| n.ap_phrase.clone())
                .unwrap_or_default();
            for k in ap {
                self.phrases[k].n_poslist = 0;
            }
        } else {
            for c in self.nodes[i_node].children.clone() {
                self.expr_node_zero_poslist(c);
            }
        }
    }

    /// `fts5NodeCompare`.
    fn node_compare(&self, p1: usize, p2: usize) -> i32 {
        if self.nodes[p2].b_eof {
            return -1;
        }
        if self.nodes[p1].b_eof {
            return 1;
        }
        self.rowid_cmp(self.nodes[p1].i_rowid, self.nodes[p2].i_rowid)
    }

    /// `fts5ExprNodeTest_STRING`: advances the term iterators of the nearset until they
    /// all point to the same rowid, then tests that row.
    fn expr_node_test_string(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
    ) -> Result<()> {
        let b_desc = self.b_desc;
        let ap_list = PhraseList::of(
            self.nodes[i_node].near.as_ref().map_or(&[][..], |n| n.ap_phrase.as_slice()),
        );
        let ap = ap_list.as_slice();
        // Initialize iLast, the "lastest" rowid any iterator points to.
        let left0 = &self.phrases[ap[0]].a_term[0];
        let mut i_last = if !left0.syn.is_empty() {
            expr_synonym_rowid(left0, b_desc).0
        } else {
            left0.iter_rowid()
        };
        loop {
            let mut b_match = true;
            for &k in ap {
                for j in 0..self.phrases[k].a_term.len() {
                    let term = &mut self.phrases[k].a_term[j];
                    if !term.syn.is_empty() {
                        let i_rowid = expr_synonym_rowid(term, b_desc).0;
                        if i_rowid == i_last {
                            continue;
                        }
                        b_match = false;
                        // fts5ExprSynonymAdvanceto
                        for s in 0..term.syn_count() {
                            let p = term.syn_get_mut(s);
                            if !p.iter_eof() {
                                let r = p.iter_rowid();
                                if ((!b_desc && i_last > r) || (b_desc && i_last < r))
                                    && let Some(it) = p.iter.as_mut()
                                {
                                    ix.iter_next_from(ctx, it, i_last)?;
                                }
                            }
                        }
                        let (r, b_eof) = expr_synonym_rowid(term, b_desc);
                        i_last = r;
                        if b_eof {
                            self.nodes[i_node].b_nomatch = false;
                            self.nodes[i_node].b_eof = true;
                            return Ok(());
                        }
                    } else {
                        let Some(it) = term.iter.as_mut() else {
                            continue;
                        };
                        if it.i_rowid == i_last {
                            continue;
                        }
                        b_match = false;
                        // fts5ExprAdvanceto
                        let mut i_rowid = it.i_rowid;
                        if (!b_desc && i_last > i_rowid) || (b_desc && i_last < i_rowid) {
                            ix.iter_next_from(ctx, it, i_last)?;
                            if it.eof() {
                                self.nodes[i_node].b_eof = true;
                                return Ok(());
                            }
                            i_rowid = it.i_rowid;
                        }
                        i_last = i_rowid;
                    }
                }
            }
            if b_match {
                break;
            }
        }
        self.nodes[i_node].i_rowid = i_last;
        let hit = self.expr_near_test(i_node);
        self.nodes[i_node].b_nomatch = !hit;
        Ok(())
    }

    /// `fts5ExprNodeNext_STRING`.
    fn expr_node_next_string(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
        b_from_valid: bool,
        i_from: i64,
    ) -> Result<()> {
        let b_desc = self.b_desc;
        let k0 = self.nodes[i_node].near.as_ref().map_or(0, |n| n.ap_phrase[0]);
        self.nodes[i_node].b_nomatch = false;
        let term = &mut self.phrases[k0].a_term[0];
        let b_eof;
        if !term.syn.is_empty() {
            let mut eof = true;
            // Find the firstest rowid any synonym points to.
            let i_rowid = expr_synonym_rowid(term, b_desc).0;
            // Advance each iterator that currently points to iRowid. Or, if iFrom is
            // valid - each iterator that points to a rowid before iFrom.
            for s in 0..term.syn_count() {
                let p = term.syn_get_mut(s);
                if !p.iter_eof() {
                    let ii = p.iter_rowid();
                    if ii == i_rowid || (b_from_valid && ii != i_from && (ii > i_from) == b_desc) {
                        if let Some(it) = p.iter.as_mut() {
                            if b_from_valid {
                                ix.iter_next_from(ctx, it, i_from)?;
                            } else {
                                ix.iter_next(ctx, it)?;
                            }
                            if !it.eof() {
                                eof = false;
                            }
                        }
                    } else {
                        eof = false;
                    }
                }
            }
            b_eof = eof;
        } else {
            let Some(it) = term.iter.as_mut() else {
                self.nodes[i_node].b_eof = true;
                return Ok(());
            };
            if b_from_valid {
                ix.iter_next_from(ctx, it, i_from)?;
            } else {
                ix.iter_next(ctx, it)?;
            }
            b_eof = it.eof();
        }
        self.nodes[i_node].b_eof = b_eof;
        if !b_eof {
            self.expr_node_test_string(ix, ctx, i_node)?;
        }
        Ok(())
    }

    /// `fts5ExprNodeTest_TERM`.
    fn expr_node_test_term(&mut self, i_node: usize) {
        let k0 = self.nodes[i_node].near.as_ref().map_or(0, |n| n.ap_phrase[0]);
        let detail = self.detail;
        let ph = &mut self.phrases[k0];
        let (n_data, i_rowid) = ph.a_term[0]
            .iter
            .as_ref()
            .map_or((0, 0), |it| (it.n_data, it.i_rowid));
        ph.n_poslist = n_data;
        if detail == DetailMode::Full {
            ph.alias = true;
        } else if !ph.alias && (ph.poslist.len() as i32) < n_data {
            ph.poslist.resize(n_data.max(0) as usize, 0);
        }
        let n = &mut self.nodes[i_node];
        n.i_rowid = i_rowid;
        n.b_nomatch = n_data == 0;
    }

    /// `fts5ExprNodeNext_TERM`.
    fn expr_node_next_term(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
        b_from_valid: bool,
        i_from: i64,
    ) -> Result<()> {
        let k0 = self.nodes[i_node].near.as_ref().map_or(0, |n| n.ap_phrase[0]);
        let Some(it) = self.phrases[k0].a_term[0].iter.as_mut() else {
            self.nodes[i_node].b_eof = true;
            self.nodes[i_node].b_nomatch = false;
            return Ok(());
        };
        if b_from_valid {
            ix.iter_next_from(ctx, it, i_from)?;
        } else {
            ix.iter_next(ctx, it)?;
        }
        if !it.eof() {
            self.expr_node_test_term(i_node);
        } else {
            self.nodes[i_node].b_eof = true;
            self.nodes[i_node].b_nomatch = false;
        }
        Ok(())
    }

    /// `fts5ExprNodeTest_OR`.
    fn expr_node_test_or(&mut self, i_node: usize) {
        let ch = &self.nodes[i_node].children;
        let mut p_next = ch[0];
        for &c in &ch[1..] {
            let cmp = self.node_compare(p_next, c);
            if cmp > 0 || (cmp == 0 && !self.nodes[c].b_nomatch) {
                p_next = c;
            }
        }
        let (r, e, nm) = {
            let n = &self.nodes[p_next];
            (n.i_rowid, n.b_eof, n.b_nomatch)
        };
        let n = &mut self.nodes[i_node];
        n.i_rowid = r;
        n.b_eof = e;
        n.b_nomatch = nm;
    }

    /// `fts5ExprNodeNext_OR`.
    fn expr_node_next_or(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
        b_from_valid: bool,
        i_from: i64,
    ) -> Result<()> {
        let i_last = self.nodes[i_node].i_rowid;
        for ci in 0..self.nodes[i_node].children.len() {
            let c = self.nodes[i_node].children[ci];
            if !self.nodes[c].b_eof
                && (self.nodes[c].i_rowid == i_last
                    || (b_from_valid && self.rowid_cmp(self.nodes[c].i_rowid, i_from) < 0))
            {
                self.expr_node_next(ix, ctx, c, b_from_valid, i_from)?;
            }
        }
        self.expr_node_test_or(i_node);
        Ok(())
    }

    /// `fts5ExprNodeTest_AND`.
    fn expr_node_test_and(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_and: usize,
    ) -> Result<()> {
        let mut i_last = self.nodes[i_and].i_rowid;
        let n_child = self.nodes[i_and].children.len();
        loop {
            self.nodes[i_and].b_nomatch = false;
            let mut b_match = true;
            for ci in 0..n_child {
                let c = self.nodes[i_and].children[ci];
                let cmp = self.rowid_cmp(i_last, self.nodes[c].i_rowid);
                if cmp > 0 {
                    // Advance pChild until it points to iLast or laster
                    self.expr_node_next(ix, ctx, c, true, i_last)?;
                }
                // If the child node is now at EOF, so is the parent AND node. Otherwise,
                // the child node is guaranteed to have advanced at least as far as rowid
                // iLast. So if it is not at exactly iLast, pChild->iRowid is the new
                // lastest rowid seen so far.
                if self.nodes[c].b_eof {
                    self.expr_set_eof(i_and);
                    b_match = true;
                    break;
                } else if i_last != self.nodes[c].i_rowid {
                    b_match = false;
                    i_last = self.nodes[c].i_rowid;
                }
                if self.nodes[c].b_nomatch {
                    self.nodes[i_and].b_nomatch = true;
                }
            }
            if b_match {
                break;
            }
        }
        if self.nodes[i_and].b_nomatch && i_and != self.root {
            self.expr_node_zero_poslist(i_and);
        }
        self.nodes[i_and].i_rowid = i_last;
        Ok(())
    }

    /// `fts5ExprNodeTest_NOT`.
    fn expr_node_test_not(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
    ) -> Result<()> {
        let p1 = self.nodes[i_node].children[0];
        let p2 = self.nodes[i_node].children[1];
        while !self.nodes[p1].b_eof {
            let mut cmp = self.node_compare(p1, p2);
            if cmp > 0 {
                let r = self.nodes[p1].i_rowid;
                self.expr_node_next(ix, ctx, p2, true, r)?;
                cmp = self.node_compare(p1, p2);
            }
            if cmp != 0 || self.nodes[p2].b_nomatch {
                break;
            }
            self.expr_node_next(ix, ctx, p1, false, 0)?;
        }
        let (e, nm, r) = {
            let n = &self.nodes[p1];
            (n.b_eof, n.b_nomatch, n.i_rowid)
        };
        let n = &mut self.nodes[i_node];
        n.b_eof = e;
        n.b_nomatch = nm;
        n.i_rowid = r;
        if e {
            self.expr_node_zero_poslist(p2);
        }
        Ok(())
    }

    /// `fts5ExprNodeNext` (`pNode->xNext`).
    fn expr_node_next(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
        b_from_valid: bool,
        i_from: i64,
    ) -> Result<()> {
        match self.nodes[i_node].e_type {
            NodeType::String => self.expr_node_next_string(ix, ctx, i_node, b_from_valid, i_from),
            NodeType::Term => self.expr_node_next_term(ix, ctx, i_node, b_from_valid, i_from),
            NodeType::Or => self.expr_node_next_or(ix, ctx, i_node, b_from_valid, i_from),
            NodeType::And => {
                // fts5ExprNodeNext_AND
                let c0 = self.nodes[i_node].children[0];
                self.expr_node_next(ix, ctx, c0, b_from_valid, i_from)?;
                self.expr_node_test_and(ix, ctx, i_node)
            }
            NodeType::Not => {
                // fts5ExprNodeNext_NOT
                let c0 = self.nodes[i_node].children[0];
                self.expr_node_next(ix, ctx, c0, b_from_valid, i_from)?;
                self.expr_node_test_not(ix, ctx, i_node)
            }
            NodeType::Eof => Ok(()),
        }
    }

    /// `fts5ExprNodeTest`: if the node does not point to a match it is advanced until it
    /// does or reaches EOF.
    fn expr_node_test(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
    ) -> Result<()> {
        if !self.nodes[i_node].b_eof {
            match self.nodes[i_node].e_type {
                NodeType::String => self.expr_node_test_string(ix, ctx, i_node)?,
                NodeType::Term => self.expr_node_test_term(i_node),
                NodeType::And => self.expr_node_test_and(ix, ctx, i_node)?,
                NodeType::Or => self.expr_node_test_or(i_node),
                NodeType::Not | NodeType::Eof => self.expr_node_test_not(ix, ctx, i_node)?,
            }
        }
        Ok(())
    }

    /// `fts5ExprNodeFirst`.
    fn expr_node_first(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_node: usize,
    ) -> Result<()> {
        self.nodes[i_node].b_eof = false;
        self.nodes[i_node].b_nomatch = false;
        if matches!(self.nodes[i_node].e_type, NodeType::String | NodeType::Term) {
            // Initialize all term iterators in the NEAR object.
            self.expr_near_init_all(ix, ctx, i_node)?;
        } else if !self.nodes[i_node].has_next {
            self.nodes[i_node].b_eof = true;
        } else {
            let children = self.nodes[i_node].children.clone();
            let mut n_eof = 0usize;
            for &c in &children {
                self.expr_node_first(ix, ctx, c)?;
                n_eof += usize::from(self.nodes[c].b_eof);
            }
            self.nodes[i_node].i_rowid = self.nodes[children[0]].i_rowid;
            match self.nodes[i_node].e_type {
                NodeType::And => {
                    if n_eof > 0 {
                        self.expr_set_eof(i_node);
                    }
                }
                NodeType::Or => {
                    if children.len() == n_eof {
                        self.expr_set_eof(i_node);
                    }
                }
                _ => {
                    self.nodes[i_node].b_eof = self.nodes[children[0]].b_eof;
                }
            }
        }
        self.expr_node_test(ix, ctx, i_node)
    }

    /// `sqlite3Fts5ExprFirst`.
    pub(super) fn first(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_first: i64,
        i_last: i64,
        b_desc: bool,
    ) -> Result<()> {
        let root = self.root;
        self.b_desc = b_desc;
        self.expr_node_first(ix, ctx, root)?;
        // If not at EOF but the current rowid occurs earlier than iFirst in the iteration
        // order, move to document iFirst or later.
        if !self.nodes[root].b_eof && self.rowid_cmp(self.nodes[root].i_rowid, i_first) < 0 {
            self.expr_node_next(ix, ctx, root, true, i_first)?;
        }
        // If the iterator is not at a real match, skip forward until it is.
        while self.nodes[root].b_nomatch {
            self.expr_node_next(ix, ctx, root, false, 0)?;
        }
        if self.rowid_cmp(self.nodes[root].i_rowid, i_last) > 0 {
            self.nodes[root].b_eof = true;
        }
        Ok(())
    }

    /// `sqlite3Fts5ExprNext`.
    pub(super) fn next(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_last: i64,
    ) -> Result<()> {
        let root = self.root;
        loop {
            self.expr_node_next(ix, ctx, root, false, 0)?;
            if !self.nodes[root].b_nomatch {
                break;
            }
        }
        if self.rowid_cmp(self.nodes[root].i_rowid, i_last) > 0 {
            self.nodes[root].b_eof = true;
        }
        Ok(())
    }

    /// `sqlite3Fts5ExprEof`.
    #[inline]
    pub(super) fn eof(&self) -> bool {
        self.nodes[self.root].b_eof
    }

    /// `sqlite3Fts5ExprRowid`.
    #[inline]
    pub(super) fn rowid(&self) -> i64 {
        self.nodes[self.root].i_rowid
    }

    // -----------------------------------------------------------------------------------
    // Interface used by the cursor and the auxiliary-function API
    // -----------------------------------------------------------------------------------

    /// `sqlite3Fts5ExprPhraseCount`.
    pub(super) fn phrase_count(&self) -> usize {
        self.ap_expr_phrase.len()
    }

    /// `sqlite3Fts5ExprPhraseSize`.
    pub(super) fn phrase_size(&self, i_phrase: i32) -> i32 {
        if i_phrase < 0 || i_phrase as usize >= self.ap_expr_phrase.len() {
            return 0;
        }
        self.phrases[self.ap_expr_phrase[i_phrase as usize]].a_term.len() as i32
    }

    /// `sqlite3Fts5ExprPoslist`: the current position list of phrase `i_phrase`.
    pub(super) fn poslist(&self, i_phrase: usize) -> &[u8] {
        let k = self.ap_expr_phrase[i_phrase];
        let node = &self.nodes[self.phrases[k].p_node];
        if !node.b_eof && node.i_rowid == self.nodes[self.root].i_rowid {
            self.phrase_poslist(k)
        } else {
            &[]
        }
    }

    /// `sqlite3Fts5ExprClearPoslists` (detail=col / detail=none).
    pub(super) fn clear_poslists(&mut self, b_live: bool) -> Vec<PoslistPopulator> {
        let root_rowid = self.nodes[self.root].i_rowid;
        let mut ret = vec![PoslistPopulator::default(); self.ap_expr_phrase.len()];
        for (i, &k) in self.ap_expr_phrase.iter().enumerate() {
            let node = &self.nodes[self.phrases[k].p_node];
            let ph = &mut self.phrases[k];
            if b_live && (ph.n_poslist == 0 || node.i_rowid != root_rowid || node.b_eof) {
                ret[i].b_miss = true;
            } else {
                ph.n_poslist = 0;
                ph.poslist.clear();
                ph.alias = false;
            }
        }
        ret
    }

    /// Per phrase, the `bMiss` that `sqlite3Fts5ExprClearPoslists(pExpr, 1)` would set on
    /// the current row (nothing is cleared).
    pub(super) fn live_miss(&self) -> Vec<bool> {
        let root_rowid = self.nodes[self.root].i_rowid;
        self.ap_expr_phrase
            .iter()
            .map(|&k| {
                let node = &self.nodes[self.phrases[k].p_node];
                self.phrases[k].n_poslist == 0 || node.i_rowid != root_rowid || node.b_eof
            })
            .collect()
    }

    /// Puts the expression on a row saved from a live cursor: every node is on `i_rowid`
    /// and the phrases that were not on the row then stay empty.
    pub(super) fn set_replay_row(
        &mut self,
        i_rowid: i64,
        miss: &[bool],
        a_populator: &mut [PoslistPopulator],
    ) {
        for n in &mut self.nodes {
            n.i_rowid = i_rowid;
            n.b_eof = false;
        }
        for (p, &m) in a_populator.iter_mut().zip(miss) {
            p.b_miss = m;
        }
    }

    /// `sqlite3Fts5ExprPopulatePoslists`: tokenizes the text of column `i_col` of the
    /// current row and appends the positions of the query terms to the phrase poslists.
    pub(super) fn populate_poslists(
        &mut self,
        cfg: &Fts5Config,
        ix: &Fts5Index,
        a_populator: &mut [PoslistPopulator],
        i_col: i32,
        text: &[u8],
    ) {
        let mut i_off: i64 = (i64::from(i_col) << 32) - 1;
        for (i, &k) in self.ap_expr_phrase.iter().enumerate() {
            let colset = self.nodes[self.phrases[k].p_node]
                .near
                .as_ref()
                .and_then(|n| n.colset.as_ref());
            a_populator[i].b_ok =
                !(colset.is_some_and(|c| !c.contains(&i_col)) || a_populator[i].b_miss);
        }
        let i_rowid = self.nodes[self.root].i_rowid;
        let b_tokendata = self.b_tokendata;
        let ap = self.ap_expr_phrase.clone();
        let phrases = &mut self.phrases;
        // fts5ExprPopulatePoslistsCb
        cfg.tokenizer.tok.tokenize(text, &mut |tok, _, _| {
            let n_token = tok.len();
            let mut n_query = n_token.min(FTS5_MAX_TOKEN_SIZE);
            if b_tokendata {
                // fts5QueryTerm
                n_query = tok[..n_query].iter().position(|&b| b == 0).unwrap_or(n_query);
            }
            i_off += 1;
            for (i, &k) in ap.iter().enumerate() {
                if !a_populator[i].b_ok {
                    continue;
                }
                let ph = &mut phrases[k];
                let Some(t0) = ph.a_term.first_mut() else {
                    continue;
                };
                for s in 0..t0.syn_count() {
                    let p_t = t0.syn_get_mut(s);
                    let nq = p_t.n_query_term;
                    if (nq == n_query || (nq < n_query && p_t.b_prefix))
                        && p_t.term[..nq] == tok[..nq.min(tok.len())]
                    {
                        if b_tokendata || p_t.b_prefix {
                            let i_c = (i_off >> 32) as i32;
                            let i_tok_off = (i_off & 0x7FFF_FFFF) as i32;
                            if let Some(it) = p_t.iter.as_mut() {
                                ix.iter_write_tokendata(it, tok, i_rowid, i_c, i_tok_off);
                            }
                        }
                        ph.alias = false;
                        ph.poslist.truncate(ph.n_poslist.max(0) as usize);
                        poslist_safe_append(&mut ph.poslist, &mut a_populator[i].i_prev, i_off);
                        ph.n_poslist = ph.poslist.len() as i32;
                        break;
                    }
                }
            }
        });
    }

    /// `fts5ExprClearPoslists`.
    fn expr_clear_poslists(&mut self, i_node: usize) {
        if matches!(self.nodes[i_node].e_type, NodeType::Term | NodeType::String) {
            if let Some(&k) = self.nodes[i_node].near.as_ref().and_then(|n| n.ap_phrase.first())
            {
                self.phrases[k].n_poslist = 0;
            }
        } else {
            for c in self.nodes[i_node].children.clone() {
                self.expr_clear_poslists(c);
            }
        }
    }

    /// `fts5ExprCheckPoslists`.
    fn expr_check_poslists(&mut self, i_node: usize, i_rowid: i64) -> bool {
        self.nodes[i_node].i_rowid = i_rowid;
        self.nodes[i_node].b_eof = false;
        match self.nodes[i_node].e_type {
            NodeType::Eof | NodeType::Term | NodeType::String => self.nodes[i_node]
                .near
                .as_ref()
                .and_then(|n| n.ap_phrase.first())
                .is_some_and(|&k| self.phrases[k].n_poslist > 0),
            NodeType::And => {
                for c in self.nodes[i_node].children.clone() {
                    if !self.expr_check_poslists(c, i_rowid) {
                        self.expr_clear_poslists(i_node);
                        return false;
                    }
                }
                true
            }
            NodeType::Or => {
                let mut b_ret = false;
                for c in self.nodes[i_node].children.clone() {
                    if self.expr_check_poslists(c, i_rowid) {
                        b_ret = true;
                    }
                }
                b_ret
            }
            NodeType::Not => {
                let c0 = self.nodes[i_node].children[0];
                let c1 = self.nodes[i_node].children[1];
                if !self.expr_check_poslists(c0, i_rowid) || self.expr_check_poslists(c1, i_rowid) {
                    self.expr_clear_poslists(i_node);
                    return false;
                }
                true
            }
        }
    }

    /// `sqlite3Fts5ExprCheckPoslists`.
    pub(super) fn check_poslists(&mut self, i_rowid: i64) {
        let root = self.root;
        self.expr_check_poslists(root, i_rowid);
    }

    /// `sqlite3Fts5ExprPhraseCollist` (detail=columns).
    pub(super) fn phrase_collist(&self, i_phrase: usize) -> std::borrow::Cow<'_, [u8]> {
        let k = self.ap_expr_phrase[i_phrase];
        let ph = &self.phrases[k];
        let node = &self.nodes[ph.p_node];
        if !node.b_eof && node.i_rowid == self.nodes[self.root].i_rowid && ph.n_poslist > 0 {
            let t = &ph.a_term[0];
            if !t.syn.is_empty() {
                std::borrow::Cow::Owned(expr_synonym_list(t, node.i_rowid))
            } else {
                std::borrow::Cow::Borrowed(t.iter_data())
            }
        } else {
            std::borrow::Cow::Borrowed(&[])
        }
    }

    /// `sqlite3Fts5ExprQueryToken` (xQueryToken: no built-in auxiliary function calls it).
    #[allow(dead_code)]
    pub(super) fn query_token(&self, i_phrase: i32, i_token: i32) -> Option<&[u8]> {
        if i_phrase < 0 || i_phrase as usize >= self.ap_expr_phrase.len() {
            return None;
        }
        let ph = &self.phrases[self.ap_expr_phrase[i_phrase as usize]];
        if i_token < 0 || i_token as usize >= ph.a_term.len() {
            return None;
        }
        Some(&ph.a_term[i_token as usize].term)
    }

    /// `sqlite3Fts5ExprInstToken`: `Err(Range)` for a bad phrase / token number (xInstToken:
    /// no built-in auxiliary function calls it).
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn inst_token(
        &mut self,
        ix: &mut Fts5Index,
        ctx: &mut dyn VTabHostContext,
        i_rowid: i64,
        i_phrase: i32,
        i_col: i32,
        i_off: i32,
        i_token: i32,
    ) -> Result<Option<Vec<u8>>> {
        let range = || SqliteError::new(ErrorCode::Range, "column index out of range");
        if i_phrase < 0 || i_phrase as usize >= self.ap_expr_phrase.len() {
            return Err(range());
        }
        let b_tokendata = self.b_tokendata;
        let ph = &mut self.phrases[self.ap_expr_phrase[i_phrase as usize]];
        if i_token < 0 || i_token as usize >= ph.a_term.len() {
            return Err(range());
        }
        let term = &mut ph.a_term[i_token as usize];
        if b_tokendata || term.b_prefix {
            let tok = term.term[..term.n_query_term].to_vec();
            match term.iter.as_mut() {
                Some(it) => ix.iter_token(ctx, it, &tok, i_rowid, i_col, i_off + i_token),
                None => Ok(None),
            }
        } else {
            Ok(Some(term.term.clone()))
        }
    }

    /// `sqlite3Fts5ExprClearTokens`.
    pub(super) fn clear_tokens(&mut self, ix: &Fts5Index) {
        for &k in &self.ap_expr_phrase {
            if let Some(t0) = self.phrases[k].a_term.first_mut() {
                for s in 0..t0.syn_count() {
                    if let Some(it) = t0.syn_get_mut(s).iter.as_mut() {
                        ix.iter_clear_tokendata(it);
                    }
                }
            }
        }
    }

    /// The column set of the nearset phrase `i_phrase` belongs to.
    #[allow(dead_code)]
    pub(super) fn phrase_colset(&self, i_phrase: usize) -> Option<&[i32]> {
        let k = self.ap_expr_phrase[i_phrase];
        self.nodes[self.phrases[k].p_node]
            .near
            .as_ref()
            .and_then(|n| n.colset.as_deref())
    }
}
